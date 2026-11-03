//! The content writer against real files: every page's content stream,
//! parsed, written and parsed again, gives the same operations.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::path::Path;

use otf_pdf_content::{Operation, page_content, parse_content, write_content};
use otf_pdf_doc::Document;
use otf_pdf_syntax::{Object, OpenOptions};

#[path = "../../otf-pdf-syntax/tests/support/mod.rs"]
mod support;

/// Equal, allowing for the writer's six-decimal reals (`2.0` written as
/// `2`) and its choice of literal or hex spelling for strings.
fn same(a: &Object, b: &Object) -> bool {
    match (a, b) {
        (Object::String(x), Object::String(y)) => x.bytes == y.bytes,
        (Object::Real(_) | Object::Int(_), Object::Real(_) | Object::Int(_)) => {
            let (x, y) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            (x - y).abs() <= 1e-6 * x.abs().max(1.0) || (!x.is_finite() && y == 0.0)
        }
        (Object::Array(x), Object::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Object::Dict(x), Object::Dict(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k.as_bytes()).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn same_ops(a: &[Operation], b: &[Operation]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.operator == y.operator
                && x.operands.len() == y.operands.len()
                && x.operands.iter().zip(&y.operands).all(|(p, q)| same(p, q))
        })
}

/// Whether every page of `bytes` that has content round-trips. Files that
/// do not open, and pages whose content does not parse, are not the
/// writer's concern and pass.
fn round_trips(bytes: &[u8], password: &[u8]) -> bool {
    let options = OpenOptions {
        password: password.to_vec(),
        ..OpenOptions::default()
    };
    let Ok(doc) = Document::open(bytes.to_vec(), options) else {
        return true;
    };
    let Ok(pages) = doc.pages() else {
        return true;
    };
    let limits = doc.store().limits();
    pages.iter().take(50).all(|page| {
        let Ok(content) = page_content(doc.store(), page) else {
            return true;
        };
        let Ok(ops) = parse_content(&content, limits) else {
            return true;
        };
        let Ok(written) = write_content(&ops) else {
            return false;
        };
        parse_content(&written, limits).is_ok_and(|again| same_ops(&ops, &again))
    })
}

#[test]
fn fixtures_round_trip() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut checked = 0;
    for dir in ["text", "malformed"] {
        for entry in std::fs::read_dir(corpus.join(dir)).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "pdf") {
                let bytes = std::fs::read(&path).unwrap();
                assert!(round_trips(&bytes, b""), "{}", path.display());
                checked += 1;
            }
        }
    }
    assert!(checked > 5);
}

/// Every external file; failures are baselined. The one known failure is
/// a fuzzed file whose content has a stray `)` the parser keeps as an
/// operator, which the writer refuses.
#[test]
fn external_corpus_round_trips() {
    let passwords = support::passwords();
    support::against_baseline("content", |key, bytes| {
        round_trips(
            bytes,
            passwords.get(key).map(|p| p.as_bytes()).unwrap_or_default(),
        )
    });
}
