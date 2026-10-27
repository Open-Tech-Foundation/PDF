//! Find the header in arbitrary bytes. The property is that nothing panics and
//! a found header lies inside the search window and the input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use otf_pdf_syntax::{HEADER_SEARCH_WINDOW, find_header};

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = find_header(data) {
        assert!(header.offset < HEADER_SEARCH_WINDOW);
        assert!(header.offset < data.len() as u64);
        let at = &data[header.offset as usize..];
        assert!(at.starts_with(b"%PDF-"));
    }
});
