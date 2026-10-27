//! Phase 1 acceptance over the external corpus: every file opens and its
//! pages are enumerated, and the page count matches poppler's
//! (`corpus/reference/pages.tsv`) wherever poppler has one. Failures are
//! tracked in `corpus/baseline/pages.txt`, which may only shrink.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::fs;

use otf_pdf_doc::Document;
use otf_pdf_syntax::OpenOptions;

#[path = "../../otf-pdf-syntax/tests/support/mod.rs"]
mod support;
use support::{against_baseline, corpus, passwords};

fn reference_counts() -> HashMap<String, usize> {
    fs::read_to_string(corpus().join("reference/pages.tsv"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let (key, count) = l.split_once('\t')?;
            Some((key.to_owned(), count.parse().ok()?))
        })
        .collect()
}

#[test]
fn external_corpus_pages() {
    let passwords = passwords();
    let reference = reference_counts();
    against_baseline("pages", |key, bytes| {
        let options = OpenOptions {
            password: passwords
                .get(key)
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default(),
            ..OpenOptions::default()
        };
        let Ok(doc) = Document::open(bytes.to_vec(), options) else {
            return false;
        };
        let Ok(count) = doc.page_count() else {
            return false;
        };
        match reference.get(key) {
            Some(&expected) => count == expected,
            None => count > 0,
        }
    });
}
