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

/// Runs `check` over every fetched external file and compares the failures
/// with `corpus/baseline/<name>.txt`.
fn against_baseline(name: &str, check: impl Fn(&[u8]) -> bool) {
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
            if !check(&bytes) {
                failures.insert(key(&file));
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
    against_baseline("header", |bytes| find_header(bytes).is_ok());
}
