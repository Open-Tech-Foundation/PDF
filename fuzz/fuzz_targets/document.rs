//! Open arbitrary bytes as a document and read everything Phase 1 offers:
//! xref, every object, pages, metadata, outlines, attachments. The property
//! is that nothing panics and limits bound the work.
#![no_main]

use libfuzzer_sys::fuzz_target;
use otf_pdf_doc::Document;
use otf_pdf_syntax::{Limits, ObjId, OpenOptions};

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions {
        limits: Limits {
            max_decoded_size: 1 << 22,
            max_object_size: 1 << 20,
            max_pages: 10_000,
            ..Limits::default()
        },
        ..OpenOptions::default()
    };
    let Ok(doc) = Document::open(data.to_vec(), options) else {
        return;
    };
    let _ = doc.pages();
    let _ = doc.info();
    let _ = doc.xmp_metadata();
    let _ = doc.outlines();
    if let Ok(attachments) = doc.attachments() {
        for a in &attachments {
            let _ = doc.attachment_data(a);
        }
    }
    let file = doc.file();
    for &num in file.entries().keys().take(1000) {
        if let Ok(obj) = file.get(ObjId::new(num, 0)) {
            if let Some(stream) = obj.as_stream() {
                let _ = file.decode_stream(stream);
            }
        }
    }
});
