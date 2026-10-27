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

use otf_pdf_syntax::{Header, HeaderError, Version, find_header};

mod support;
use support::{against_baseline, corpus, json_field, passwords, top_level_objects};

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
