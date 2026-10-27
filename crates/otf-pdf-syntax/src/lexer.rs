//! Tokenizer for PDF syntax (ISO 32000-2 §7.2), also used for content
//! streams.
//!
//! Lenient where real files are: doubled signs, `.5` and `5.` numbers, odd
//! hex digit counts, junk inside hex strings, overflowing octal escapes and
//! invalid `#` escapes in names are all accepted. Running out of input inside
//! a token is [`Error::UnexpectedEof`], so a caller reading a window of a
//! larger file can retry with more bytes.

use crate::error::{Error, Result};
use crate::object::{Name, PdfString};

/// One token.
#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    /// An integer.
    Int(i64),
    /// A real number.
    Real(f64),
    /// A literal or hex string.
    String(PdfString),
    /// A name, `/` stripped and escapes resolved.
    Name(Name),
    /// `[`
    ArrayStart,
    /// `]`
    ArrayEnd,
    /// `<<`
    DictStart,
    /// `>>`
    DictEnd,
    /// `{`
    BraceOpen,
    /// `}`
    BraceClose,
    /// Any other run of regular characters: `obj`, `R`, `true`, operators.
    Keyword(&'a [u8]),
    /// End of input.
    Eof,
}

/// Whether `b` is PDF whitespace.
pub fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

