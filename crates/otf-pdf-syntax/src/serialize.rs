//! Writing objects back as PDF syntax.
//!
//! Output is deterministic (dictionary keys are sorted, numbers have one
//! spelling) so the same object always serializes to the same bytes.

use crate::lexer::is_regular;
use crate::object::{Dict, Name, Object, PdfString};

/// Appends `object` in PDF syntax. A stream is written as its dictionary
/// followed by `stream`, its raw bytes and `endstream`.
pub fn write_object(object: &Object, out: &mut Vec<u8>) {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(true) => out.extend_from_slice(b"true"),
        Object::Bool(false) => out.extend_from_slice(b"false"),
        Object::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => write_real(*r, out),
        Object::String(s) => write_string(s, out),
        Object::Name(n) => write_name(n, out),
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object(item, out);
            }
            out.push(b']');
        }
        Object::Dict(d) => write_dict(d, out),
        Object::Stream(s) => {
            let mut dict = s.dict.clone();
            dict.insert("Length", s.raw.len() as i64);
            write_dict(&dict, out);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&s.raw);
            out.extend_from_slice(b"\nendstream");
        }
        Object::Ref(id) => {
            out.extend_from_slice(format!("{} {} R", id.num, id.generation).as_bytes())
        }
    }
}

/// `object` in PDF syntax, as a byte vector.
pub fn to_bytes(object: &Object) -> Vec<u8> {
    let mut out = Vec::new();
    write_object(object, &mut out);
    out
}

fn write_dict(dict: &Dict, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (key, value) in dict.iter() {
        write_name(key, out);
        out.push(b' ');
        write_object(value, out);
        out.push(b' ');
    }
    out.extend_from_slice(b">>");
}

/// A real with up to six decimals, no exponent, no trailing zeros; a
/// non-finite value (which PDF cannot express) is written as 0.
fn write_real(r: f64, out: &mut Vec<u8>) {
    if !r.is_finite() {
        out.push(b'0');
        return;
    }
    let s = format!("{r:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    let s = if s == "-0" || s.is_empty() { "0" } else { s };
    out.extend_from_slice(s.as_bytes());
}

fn write_name(name: &Name, out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in name.as_bytes() {
        if is_regular(b) && b != b'#' && (b'!'..=b'~').contains(&b) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}

fn write_string(s: &PdfString, out: &mut Vec<u8>) {
    // Hex when it was hex, or when mostly unprintable (shorter and clearer).
    let unprintable = s
        .bytes
        .iter()
        .filter(|&&b| !(0x20..0x7f).contains(&b))
        .count();
    if s.hex || unprintable * 4 > s.bytes.len() {
        out.push(b'<');
        for b in &s.bytes {
            out.extend_from_slice(format!("{b:02x}").as_bytes());
        }
        out.push(b'>');
        return;
    }
    out.push(b'(');
    for &b in &s.bytes {
        match b {
            b'(' | b')' | b'\\' => out.extend_from_slice(&[b'\\', b]),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0x20..=0x7e => out.push(b),
            _ => out.extend_from_slice(format!("\\{b:03o}").as_bytes()),
        }
    }
    out.push(b')');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{Limits, Strictness};
    use crate::object::{ObjId, Stream};
    use crate::parser::Parser;

    fn text(o: &Object) -> String {
        String::from_utf8(to_bytes(o)).unwrap()
    }

    fn reparse(o: &Object) -> Object {
        let limits = Limits::default();
        Parser::new(&to_bytes(o), 0, &limits, Strictness::Strict)
            .parse_object()
            .unwrap()
    }

    #[test]
    fn scalars() {
        assert_eq!(text(&Object::Null), "null");
        assert_eq!(text(&Object::Bool(false)), "false");
        assert_eq!(text(&Object::Int(-3)), "-3");
        assert_eq!(text(&Object::Real(0.5)), "0.5");
        assert_eq!(text(&Object::Real(2.0)), "2");
        assert_eq!(text(&Object::Real(-0.0000001)), "0");
        assert_eq!(text(&Object::Real(f64::NAN)), "0");
        assert_eq!(text(&Object::Real(1e20)), "100000000000000000000");
        assert_eq!(text(&Object::Ref(ObjId::new(4, 1))), "4 1 R");
    }

    #[test]
    fn names_are_escaped() {
        assert_eq!(
            text(&Object::Name(Name::from("A B#(c)"))),
            "/A#20B#23#28c#29"
        );
        assert_eq!(text(&Object::Name(Name::new(vec![0xe9]))), "/#E9");
    }

    #[test]
    fn strings() {
        assert_eq!(
            text(&Object::String(PdfString::literal("a(b)\\\n"))),
            "(a\\(b\\)\\\\\\n)"
        );
        assert_eq!(
            text(&Object::String(PdfString::literal(vec![
                b'a', 0xe9, b'b', b'c', b'd'
            ]))),
            "(a\\351bcd)"
        );
        assert_eq!(
            text(&Object::String(PdfString::literal(vec![0, 1, 2]))),
            "<000102>"
        );
        assert_eq!(text(&Object::String(PdfString::hex("A"))), "<41>");
    }

    #[test]
    fn containers_round_trip() {
        let mut d = Dict::new();
        d.insert("Type", Name::from("Page"));
        d.insert(
            "Kids",
            vec![Object::Ref(ObjId::new(1, 0)), Object::Real(1.25)],
        );
        d.insert("S", PdfString::literal(b"(a\xffbc)".to_vec()));
        let o = Object::Dict(d);
        assert_eq!(reparse(&o), o);
        assert_eq!(
            text(&o),
            "<</Kids [1 0 R 1.25] /S (\\(a\\377bc\\)) /Type /Page >>"
        );
    }

    #[test]
    fn streams_carry_their_length() {
        let s = Object::Stream(Stream {
            dict: Dict::new(),
            raw: b"abc".to_vec().into(),
        });
        assert_eq!(text(&s), "<</Length 3 >>\nstream\nabc\nendstream");
    }
}
