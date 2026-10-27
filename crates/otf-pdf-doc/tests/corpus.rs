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

/// Every page's decoded content, or `None` if any of it cannot be decoded.
fn page_contents(doc: &Document) -> Option<Vec<Vec<u8>>> {
    use otf_pdf_syntax::Object;
    let store = doc.store();
    doc.pages()
        .ok()?
        .iter()
        .map(|p| {
            let contents = store.resolve(p.dict.get(b"Contents").unwrap_or(&Object::Null));
            let parts: Vec<Object> = match contents {
                Object::Array(items) => items.iter().map(|i| store.resolve(i)).collect(),
                other => vec![other],
            };
            let mut bytes = Vec::new();
            for part in parts {
                if let Object::Stream(s) = part {
                    bytes.extend(store.decode_stream(&s).ok()?.data);
                }
            }
            Some(bytes)
        })
        .collect()
}

/// Phase 2 acceptance: open → full save → open keeps the page count and
/// every page's content bytes; an incremental save (where allowed) keeps the
/// original bytes as an exact prefix.
#[test]
fn external_corpus_round_trip() {
    use otf_pdf_doc::{SaveMode, SaveOptions};
    let passwords = passwords();
    against_baseline("roundtrip", |key, bytes| {
        let open = |b: Vec<u8>| {
            Document::open(
                b,
                OpenOptions {
                    password: passwords
                        .get(key)
                        .map(|p| p.as_bytes().to_vec())
                        .unwrap_or_default(),
                    ..OpenOptions::default()
                },
            )
        };
        let Ok(doc) = open(bytes.to_vec()) else {
            return false;
        };
        let before = page_contents(&doc);
        let Ok(saved) = doc.save(&SaveOptions::default()) else {
            return false;
        };
        let Ok(again) = Document::open(saved, OpenOptions::default()) else {
            return false;
        };
        // Content that cannot be decoded (corrupt streams) is compared by
        // page count; the raw bytes are carried over unchanged either way.
        let same = match &before {
            Some(b) => page_contents(&again).as_ref() == Some(b),
            None => again.page_count().ok() == doc.page_count().ok(),
        };
        if !same {
            return false;
        }
        let legacy = doc.file().security().is_some_and(|s| !s.can_encrypt());
        if !doc.file().was_rebuilt() && !legacy {
            let options = SaveOptions {
                mode: SaveMode::Incremental,
                ..SaveOptions::default()
            };
            match doc.save(&options) {
                Ok(out) if out.starts_with(bytes) => {}
                _ => return false,
            }
        }
        true
    });
}