/// Whether `b` is a PDF delimiter.
pub fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Whether `b` is a regular character (neither whitespace nor delimiter).
pub fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A tokenizer over a byte buffer.
#[derive(Debug, Clone)]
pub struct Lexer<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    /// A lexer at the start of `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// The whole buffer.
    pub fn buffer(&self) -> &'a [u8] {
        self.buf
    }

    /// Current position in the buffer.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Moves to `pos` (clamped to the buffer's end).
    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos.min(self.buf.len());
    }

    /// Whether the end of the buffer is reached.
    pub fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// The bytes from the current position on.
    pub fn rest(&self) -> &'a [u8] {
        self.buf.get(self.pos..).unwrap_or_default()
    }

    /// The next byte, without consuming it.
    pub fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    /// Skips whitespace and comments.
    pub fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek() {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\r' || c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Reads the next token.
    pub fn next_token(&mut self) -> Result<Token<'a>> {
        self.skip_whitespace();
        let Some(b) = self.peek() else {
            return Ok(Token::Eof);
        };
        match b {
            b'[' => {
                self.pos += 1;
                Ok(Token::ArrayStart)
            }
            b']' => {
                self.pos += 1;
                Ok(Token::ArrayEnd)
            }
            b'{' => {
                self.pos += 1;
                Ok(Token::BraceOpen)
            }
            b'}' => {
                self.pos += 1;
                Ok(Token::BraceClose)
            }
            b'<' => {
                if self.buf.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Ok(Token::DictStart)
                } else {
                    self.pos += 1;
                    self.hex_string()
                }
            }
            b'>' => {
                self.pos += 1;
                if self.peek() == Some(b'>') {
                    self.pos += 1;
                    Ok(Token::DictEnd)
                } else {
                    // A stray `>`: report it as a keyword so the parser can
                    // skip it.
                    Ok(Token::Keyword(b">"))
                }
            }
            b'(' => {
                self.pos += 1;
                self.literal_string()
            }
            b')' => {
                self.pos += 1;
                Ok(Token::Keyword(b")"))
            }
            b'/' => {
                self.pos += 1;
                Ok(self.name())
            }
            b'0'..=b'9' | b'+' | b'-' | b'.' => Ok(self.number()),
            _ => {
                let start = self.pos;
                while self.peek().is_some_and(is_regular) {
                    self.pos += 1;
                }
                Ok(Token::Keyword(
                    self.buf.get(start..self.pos).unwrap_or_default(),
                ))
            }
        }
    }

    fn number(&mut self) -> Token<'a> {
        let mut negative = false;
        // Lenient: `--5` and `+-5` occur in the wild; readers treat any `-`
        // among the leading signs as negative.
        while let Some(b @ (b'+' | b'-')) = self.peek() {
            negative |= b == b'-';
            self.pos += 1;
        }
        let int_start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        let int_digits = self.buf.get(int_start..self.pos).unwrap_or_default();
        let mut frac_digits: &[u8] = &[];
        let is_real = self.peek() == Some(b'.');
        if is_real {
            self.pos += 1;
            let frac_start = self.pos;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
            frac_digits = self.buf.get(frac_start..self.pos).unwrap_or_default();
            // `1.2.3` and `1.-2`: readers take the leading number and drop the
            // rest of the regular run.
            while self
                .peek()
                .is_some_and(|b| b == b'.' || b == b'-' || b.is_ascii_digit())
            {
                self.pos += 1;
            }
        }
        if !is_real {
            let mut value: i64 = 0;
            let mut overflow = false;
            for &d in int_digits {
                match value
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(i64::from(d - b'0')))
                {
                    Some(v) => value = v,
                    None => {
                        overflow = true;
                        break;
                    }
                }
            }
            if !overflow {
                return Token::Int(if negative { -value } else { value });
            }
        }
        // The digits are ASCII, so this always parses; `str::parse` rounds
        // correctly where summing digit by digit would not.
        let mut text = String::with_capacity(int_digits.len() + frac_digits.len() + 2);
        text.push('0');
        text.extend(int_digits.iter().map(|&d| char::from(d)));
        text.push('.');
        text.extend(frac_digits.iter().map(|&d| char::from(d)));
        let value: f64 = text.parse().unwrap_or(0.0);
        Token::Real(if negative { -value } else { value })
    }

    fn name(&mut self) -> Token<'a> {
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let hi = self.peek().and_then(hex_value);
                let lo = self.buf.get(self.pos + 1).copied().and_then(hex_value);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push(hi << 4 | lo);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(b);
        }
        Token::Name(Name(out))
    }

    fn hex_string(&mut self) -> Result<Token<'a>> {
        let mut out = Vec::new();
        let mut high: Option<u8> = None;
        loop {
            let Some(b) = self.bump() else {
                return Err(Error::UnexpectedEof);
            };
            if b == b'>' {
                break;
            }
            // Whitespace and, leniently, any other junk is skipped.
            let Some(v) = hex_value(b) else { continue };
            match high.take() {
                Some(h) => out.push(h << 4 | v),
                None => high = Some(v),
            }
        }
        if let Some(h) = high {
            out.push(h << 4);
        }
        Ok(Token::String(PdfString::hex(out)))
    }

    fn literal_string(&mut self) -> Result<Token<'a>> {
        let mut out = Vec::new();
        let mut depth = 1u32;
        loop {
            let Some(b) = self.bump() else {
                return Err(Error::UnexpectedEof);
            };
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\r' => {
                    // An unescaped end of line reads as `\n`.
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                b'\\' => {
                    let Some(e) = self.bump() else {
                        return Err(Error::UnexpectedEof);
                    };
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            // High-order overflow is ignored (§7.3.4.2).
                            out.push((v & 0xff) as u8);
                        }
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        Ok(Token::String(PdfString::literal(out)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &[u8]) -> Vec<Token<'_>> {
        let mut lexer = Lexer::new(input);
        let mut out = Vec::new();
        loop {
            match lexer.next_token().unwrap() {
                Token::Eof => return out,
                t => out.push(t),
            }
        }
    }

    fn one(input: &[u8]) -> Token<'_> {
        let mut t = tokens(input);
        assert_eq!(t.len(), 1, "{t:?}");
        t.remove(0)
    }

    #[test]
    fn numbers() {
        assert_eq!(one(b"123"), Token::Int(123));
        assert_eq!(one(b"-17"), Token::Int(-17));
        assert_eq!(one(b"+5"), Token::Int(5));
        assert_eq!(one(b"0"), Token::Int(0));
        assert_eq!(one(b"34.5"), Token::Real(34.5));
        assert_eq!(one(b"-.002"), Token::Real(-0.002));
        assert_eq!(one(b"4."), Token::Real(4.0));
        assert_eq!(one(b"--5"), Token::Int(-5));
        assert_eq!(one(b"-"), Token::Int(0));
        assert_eq!(one(b"."), Token::Real(0.0));
        assert_eq!(one(b"1.2.3"), Token::Real(1.2));
        assert_eq!(one(b"0.00-1"), Token::Real(0.0));
        assert_eq!(
            one(b"99999999999999999999"),
            Token::Real(99999999999999999999.0)
        );
    }

    #[test]
    fn number_then_keyword() {
        assert_eq!(
            tokens(b"1 0obj"),
            vec![Token::Int(1), Token::Int(0), Token::Keyword(b"obj")]
        );
    }

    #[test]
    fn literal_strings() {
        let s = |b: &[u8]| Token::String(PdfString::literal(b.to_vec()));
        assert_eq!(one(b"(hello)"), s(b"hello"));
        assert_eq!(one(b"(a (nested) b)"), s(b"a (nested) b"));
        assert_eq!(
            one(b"(\\n\\r\\t\\b\\f\\(\\)\\\\)"),
            s(b"\n\r\t\x08\x0c()\\")
        );
        assert_eq!(one(b"(\\101\\60\\0053)"), s(b"A0\x053"));
        assert_eq!(one(b"(\\777)"), s(b"\xff"));
        assert_eq!(one(b"(a\\\r\nb\\\nc)"), s(b"abc"));
        assert_eq!(one(b"(a\r\nb\rc)"), s(b"a\nb\nc"));
        assert_eq!(one(b"(\\q)"), s(b"q"));
        assert_eq!(one(b"()"), s(b""));
    }

    #[test]
    fn hex_strings() {
        let h = |b: &[u8]| Token::String(PdfString::hex(b.to_vec()));
        assert_eq!(one(b"<48656C6c6f>"), h(b"Hello"));
        assert_eq!(one(b"<901FA>"), h(b"\x90\x1f\xa0"));
        assert_eq!(one(b"<4 8\n6 5>"), h(b"He"));
        assert_eq!(one(b"<4z8>"), h(b"H"));
        assert_eq!(one(b"<>"), h(b""));
    }

    #[test]
    fn names() {
        let n = |b: &[u8]| Token::Name(Name(b.to_vec()));
        assert_eq!(one(b"/Name1"), n(b"Name1"));
        assert_eq!(
            one(b"/A;Name_With-Various***Characters?"),
            n(b"A;Name_With-Various***Characters?")
        );
        assert_eq!(one(b"/lime#20Green"), n(b"lime Green"));
        assert_eq!(one(b"/paired#28#29parentheses"), n(b"paired()parentheses"));
        assert_eq!(one(b"/bad#zz"), n(b"bad#zz"));
        assert_eq!(one(b"/end#"), n(b"end#"));
        assert_eq!(one(b"/"), n(b""));
        assert_eq!(tokens(b"/A/B"), vec![n(b"A"), n(b"B")]);
    }

    #[test]
    fn delimiters_and_comments() {
        assert_eq!(
            tokens(b"<< /K [1 2] >> % comment\n{ } true"),
            vec![
                Token::DictStart,
                Token::Name(Name(b"K".to_vec())),
                Token::ArrayStart,
                Token::Int(1),
                Token::Int(2),
                Token::ArrayEnd,
                Token::DictEnd,
                Token::BraceOpen,
                Token::BraceClose,
                Token::Keyword(b"true"),
            ]
        );
    }

    #[test]
    fn stray_closers() {
        assert_eq!(
            tokens(b"> )"),
            vec![Token::Keyword(b">"), Token::Keyword(b")")]
        );
    }

    #[test]
    fn unterminated_is_eof() {
        for input in [&b"(abc"[..], b"<414", b"(a\\"] {
            assert_eq!(Lexer::new(input).next_token(), Err(Error::UnexpectedEof));
        }
    }

    #[test]
    fn position_helpers() {
        let mut l = Lexer::new(b"  abc");
        l.skip_whitespace();
        assert_eq!(l.pos(), 2);
        assert_eq!(l.rest(), b"abc");
        l.set_pos(100);
        assert!(l.at_end());
        assert_eq!(l.peek(), None);
        assert_eq!(l.buffer(), b"  abc");
    }
}
