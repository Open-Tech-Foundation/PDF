//! Open arbitrary bytes, edit, and save in full and incrementally. The
//! properties: nothing panics, and whatever we write opens again with the
//! same page count and without needing any repair.
#![no_main]

use libfuzzer_sys::fuzz_target;
use otf_pdf_doc::{Document, SaveMode, SaveOptions};
use otf_pdf_syntax::{Limits, OpenOptions};

fuzz_target!(|data: &[u8]| {
    let options = || OpenOptions {
        limits: Limits {
            max_decoded_size: 1 << 22,
            max_object_size: 1 << 20,
            max_pages: 1000,
            ..Limits::default()
        },
        ..OpenOptions::default()
    };
    let Ok(mut doc) = Document::open(data.to_vec(), options()) else {
        return;
    };
    let Ok(count) = doc.page_count() else { return };
    if count > 1 {
        let _ = doc.rotate_page(0, 90);
        let _ = doc.delete_pages(&[count - 1]);
    }
    let expected = doc.page_count().unwrap_or(0);
    for mode in [SaveMode::Full, SaveMode::Incremental] {
        let save = SaveOptions {
            mode,
            ..SaveOptions::default()
        };
        let Ok(bytes) = doc.save(&save) else { continue };
        let again = Document::open(bytes, options()).expect("our output must open");
        if mode == SaveMode::Full {
            assert_eq!(again.page_count().ok(), Some(expected));
            assert!(again.diagnostics().is_empty(), "{:?}", again.diagnostics());
        }
    }
});
