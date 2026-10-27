//! Open arbitrary bytes and extract the text of every page: interpreter,
//! fonts, CMaps and layout. The property is that nothing panics.
#![no_main]

use libfuzzer_sys::fuzz_target;
use otf_pdf_doc::Document;
use otf_pdf_syntax::{Limits, OpenOptions};
use otf_pdf_text::TextExtractor;

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions {
        limits: Limits {
            max_decoded_size: 1 << 22,
            max_object_size: 1 << 20,
            max_pages: 100,
            ..Limits::default()
        },
        ..OpenOptions::default()
    };
    let Ok(doc) = Document::open(data.to_vec(), options) else {
        return;
    };
    let ex = TextExtractor::new(&doc);
    for i in 0..doc.page_count().unwrap_or(0).min(20) {
        if let Ok(page) = ex.page(i) {
            let _ = page.words();
        }
    }
});
