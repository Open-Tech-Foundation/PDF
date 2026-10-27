//! Object parser on top of the [`Lexer`].

use crate::diag::{DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};
use crate::lexer::{Lexer, Token};
use crate::limits::{LimitKind, Limits, Strictness};
use crate::object::{Dict, ObjId, Object};

/// Where an indirect object's body ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectEnd {
    /// `endobj` followed (or was missing and the object was accepted).
    Endobj,
    /// `stream` followed; stream data starts at this buffer position.
    Stream {
        /// Position of the first byte of stream data.
        data_start: usize,
    },
}

/// Parses objects from a buffer.
pub struct Parser<'a, 'd> {
    lexer: Lexer<'a>,
    limits: &'d Limits,
    strictness: Strictness,
    diagnostics: Option<&'d Diagnostics>,
    /// Added to buffer positions when reporting file offsets.
    base: u64,
}

impl<'a, 'd> Parser<'a, 'd> {
    /// A parser over `buf`, whose first byte is at file offset `base`.
    pub fn new(buf: &'a [u8], base: u64, limits: &'d Limits, strictness: Strictness) -> Self {
        Self {
            lexer: Lexer::new(buf),
            limits,
            strictness,
            diagnostics: None,
            base,
        }
    }

    /// Records repairs into `diagnostics`.
    pub fn with_diagnostics(mut self, diagnostics: &'d Diagnostics) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    /// The underlying lexer.
    pub fn lexer(&mut self) -> &mut Lexer<'a> {
        &mut self.lexer
    }

    fn offset(&self) -> u64 {
        self.base + self.lexer.pos() as u64
    }

    /// Accepts a repair in lenient mode (recording it) or fails in strict mode.
    fn repair(&self, kind: DiagnosticKind, message: &'static str) -> Result<()> {
        match self.strictness {
            Strictness::Strict => Err(Error::syntax(self.offset(), message)),
            Strictness::Lenient => {
                if let Some(d) = self.diagnostics {
                    d.push(kind, Some(self.offset()), None);
                }
                Ok(())
            }
        }
    }

    /// Parses one direct object.
    pub fn parse_object(&mut self) -> Result<Object> {
        let token = self.lexer.next_token()?;
        self.object_from(token, 0)
    }

    fn object_from(&mut self, token: Token<'a>, depth: u32) -> Result<Object> {
        if depth > self.limits.max_depth {
            return Err(Error::Limit(LimitKind::Depth));
        }
        match token {
            Token::Int(n) => self.int_or_ref(n),
            Token::Real(r) => Ok(Object::Real(r)),
            Token::String(s) => Ok(Object::String(s)),
            Token::Name(n) => Ok(Object::Name(n)),
            Token::ArrayStart => self.array(depth),
            Token::DictStart => self.dict(depth).map(Object::Dict),
            Token::Keyword(b"true") => Ok(Object::Bool(true)),
            Token::Keyword(b"false") => Ok(Object::Bool(false)),
            Token::Keyword(b"null") => Ok(Object::Null),
            Token::Eof => Err(Error::UnexpectedEof),
            _ => Err(Error::syntax(self.offset(), "expected an object")),
        }
    }

    /// `n` alone, or `n g R`.
    fn int_or_ref(&mut self, n: i64) -> Result<Object> {
        let save = self.lexer.pos();
        if let (Ok(num), Ok(Token::Int(g))) = (u32::try_from(n), self.lexer.next_token())
            && let (Ok(generation), Ok(Token::Keyword(b"R"))) =
                (u16::try_from(g), self.lexer.next_token())
        {
            return Ok(Object::Ref(ObjId::new(num, generation)));
        }
        self.lexer.set_pos(save);
        Ok(Object::Int(n))
    }

    fn array(&mut self, depth: u32) -> Result<Object> {
        let mut items = Vec::new();
        loop {
            let save = self.lexer.pos();
            match self.lexer.next_token()? {
                Token::ArrayEnd => return Ok(Object::Array(items)),
                Token::Eof => return Err(Error::UnexpectedEof),
                Token::Keyword(b"endobj" | b"stream" | b"endstream" | b"obj") => {
                    // An unterminated array: end it here and leave the
                    // keyword for the caller.
                    self.lexer.set_pos(save);
                    self.repair(DiagnosticKind::SyntaxError, "unterminated array")?;
                    return Ok(Object::Array(items));
                }
                // Junk, but not the keywords that are objects.
                Token::Keyword(k) if !matches!(k, b"true" | b"false" | b"null") => {
                    self.repair(DiagnosticKind::SyntaxError, "unexpected token in array")?;
                }
                Token::DictEnd | Token::BraceOpen | Token::BraceClose => {
                    self.repair(DiagnosticKind::SyntaxError, "unexpected token in array")?;
                }
                t => items.push(self.object_from(t, depth + 1)?),
            }
        }
    }

