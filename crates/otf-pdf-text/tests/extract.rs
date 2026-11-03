//! Extraction through the public API on small built files: limits surface as
//! errors, and a shared font cache carries across extractors.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use otf_pdf_doc::Document;
use otf_pdf_font::FontCache;
use otf_pdf_syntax::{Error, LimitKind, Limits, OpenOptions};
use otf_pdf_text::TextExtractor;

/// Two pages drawing "Hello" in Helvetica (object 5, shared). Page 1's
/// content is unfiltered; page 2's is ASCIIHex-encoded, so only it counts
/// against `max_decoded_size`.
fn two_pages() -> Vec<u8> {
    let content = "BT /F1 12 Tf 10 10 Td (Hello) Tj ET";
    let hex: String = content
        .bytes()
        .map(|b| format!("{b:02X}"))
        .chain([">".to_owned()])
        .collect();
    let page = |n: u32, contents: u32| {
        format!(
            "{n} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 50] \
             /Contents {contents} 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj\n"
        )
    };
    let mut out = String::from(
        "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
         2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n",
    );
    out.push_str(&page(3, 6));
    out.push_str(&page(4, 7));
    out.push_str("5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n");
    out.push_str(&format!(
        "6 0 obj << /Length {} >> stream\n{content}\nendstream endobj\n",
        content.len()
    ));
    out.push_str(&format!(
        "7 0 obj << /Filter /ASCIIHexDecode /Length {} >> stream\n{hex}\nendstream endobj\n",
        hex.len()
    ));
    out.into_bytes()
}

fn open(max_decoded_size: u64) -> Document {
    let options = OpenOptions {
        limits: Limits {
            max_decoded_size,
            ..Limits::default()
        },
        ..OpenOptions::default()
    };
    Document::open(two_pages(), options).unwrap()
}

#[test]
fn both_pages_read_within_limits() {
    let doc = open(1 << 20);
    let ex = TextExtractor::new(&doc);
    assert_eq!(ex.page_text(0).unwrap(), "Hello\n");
    assert_eq!(ex.page_text(1).unwrap(), "Hello\n");
    assert_eq!(ex.text().unwrap(), "Hello\n\x0cHello\n\x0c");
}

#[test]
fn page_over_a_limit_is_an_error_not_empty_text() {
    let doc = open(8);
    let ex = TextExtractor::new(&doc);
    // The unfiltered page is unaffected.
    assert_eq!(ex.page_text(0).unwrap(), "Hello\n");
    assert_eq!(ex.page_text(1), Err(Error::Limit(LimitKind::DecodedSize)));
    assert_eq!(ex.page(1), Err(Error::Limit(LimitKind::DecodedSize)));
    // The whole-document text stops too, rather than dropping the page.
    assert_eq!(ex.text(), Err(Error::Limit(LimitKind::DecodedSize)));
}

#[test]
fn font_cache_is_shared_across_extractors() {
    let doc = open(1 << 20);
    let fonts = FontCache::new();
    // Separate calls, as a host answering one page per request makes them.
    let first = TextExtractor::with_fonts(&doc, fonts.clone())
        .page_text(0)
        .unwrap();
    assert_eq!(fonts.len(), 1);
    let ex = TextExtractor::with_fonts(&doc, fonts.clone());
    assert_eq!(ex.page_text(1).unwrap(), first);
    // Page 2 uses the same font object: nothing new was loaded.
    assert_eq!(fonts.len(), 1);
    assert_eq!(ex.fonts().len(), 1);
    // `new` keeps its own cache, separate from the shared one.
    let own = TextExtractor::new(&doc);
    assert!(own.fonts().is_empty());
    own.page_text(0).unwrap();
    assert_eq!(own.fonts().len(), 1);
}
