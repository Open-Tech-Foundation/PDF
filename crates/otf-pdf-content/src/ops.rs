//! Content-stream tokenizer (ISO 32000-2 §7.8.2): operands followed by an
//! operator, with inline images (`BI … ID … EI`) read as one operation.

use otf_pdf_syntax::lexer::Token;
use otf_pdf_syntax::parser::Parser;
use otf_pdf_syntax::{Dict, Error, Limits, Name, Object, PdfString, Result, Strictness};

/// One operation: an operator and its operands.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    /// The operator, e.g. `Tj` or `re`.
    pub operator: Vec<u8>,
    /// Its operands, in order. For `BI` (an inline image): the image
    /// dictionary and the data as a string.
    pub operands: Vec<Object>,
}

/// Operands kept before an operator; more is junk, and is dropped.
const MAX_OPERANDS: usize = 4096;

/// Parses a content stream into operations. Malformed operands are skipped
/// (content streams in the wild are full of them), but limits still apply.
pub fn parse_content(bytes: &[u8], limits: &Limits) -> Result<Vec<Operation>> {
    let mut parser = Parser::new(bytes, 0, limits, Strictness::Lenient);
    let mut ops = Vec::new();
    let mut operands: Vec<Object> = Vec::new();
    loop {
        let save = parser.lexer().pos();
        let token = match parser.lexer().next_token() {
            Ok(t) => t,
            // An unterminated string or hex string at the end: stop there.
            Err(Error::UnexpectedEof) => break,
            Err(e) => return Err(e),
        };
        match token {
            Token::Eof => break,
            Token::Keyword(b"BI") => {
                limits.charge(1)?;
                let pos = parser.lexer().pos();
                let rest = bytes.get(pos..).unwrap_or_default();
                let (dict, data, used) = inline_image(rest, limits);
                parser.lexer().set_pos(pos + used);
                ops.push(Operation {
                    operator: b"BI".to_vec(),
                    operands: vec![Object::Dict(dict), Object::String(PdfString::literal(data))],
                });
                operands.clear();
            }
            Token::Keyword(k) if !matches!(k, b"true" | b"false" | b"null") => {
                limits.charge(1)?;
                ops.push(Operation {
                    operator: k.to_vec(),
                    operands: std::mem::take(&mut operands),
                });
            }
            // Stray closers: skip.
            Token::ArrayEnd | Token::DictEnd | Token::BraceClose => {}
            // PostScript-calculator braces only appear in Type 4 functions.
            Token::BraceOpen => {}
            _ => {
                parser.lexer().set_pos(save);
                match parser.parse_object() {
                    Ok(obj) => {
                        if operands.len() < MAX_OPERANDS {
                            operands.push(obj);
                        }
                    }
                    Err(e @ Error::Limit(_)) => return Err(e),
                    Err(Error::UnexpectedEof) => break,
                    Err(_) => {
                        // Skip one byte past whatever could not be parsed.
                        parser.lexer().set_pos(save + 1);
                    }
                }
            }
        }
    }
    Ok(ops)
}

/// Reads an inline image after `BI`: its dictionary up to `ID`, then the
/// data up to `EI`. Returns the dictionary, the data and the bytes used.
fn inline_image(bytes: &[u8], limits: &Limits) -> (Dict, Vec<u8>, usize) {
    let mut parser = Parser::new(bytes, 0, limits, Strictness::Lenient);
    let mut dict = Dict::new();
    loop {
        let save = parser.lexer().pos();
        match parser.lexer().next_token() {
            Ok(Token::Keyword(b"ID")) | Ok(Token::Eof) | Err(_) => break,
            Ok(Token::Name(key)) => match parser.parse_object() {
                Ok(value) => {
                    dict.insert(key, value);
                }
                Err(_) => break,
            },
            Ok(_) => {
                parser.lexer().set_pos(save + 1);
            }
        }
    }
    // One whitespace byte separates `ID` from the data.
    let start = (parser.lexer().pos() + 1).min(bytes.len());
    let data = bytes.get(start..).unwrap_or_default();
    // `EI` delimited by whitespace and followed by text that looks like
    // operators (binary data can contain "EI" by chance).
    let end = (0..data.len().saturating_sub(1))
        .find(|&i| {
            data.get(i..i + 2) == Some(b"EI")
                && (i == 0
                    || data
                        .get(i - 1)
                        .is_some_and(|&b| otf_pdf_syntax::lexer::is_whitespace(b)))
                && data
                    .get(i + 2)
                    .is_none_or(|&b| otf_pdf_syntax::lexer::is_whitespace(b))
                && data
                    .get(i + 2..(i + 12).min(data.len()))
                    .unwrap_or_default()
                    .iter()
                    .all(|&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..0x7f).contains(&b))
        })
        .unwrap_or(data.len());
    let mut image = data.get(..end).unwrap_or_default().to_vec();
    // Drop the whitespace before `EI`.
    if image
        .last()
        .is_some_and(|&b| otf_pdf_syntax::lexer::is_whitespace(b))
    {
        image.pop();
    }
    (dict, image, (start + end + 2).min(bytes.len()))
}

/// The name operand at `i`, if it is one.
pub(crate) fn name_at(operands: &[Object], i: usize) -> Option<&Name> {
    match operands.get(i) {
        Some(Object::Name(n)) => Some(n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(input: &[u8]) -> Vec<Operation> {
        parse_content(input, &Limits::default()).unwrap()
    }

    #[test]
    fn operators_and_operands() {
        let o = ops(b"q 1 0 0 1 10 20 cm BT /F1 12 Tf (Hi) Tj [(A) -50 (B)] TJ ET Q");
        let names: Vec<_> = o
            .iter()
            .map(|op| String::from_utf8_lossy(&op.operator).into_owned())
            .collect();
        assert_eq!(names, ["q", "cm", "BT", "Tf", "Tj", "TJ", "ET", "Q"]);
        assert_eq!(o[1].operands.len(), 6);
        assert_eq!(o[3].operands[0], Object::Name(Name::from("F1")));
        assert_eq!(o[5].operands[0].as_array().unwrap().len(), 3);
    }

    #[test]
    fn inline_image() {
        let o = ops(b"q BI /W 2 /H 1 /CS /G /BPC 8 ID \x00EI\xff EI Q");
        assert_eq!(o.len(), 3);
        assert_eq!(o[1].operator, b"BI");
        assert_eq!(o[1].operands[0].as_dict().unwrap().get_int(b"W"), Some(2));
        // "EI" inside the data is followed by binary, so it is not the end.
        assert_eq!(o[1].operands[1].as_string(), Some(&b"\x00EI\xff"[..]));
        assert_eq!(o[2].operator, b"Q");
    }

    #[test]
    fn junk_is_skipped() {
        let o = ops(b"1 2 ] >> } { m (unterminated");
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].operator, b"m");
        assert_eq!(o[0].operands.len(), 2);
        assert!(ops(b"").is_empty());
        assert!(ops(b"BI /W 1").len() == 1);
    }

    #[test]
    fn keyword_objects_are_operands() {
        let o = ops(b"true false null d0");
        assert_eq!(
            o[0].operands,
            [Object::Bool(true), Object::Bool(false), Object::Null]
        );
    }
}
