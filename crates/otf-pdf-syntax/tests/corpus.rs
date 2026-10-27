//! Corpus-wide tests (docs/PLAN.md §7).
//!
//! The hand-made files in `corpus/malformed` are committed, so their results
//! are asserted exactly. The external corpus (`scripts/fetch-corpus.sh`) is
//! optional: when it is absent its checks are skipped. Where it is present,
//! every file must be processed without a panic, and failures must match the
//! known-failure baseline in `corpus/baseline`, which may only shrink. Set
//! `OTF_PDF_UPDATE_BASELINE=1` to rewrite a baseline after fixing failures.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use otf_pdf_syntax::{Header, HeaderError, Version, find_header};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

fn read(name: &str) -> Vec<u8> {
    fs::read(corpus().join("malformed").join(name)).unwrap()
}

#[test]
fn malformed_set_headers() {
    let header = |offset, major, minor| {
        Ok(Header {
            offset,
            version: Version::new(major, minor),
        })
    };
    let expected: &[(&str, Result<Header, HeaderError>)] = &[
        ("minimal-valid.pdf", header(0, 1, 7)),
        ("empty.pdf", Err(HeaderError::NotFound)),
        ("not-a-pdf.pdf", Err(HeaderError::NotFound)),
        (
            "malformed-version.pdf",
            Err(HeaderError::MalformedVersion { offset: 0 }),
        ),
        ("unknown-version.pdf", header(0, 1, 9)),
        ("junk-before-header.pdf", header(39, 1, 4)),
        ("truncated.pdf", header(0, 1, 7)),
        ("wrong-xref-offsets.pdf", header(0, 1, 7)),
        ("missing-xref.pdf", header(0, 1, 7)),
        ("wrong-stream-length.pdf", header(0, 1, 7)),
        ("missing-length-object.pdf", header(0, 1, 7)),
        ("missing-endobj.pdf", header(0, 1, 7)),
        ("cyclic-refs.pdf", header(0, 1, 7)),
        ("deep-nesting.pdf", header(0, 1, 7)),
        ("flate-bomb.pdf", header(0, 1, 7)),
        ("huge-xref-count.pdf", header(0, 1, 7)),
        ("startxref-past-eof.pdf", header(0, 1, 7)),
    ];
    for (name, want) in expected {
        assert_eq!(&find_header(&read(name)), want, "{name}");
    }

    // Every committed file is covered above, so a new one cannot go untested.
    let listed: BTreeSet<&str> = expected.iter().map(|(n, _)| *n).collect();
    let on_disk: BTreeSet<String> = fs::read_dir(corpus().join("malformed"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".pdf"))
        .collect();
    let on_disk: BTreeSet<&str> = on_disk.iter().map(String::as_str).collect();
    assert_eq!(listed, on_disk);
}

/// Every `.pdf` under `dir`, recursively, case-insensitively, sorted.
fn pdfs(dir: &Path) -> Vec<PathBuf> {
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
fn key(path: &Path) -> String {
    let rel = path.strip_prefix(corpus()).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Passwords for encrypted corpus files, by baseline key, from pdf.js's
/// `test_manifest.json` and `corpus/passwords.tsv`.
fn passwords() -> std::collections::HashMap<String, String> {
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
fn top_level_objects(json: &str) -> Vec<&str> {
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
fn json_field(object: &str, key: &str) -> Option<String> {
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
fn against_baseline(name: &str, check: impl Fn(&str, &[u8]) -> bool) {
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

#[test]
fn external_corpus_headers() {
    against_baseline("header", |_, bytes| find_header(bytes).is_ok());
}

#[test]
fn malformed_set_opens_or_fails_cleanly() {
    use otf_pdf_syntax::{File, OpenOptions};
    use std::sync::Arc;
    for entry in fs::read_dir(corpus().join("malformed")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "pdf") {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        // Either outcome is fine; panicking or hanging is not.
        if let Ok(f) = File::open(Arc::new(bytes), OpenOptions::default()) {
            for &num in f.entries().keys() {
                let _ = f.get(otf_pdf_syntax::ObjId::new(num, 0));
            }
        }
    }
}

#[test]
fn external_corpus_opens() {
    use otf_pdf_syntax::{File, OpenOptions};
    use std::sync::Arc;
    let passwords = passwords();
    assert!(
        passwords.is_empty() || passwords.len() >= 5,
        "pdf.js manifest passwords not found"
    );
    against_baseline("open", |key, bytes| {
        let options = OpenOptions {
            password: passwords
                .get(key)
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default(),
            ..OpenOptions::default()
        };
        File::open(Arc::new(bytes.to_vec()), options).is_ok_and(|f| {
            f.resolve_dict(
                f.trailer()
                    .get(b"Root")
                    .unwrap_or(&otf_pdf_syntax::Object::Null),
            )
            .is_some()
        })
    });
}

#[test]
fn manifest_json_reader() {
    let json = r#"[{"id": "a", "file": "pdfs/a.pdf", "password": "pæ😀\"q"}, {"file": "pdfs/b.pdf", "x": {"y": "}"}}]"#;
    let objects = top_level_objects(json);
    assert_eq!(objects.len(), 2);
    assert_eq!(
        json_field(objects[0], "file").as_deref(),
        Some("pdfs/a.pdf")
    );
    assert_eq!(
        json_field(objects[0], "password").as_deref(),
        Some("pæ😀\"q")
    );
    assert_eq!(json_field(objects[1], "password"), None);
}
