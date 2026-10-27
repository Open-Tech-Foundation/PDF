//! Tokenize arbitrary bytes as a content stream and parse them as a CMap.
//! The property is that nothing panics.
#![no_main]

use libfuzzer_sys::fuzz_target;
use otf_pdf_content::parse_content;
use otf_pdf_font::CMap;
use otf_pdf_syntax::Limits;

fuzz_target!(|data: &[u8]| {
    let _ = parse_content(data, &Limits::default());
    let cmap = CMap::parse(data);
    for (code, _) in cmap.codes(data).into_iter().take(256) {
        let _ = cmap.unicode(code);
        let _ = cmap.cid(code);
    }
});
