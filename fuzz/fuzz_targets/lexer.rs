//! Tokenize and parse arbitrary bytes as objects, in both modes. The
//! property is that nothing panics and every loop terminates.
#![no_main]

use libfuzzer_sys::fuzz_target;
use otf_pdf_syntax::lexer::{Lexer, Token};
use otf_pdf_syntax::parser::Parser;
use otf_pdf_syntax::{Limits, Strictness};

fuzz_target!(|data: &[u8]| {
    let mut lexer = Lexer::new(data);
    let mut last = usize::MAX;
    while let Ok(token) = lexer.next_token() {
        if token == Token::Eof {
            break;
        }
        // Every token consumes input.
        assert_ne!(lexer.pos(), last);
        last = lexer.pos();
    }
    let limits = Limits::default();
    for mode in [Strictness::Lenient, Strictness::Strict] {
        let _ = Parser::new(data, 0, &limits, mode).parse_object();
        let _ = Parser::new(data, 0, &limits, mode).parse_indirect(None);
    }
});
