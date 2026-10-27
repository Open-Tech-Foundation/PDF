//! Phase 3 acceptance: extracted text against references, by character error
//! rate (edit distance / reference length, whitespace ignored).
//!
//! * `corpus/text`: Indic and CJK fixtures printed by Chromium and
//!   LibreOffice, measured against their source text. Always present.
//! * `corpus/reference/text`: a labelled subset of the external corpus,
//!   measured against poppler's `pdftotext -raw` (first 5 pages). Skipped when
//!   the external corpus is not fetched.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use otf_pdf_doc::Document;
use otf_pdf_syntax::OpenOptions;
use otf_pdf_text::TextExtractor;

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

/// Characters without whitespace (layout differences are not text errors).
fn normalize(s: &str) -> Vec<char> {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Levenshtein distance, two rows.
fn distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Our text of the first `pages` pages, one string per page.
fn extract(path: &Path, pages: usize) -> Option<Vec<String>> {
    let doc = Document::open(std::fs::read(path).ok()?, OpenOptions::default()).ok()?;
    let ex = TextExtractor::new(&doc);
    let count = doc.page_count().ok()?.min(pages);
    Some(
        (0..count)
            .map(|i| ex.page_text(i).unwrap_or_default())
            .collect(),
    )
}

/// Accumulated (distance, reference length) per group.
#[derive(Default)]
struct Score(BTreeMap<String, (usize, usize)>);

impl Score {
    fn add(&mut self, group: &str, name: &str, ours: &[String], reference: &[&str]) {
        let mut dist = 0;
        let mut len = 0;
        for (i, r) in reference.iter().enumerate() {
            let r = normalize(r);
            let o = normalize(ours.get(i).map_or("", String::as_str));
            dist += distance(&o, &r);
            len += r.len();
        }
        if len == 0 {
            return;
        }
        let rate = dist as f64 / len as f64;
        if rate > 0.02 {
            eprintln!("  {group:6} CER {:6.2}%  {name}", rate * 100.0);
        }
        let e = self.0.entry(group.to_owned()).or_default();
        e.0 += dist;
        e.1 += len;
    }

    fn rates(&self) -> BTreeMap<String, f64> {
        self.0
            .iter()
            .map(|(g, (d, l))| (g.clone(), *d as f64 / (*l).max(1) as f64))
            .collect()
    }
}

/// Character error rate allowed per script group (DECISIONS.md D27).
fn threshold(group: &str) -> f64 {
    match group {
        "latin" => 0.02,
        "cjk" => 0.02,
        _ => 0.03,
    }
}

fn manifest(dir: &Path) -> Vec<(String, String, String)> {
    std::fs::read_to_string(dir.join("manifest.tsv"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[0].to_owned(), f[1].to_owned(), f[2].to_owned())
        })
        .collect()
}

#[test]
fn indic_and_cjk_fixtures_match_their_source_text() {
    let dir = corpus().join("text");
    let mut score = Score::default();
    let entries = manifest(&dir);
    assert_eq!(entries.len(), 14);
    for (file, group, source) in entries {
        let reference = std::fs::read_to_string(dir.join(&source)).unwrap();
        let ours = extract(&dir.join(&file), 10).unwrap().join("");
        score.add(&group, &file, &[ours], &[&reference]);
    }
    for (group, rate) in score.rates() {
        eprintln!("fixtures {group}: CER {:.3}%", rate * 100.0);
        assert!(rate <= threshold(&group), "{group} CER {rate}");
    }
}

#[test]
fn external_subset_matches_poppler() {
    let dir = corpus().join("reference/text");
    let entries = manifest(&dir);
    if !corpus().join("external").exists() || entries.is_empty() {
        eprintln!("external corpus not fetched; run scripts/fetch-corpus.sh");
        return;
    }
    let mut score = Score::default();
    for (file, group, reference) in entries {
        let reference = std::fs::read_to_string(dir.join(&reference)).unwrap();
        let pages: Vec<&str> = reference
            .split('\x0c')
            .filter(|p| !p.trim().is_empty() || true)
            .collect();
        let ours = extract(&corpus().join(&file), 5).unwrap_or_default();
        let pages = &pages[..pages.len().min(5)];
        score.add(&group, &file, &ours, pages);
    }
    for (group, rate) in score.rates() {
        eprintln!("external {group}: CER {:.3}%", rate * 100.0);
        assert!(rate <= threshold(&group), "{group} CER {rate}");
    }
}

#[test]
fn edit_distance() {
    let d = |a: &str, b: &str| distance(&normalize(a), &normalize(b));
    assert_eq!(d("kitten", "sitting"), 3);
    assert_eq!(d("a b c", "abc"), 0);
    assert_eq!(d("", "abc"), 3);
    assert_eq!(d("日本語", "日本"), 1);
}

#[path = "../../otf-pdf-syntax/tests/support/mod.rs"]
mod support;

/// Every external file that opens: text extraction runs on every page
/// without panicking, and failures (an error from a page) are baselined.
#[test]
fn external_corpus_extracts() {
    let passwords = support::passwords();
    support::against_baseline("text", |key, bytes| {
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
        let ex = TextExtractor::new(&doc);
        (0..count.min(50)).all(|i| ex.page(i).is_ok())
    });
}
