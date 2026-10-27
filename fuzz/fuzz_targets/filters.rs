//! Decode arbitrary bytes through every filter, with and without a PNG
//! predictor. The property is that nothing panics and the output limit holds.
#![no_main]

use libfuzzer_sys::fuzz_target;
use otf_pdf_syntax::filters::{Filter, decode};
use otf_pdf_syntax::{Dict, Limits, Name, Strictness};

const LIMIT: u64 = 1 << 22;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, input)) = data.split_first() else {
        return;
    };
    let names = [
        "FlateDecode",
        "LZWDecode",
        "ASCII85Decode",
        "ASCIIHexDecode",
        "RunLengthDecode",
    ];
    let name = names[usize::from(selector) % names.len()];
    let params = (selector & 0x80 != 0).then(|| {
        let mut p = Dict::new();
        p.insert("Predictor", i64::from(10 + (selector >> 4 & 0x7) % 6));
        p.insert("Columns", i64::from(selector & 0xf) + 1);
        p.insert("Colors", i64::from(selector >> 5 & 0x3) + 1);
        p
    });
    let limits = Limits {
        max_decoded_size: LIMIT,
        ..Limits::default()
    };
    let filters = [Filter {
        name: Name::from(name),
        params,
    }];
    for mode in [Strictness::Lenient, Strictness::Strict] {
        if let Ok(out) = decode(input, &filters, &limits, mode, None) {
            assert!(out.data.len() as u64 <= LIMIT, "output bound exceeded");
        }
    }
});
