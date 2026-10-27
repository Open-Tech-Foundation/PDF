//! Shared helpers for corpus tests, included by each crate's corpus test
//! with `#[path]` (integration tests cannot depend on each other).
#![allow(dead_code, clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The repository's `corpus/` directory.
pub fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

/// Every `.pdf` under `dir`, recursively, case-insensitively, sorted.
pub fn pdfs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A corpus path as it appears in a baseline: relative, `/`-separated.
pub fn key(path: &Path) -> String {
    let rel = path.strip_prefix(corpus()).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Passwords for encrypted corpus files, by baseline key, from pdf.js's
/// `test_manifest.json` and `corpus/passwords.tsv`.
pub fn passwords() -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let listed = fs::read_to_string(corpus().join("passwords.tsv")).unwrap_or_default();
    for line in listed.lines().filter(|l| !l.starts_with('#')) {
        if let Some((key, password)) = line.split_once('\t') {
            out.insert(key.to_owned(), password.to_owned());
        }
    }
    let path = corpus().join("external/pdfjs/test/test_manifest.json");
    let Ok(json) = fs::read_to_string(path) else {
        return out;
    };
    for object in top_level_objects(&json) {
        if let (Some(file), Some(password)) =
            (json_field(object, "file"), json_field(object, "password"))
        {
            out.insert(format!("external/pdfjs/test/{file}"), password);
        }
    }
    out
}

/// The `{...}` objects directly inside the top-level array.
pub fn top_level_objects(json: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut in_string, mut escaped) = (0, 0, false, false);
    for (i, c) in json.char_indices() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    out.push(&json[start..=i]);
                }
            }
            _ => {}
        }
    }
    out
}

/// The string value of `"key": "..."` in a flat JSON object.
pub fn json_field(object: &str, key: &str) -> Option<String> {
    let at = object.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = object[at..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    let mut pending_high: Option<u32> = None;
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let unit = u32::from_str_radix(&hex, 16).ok()?;
                    if (0xd800..0xdc00).contains(&unit) {
                        pending_high = Some(unit);
                        continue;
                    }
                    let code = match pending_high.take() {
                        Some(high) => 0x10000 + ((high - 0xd800) << 10) + (unit - 0xdc00),
                        None => unit,
                    };
                    out.push(char::from_u32(code)?);
                }
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

/// Runs `check` over every fetched external file and compares the failures
/// with `corpus/baseline/<name>.txt`. `check` gets the file's baseline key
/// and its bytes.
pub fn against_baseline(name: &str, check: impl Fn(&str, &[u8]) -> bool) {
    let external = corpus().join("external");
    let sources: Vec<PathBuf> = fs::read_dir(&external)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    if sources.is_empty() {
        eprintln!("external corpus not fetched; run scripts/fetch-corpus.sh");
        return;
    }
    let fetched: Vec<String> = sources.iter().map(|s| key(s) + "/").collect();

    let mut total = 0;
    let mut failures = BTreeSet::new();
    for source in &sources {
        for file in pdfs(source) {
            total += 1;
            let bytes = fs::read(&file).unwrap();
            let file_key = key(&file);
            if !check(&file_key, &bytes) {
                failures.insert(file_key);
            }
        }
    }

    let baseline_path = corpus().join("baseline").join(format!("{name}.txt"));
    let baseline: BTreeSet<String> = fs::read_to_string(&baseline_path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        // Only judge entries whose source was fetched this run.
        .filter(|l| fetched.iter().any(|f| l.starts_with(f.as_str())))
        .map(str::to_owned)
        .collect();

    eprintln!(
        "{name}: {total} files, {} failures, {} in baseline",
        failures.len(),
        baseline.len()
    );
    if std::env::var_os("OTF_PDF_UPDATE_BASELINE").is_some() {
        let mut text = format!(
            "# Known {name} failures in the external corpus. May only shrink.\n\
             # Regenerate: OTF_PDF_UPDATE_BASELINE=1 cargo test --test corpus\n"
        );
        for f in &failures {
            text.push_str(f);
            text.push('\n');
        }
        fs::create_dir_all(baseline_path.parent().unwrap()).unwrap();
        fs::write(&baseline_path, text).unwrap();
        return;
    }
    let new: Vec<_> = failures.difference(&baseline).collect();
    let fixed: Vec<_> = baseline.difference(&failures).collect();
    assert!(new.is_empty(), "new {name} failures: {new:#?}");
    assert!(
        fixed.is_empty(),
        "{name} baseline entries now pass; shrink {}: {fixed:#?}",
        baseline_path.display()
    );
}