    fn dict(&mut self, depth: u32) -> Result<Dict> {
        let mut dict = Dict::new();
        loop {
            let save = self.lexer.pos();
            match self.lexer.next_token()? {
                Token::DictEnd => return Ok(dict),
                Token::Eof => return Err(Error::UnexpectedEof),
                Token::Name(key) => {
                    let save_value = self.lexer.pos();
                    match self.lexer.next_token()? {
                        Token::DictEnd => {
                            self.repair(
                                DiagnosticKind::SyntaxError,
                                "dictionary key without value",
                            )?;
                            dict.insert(key, Object::Null);
                            return Ok(dict);
                        }
                        Token::Keyword(b"endobj" | b"stream" | b"endstream" | b"obj") => {
                            self.lexer.set_pos(save_value);
                            self.repair(DiagnosticKind::SyntaxError, "unterminated dictionary")?;
                            dict.insert(key, Object::Null);
                            return Ok(dict);
                        }
                        Token::Eof => return Err(Error::UnexpectedEof),
                        t => match self.object_from(t, depth + 1) {
                            Ok(v) => {
                                dict.insert(key, v);
                            }
                            Err(Error::Syntax { .. }) if self.strictness == Strictness::Lenient => {
                                self.repair(DiagnosticKind::SyntaxError, "bad dictionary value")?;
                                dict.insert(key, Object::Null);
                            }
                            Err(e) => return Err(e),
                        },
                    }
                }
                Token::Keyword(b"endobj" | b"stream" | b"endstream" | b"obj") => {
                    self.lexer.set_pos(save);
                    self.repair(DiagnosticKind::SyntaxError, "unterminated dictionary")?;
                    return Ok(dict);
                }
                _ => {
                    self.repair(DiagnosticKind::SyntaxError, "dictionary key is not a name")?;
                }
            }
        }
    }

    /// Parses `num gen obj <object>` and what ends it. When `expected` is
    /// given, a different id is an error.
    pub fn parse_indirect(
        &mut self,
        expected: Option<ObjId>,
    ) -> Result<(ObjId, Object, ObjectEnd)> {
        let id = self.indirect_header()?;
        if let Some(want) = expected {
            if want.num != id.num {
                return Err(Error::syntax(self.offset(), "object number mismatch"));
            }
            if want.generation != id.generation {
                self.repair(DiagnosticKind::ObjectIdMismatch, "generation mismatch")?;
            }
        }
        let object = self.parse_object_or_empty()?;
        let save = self.lexer.pos();
        let end = match self.lexer.next_token() {
            Ok(Token::Keyword(b"endobj")) => ObjectEnd::Endobj,
            Ok(Token::Keyword(b"stream")) => {
                if !matches!(object, Object::Dict(_)) {
                    return Err(Error::syntax(self.offset(), "stream without dictionary"));
                }
                ObjectEnd::Stream {
                    data_start: self.stream_data_start(),
                }
            }
            Err(Error::UnexpectedEof) | Ok(Token::Eof) => {
                return Err(Error::UnexpectedEof);
            }
            _ => {
                // Missing `endobj`: the next object (or junk) follows.
                self.lexer.set_pos(save);
                self.repair(DiagnosticKind::MissingEndobj, "missing endobj")?;
                ObjectEnd::Endobj
            }
        };
        Ok((id, object, end))
    }

    /// `num gen obj`.
    fn indirect_header(&mut self) -> Result<ObjId> {
        let (Token::Int(num), Token::Int(generation), Token::Keyword(b"obj")) = (
            self.lexer.next_token()?,
            self.lexer.next_token()?,
            self.lexer.next_token()?,
        ) else {
            return Err(Error::syntax(self.offset(), "expected `num gen obj`"));
        };
        match (u32::try_from(num), u16::try_from(generation)) {
            (Ok(num), Ok(generation)) => Ok(ObjId::new(num, generation)),
            _ => Err(Error::syntax(self.offset(), "object id out of range")),
        }
    }

    /// An object body; `n g obj endobj` is accepted as `null`.
    fn parse_object_or_empty(&mut self) -> Result<Object> {
        let save = self.lexer.pos();
        let token = self.lexer.next_token()?;
        if let Token::Keyword(b"endobj") = token {
            self.lexer.set_pos(save);
            self.repair(DiagnosticKind::SyntaxError, "empty object")?;
            return Ok(Object::Null);
        }
        self.object_from(token, 0)
    }

    /// After the `stream` keyword: skips the end-of-line marker. The
    /// standard requires CRLF or LF; a lone CR and trailing spaces are
    /// tolerated.
    fn stream_data_start(&mut self) -> usize {
        let buf = self.lexer.buffer();
        let mut pos = self.lexer.pos();
        while buf.get(pos) == Some(&b' ') {
            pos += 1;
        }
        match (buf.get(pos), buf.get(pos + 1)) {
            (Some(b'\r'), Some(b'\n')) => pos += 2,
            (Some(b'\r' | b'\n'), _) => pos += 1,
            _ => {}
        }
        self.lexer.set_pos(pos);
        pos
    }

    /// Parses a trailer dictionary, after the `trailer` keyword.
    pub fn parse_dict(&mut self) -> Result<Dict> {
        match self.lexer.next_token()? {
            Token::DictStart => self.dict(0),
            Token::Eof => Err(Error::UnexpectedEof),
            _ => Err(Error::syntax(self.offset(), "expected a dictionary")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Name, PdfString};

    fn name(bytes: &[u8]) -> Object {
        Object::Name(Name(bytes.to_vec()))
    }

    fn parse(input: &[u8]) -> Result<Object> {
        let limits = Limits::default();
        Parser::new(input, 0, &limits, Strictness::Lenient).parse_object()
    }

    fn strict(input: &[u8]) -> Result<Object> {
        let limits = Limits::default();
        Parser::new(input, 0, &limits, Strictness::Strict).parse_object()
    }

    #[test]
    fn scalars() {
        assert_eq!(parse(b"true"), Ok(Object::Bool(true)));
        assert_eq!(parse(b"false"), Ok(Object::Bool(false)));
        assert_eq!(parse(b"null"), Ok(Object::Null));
        assert_eq!(parse(b"42"), Ok(Object::Int(42)));
        assert_eq!(parse(b"-1.5"), Ok(Object::Real(-1.5)));
        assert_eq!(parse(b"/N"), Ok(name(b"N")));
        assert_eq!(
            parse(b"(s)"),
            Ok(Object::String(PdfString::literal(b"s".to_vec())))
        );
    }

    #[test]
    fn references() {
        assert_eq!(parse(b"12 0 R"), Ok(Object::Ref(ObjId::new(12, 0))));
        assert_eq!(
            parse(b"[1 2 R 3 4 5 R]"),
            Ok(Object::Array(vec![
                Object::Ref(ObjId::new(1, 2)),
                Object::Int(3),
                Object::Ref(ObjId::new(4, 5)),
            ]))
        );
        // Not a reference: negative number, generation too large.
        assert_eq!(
            parse(b"[-1 0 R]").unwrap().as_array().map(<[_]>::len),
            Some(2)
        );
        assert_eq!(
            parse(b"[1 70000 R]").unwrap().as_array().map(<[_]>::len),
            Some(2)
        );
    }

    #[test]
    fn dictionaries() {
        let obj = parse(b"<< /Type /Page /Kids [1 0 R] /Sub << /A 1 >> /N null >>").unwrap();
        let d = obj.as_dict().unwrap();
        assert!(d.is_type(b"Page"));
        assert_eq!(d.get_dict(b"Sub").unwrap().get_int(b"A"), Some(1));
        assert!(d.get(b"N").is_none());
    }

    #[test]
    fn lenient_dictionary_repairs() {
        let d = parse(b"<< /A 1 /B >>").unwrap();
        assert_eq!(d.as_dict().unwrap().get_int(b"A"), Some(1));
        let d = parse(b"<< /A 1 2 /B 3 >>").unwrap();
        assert_eq!(d.as_dict().unwrap().get_int(b"B"), Some(3));
        assert!(strict(b"<< /A 1 /B >>").is_err());
        assert!(strict(b"<< 1 /B 3 >>").is_err());
    }

    #[test]
    fn keyword_objects_in_arrays() {
        assert_eq!(
            strict(b"[true false null 1]"),
            Ok(Object::Array(vec![
                Object::Bool(true),
                Object::Bool(false),
                Object::Null,
                Object::Int(1),
            ]))
        );
    }

    #[test]
    fn lenient_array_repairs() {
        assert_eq!(
            parse(b"[1 foo 2]"),
            Ok(Object::Array(vec![Object::Int(1), Object::Int(2)]))
        );
        assert!(strict(b"[1 foo 2]").is_err());
    }

    #[test]
    fn unterminated_containers() {
        assert_eq!(parse(b"[1 2"), Err(Error::UnexpectedEof));
        assert_eq!(parse(b"<< /A 1"), Err(Error::UnexpectedEof));
        assert_eq!(parse(b""), Err(Error::UnexpectedEof));
    }

    #[test]
    fn depth_limit() {
        let mut deep = vec![b'['; 300];
        deep.extend(vec![b']'; 300]);
        assert_eq!(parse(&deep), Err(Error::Limit(LimitKind::Depth)));
        let mut ok = vec![b'['; 200];
        ok.extend(vec![b']'; 200]);
        assert!(parse(&ok).is_ok());
    }

    fn indirect(input: &[u8], mode: Strictness) -> Result<(ObjId, Object, ObjectEnd)> {
        let limits = Limits::default();
        Parser::new(input, 0, &limits, mode).parse_indirect(None)
    }

    #[test]
    fn indirect_objects() {
        let (id, obj, end) = indirect(b"3 0 obj\n<< /A 1 >>\nendobj", Strictness::Strict).unwrap();
        assert_eq!(id, ObjId::new(3, 0));
        assert!(obj.as_dict().is_some());
        assert_eq!(end, ObjectEnd::Endobj);
    }

    #[test]
    fn stream_start() {
        for (input, start) in [
            (&b"1 0 obj << /Length 3 >> stream\r\nabc"[..], 32),
            (b"1 0 obj << /Length 3 >> stream\nabc", 31),
            (b"1 0 obj << /Length 3 >> stream\rabc", 31),
            (b"1 0 obj << /Length 3 >> stream  \nabc", 33),
        ] {
            let (_, _, end) = indirect(input, Strictness::Lenient).unwrap();
            assert_eq!(end, ObjectEnd::Stream { data_start: start });
            assert_eq!(&input[start..], b"abc");
        }
        assert!(indirect(b"1 0 obj 5 stream\nabc", Strictness::Lenient).is_err());
    }

    #[test]
    fn missing_endobj() {
        let diags = Diagnostics::default();
        let limits = Limits::default();
        let (_, obj, end) = Parser::new(b"1 0 obj 5 2 0 obj", 0, &limits, Strictness::Lenient)
            .with_diagnostics(&diags)
            .parse_indirect(None)
            .unwrap();
        assert_eq!(obj, Object::Int(5));
        assert_eq!(end, ObjectEnd::Endobj);
        assert_eq!(diags.snapshot()[0].kind, DiagnosticKind::MissingEndobj);
        assert!(indirect(b"1 0 obj 5 2 0 obj", Strictness::Strict).is_err());
    }

    #[test]
    fn empty_object_is_null() {
        let (_, obj, _) = indirect(b"1 0 obj endobj", Strictness::Lenient).unwrap();
        assert_eq!(obj, Object::Null);
    }

    #[test]
    fn id_checks() {
        let limits = Limits::default();
        let mut p = Parser::new(b"4 0 obj 1 endobj", 0, &limits, Strictness::Lenient);
        assert!(p.parse_indirect(Some(ObjId::new(5, 0))).is_err());
        let mut p = Parser::new(b"4 1 obj 1 endobj", 0, &limits, Strictness::Lenient);
        assert!(p.parse_indirect(Some(ObjId::new(4, 0))).is_ok());
        let mut p = Parser::new(b"4 1 obj 1 endobj", 0, &limits, Strictness::Strict);
        assert!(p.parse_indirect(Some(ObjId::new(4, 0))).is_err());
        assert!(indirect(b"x 0 obj", Strictness::Lenient).is_err());
        assert!(indirect(b"99999999999 0 obj 1 endobj", Strictness::Lenient).is_err());
    }

    #[test]
    fn trailer_dict() {
        let limits = Limits::default();
        let mut p = Parser::new(b" << /Size 3 >>", 0, &limits, Strictness::Strict);
        assert_eq!(p.parse_dict().unwrap().get_int(b"Size"), Some(3));
        let mut p = Parser::new(b" 5", 0, &limits, Strictness::Strict);
        assert!(p.parse_dict().is_err());
    }
}
