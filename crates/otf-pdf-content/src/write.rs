//! Content-stream writer: the inverse of [`parse_content`](crate::parse_content).
//!
//! Operations are written one per line, operands then operator, so the
//! output parses back to the same operations. Anything that would not (an
//! operator the lexer would read as something else, a stream operand) is
//! refused rather than written: a sanitizer rewriting a page must not be
//! able to inject operators by accident.

use otf_pdf_syntax::serialize::write_object;
use otf_pdf_syntax::{Dict, Error, Name, Object, Result};

use crate::ops::{Operation, image_end};

/// Writes `ops` as content-stream bytes.
///
/// Fails with [`Error::WrongType`] for an operation that would not parse
/// back as itself: an empty operator, one with whitespace or delimiters,
/// one that reads as a number or as `true`, `false`, `null`, `R`, `ID` or
/// `EI`, a stream anywhere in the operands, or a `BI` whose operands are
/// not an image dictionary and its data.
///
/// Inline-image data that contains something a reader could take for the
/// closing `EI` is written ASCIIHex-encoded (with `/AHx` prepended to its
/// filters), so it always reads back as the same image.
pub fn write_content(ops: &[Operation]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for op in ops {
        write_operation(op, &mut out)?;
    }
    Ok(out)
}

/// Appends one operation and a newline to `out`. On error nothing is
/// appended.
pub fn write_operation(op: &Operation, out: &mut Vec<u8>) -> Result<()> {
    if op.operator == b"BI" {
        return write_inline_image(op, out);
    }
    check_operator(&op.operator)?;
    for operand in &op.operands {
        check_operand(operand)?;
    }
    for operand in &op.operands {
        write_object(operand, out);
        out.push(b' ');
    }
    out.extend_from_slice(&op.operator);
    out.push(b'\n');
    Ok(())
}

fn wrong(expected: &'static str, found: &'static str) -> Error {
    Error::WrongType { expected, found }
}

fn check_operator(op: &[u8]) -> Result<()> {
    let Some(&first) = op.first() else {
        return Err(wrong("a content operator", "an empty operator"));
    };
    // Any regular byte reads back as part of the keyword, binary junk
    // included: the writer keeps what the parser found and leaves dropping
    // junk to the caller.
    if !op.iter().all(|&b| otf_pdf_syntax::lexer::is_regular(b)) {
        return Err(wrong(
            "a content operator",
            "whitespace or a delimiter in an operator",
        ));
    }
    if first.is_ascii_digit() || matches!(first, b'+' | b'-' | b'.') {
        return Err(wrong(
            "a content operator",
            "an operator that reads as a number",
        ));
    }
    if matches!(op, b"true" | b"false" | b"null" | b"R" | b"ID" | b"EI") {
        return Err(wrong(
            "a content operator",
            "a reserved keyword as an operator",
        ));
    }
    Ok(())
}

fn check_operand(object: &Object) -> Result<()> {
    match object {
        Object::Stream(_) => Err(wrong("a content operand", "a stream")),
        Object::Array(items) => items.iter().try_for_each(check_operand),
        Object::Dict(d) => d.iter().try_for_each(|(_, v)| check_operand(v)),
        _ => Ok(()),
    }
}

/// `BI <dict entries> ID <data>\nEI`.
fn write_inline_image(op: &Operation, out: &mut Vec<u8>) -> Result<()> {
    let [Object::Dict(dict), Object::String(data)] = op.operands.as_slice() else {
        return Err(wrong(
            "an inline image dictionary and data",
            "other BI operands",
        ));
    };
    for (_, value) in dict.iter() {
        check_operand(value)?;
    }
    let (dict, data) = if is_safe_image_data(&data.bytes) {
        (dict.clone(), data.bytes.clone())
    } else {
        hex_encoded(dict, &data.bytes)
    };
    out.extend_from_slice(b"BI\n");
    for (key, value) in dict.iter() {
        write_object(&Object::Name(key.clone()), out);
        out.push(b' ');
        write_object(value, out);
        out.push(b'\n');
    }
    // One whitespace byte separates `ID` from the data, and one ends it.
    out.extend_from_slice(b"ID ");
    out.extend_from_slice(&data);
    out.extend_from_slice(b"\nEI\n");
    Ok(())
}

/// Whether `data`, written as above, reads back whole: the reader's `EI`
/// search must not stop inside it. This check sees no bytes after `EI`,
/// which only makes it stricter than the reader.
fn is_safe_image_data(data: &[u8]) -> bool {
    let mut written = data.to_vec();
    written.extend_from_slice(b"\nEI");
    image_end(&written) == data.len() + 1
}

/// The image with `/AHx` prepended to its filters and `data` hex-encoded.
fn hex_encoded(dict: &Dict, data: &[u8]) -> (Dict, Vec<u8>) {
    let mut dict = dict.clone();
    let (filter_key, parms_key) = if dict.contains_key(b"Filter") {
        ("Filter", "DecodeParms")
    } else {
        ("F", "DP")
    };
    let ahx = Object::Name(Name::from("AHx"));
    let filters = match dict.remove(filter_key.as_bytes()) {
        None => None,
        Some(Object::Array(items)) => Some(items),
        Some(single) => Some(vec![single]),
    };
    match filters {
        None => {
            dict.insert(filter_key, ahx);
        }
        Some(mut items) => {
            let count = items.len();
            items.insert(0, ahx);
            dict.insert(filter_key, Object::Array(items));
            // Parameters line up with filters: AHx takes none.
            if let Some(parms) = dict.remove(parms_key.as_bytes()) {
                let mut list = match parms {
                    Object::Array(list) => list,
                    single => {
                        let mut list = vec![single];
                        list.resize(count, Object::Null);
                        list
                    }
                };
                list.insert(0, Object::Null);
                dict.insert(parms_key, Object::Array(list));
            }
        }
    }
    let mut hex = Vec::with_capacity(data.len() * 2 + 1);
    for b in data {
        hex.extend_from_slice(format!("{b:02X}").as_bytes());
    }
    hex.push(b'>');
    (dict, hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_content;
    use otf_pdf_syntax::{Limits, ObjId, PdfString, Stream};

    fn parse(bytes: &[u8]) -> Vec<Operation> {
        parse_content(bytes, &Limits::default()).unwrap()
    }

    fn op(operator: &str, operands: Vec<Object>) -> Operation {
        Operation {
            operator: operator.as_bytes().to_vec(),
            operands,
        }
    }

    /// Writes, parses back, and checks the operations survived.
    fn round_trip(ops: &[Operation]) -> Vec<u8> {
        let bytes = write_content(ops).unwrap();
        assert_eq!(parse(&bytes), ops, "{}", String::from_utf8_lossy(&bytes));
        bytes
    }

    #[test]
    fn parsed_content_round_trips() {
        let source = b"q 1 0 0 1 10.5 -20 cm BT /F1 12 Tf (Hi \\(there\\)) Tj \
            [(A) -50 (B)] TJ <00ff> Tj ET Q /OC /L1 BDC 0 0 m 10 10 l S EMC \
            /Span << /ActualText (x) /MCID 3 >> BDC EMC true false null d0 1 0 R";
        let ops = parse(source);
        assert!(ops.len() > 10);
        let bytes = round_trip(&ops);
        // One operation per line.
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), ops.len());
        assert!(bytes.starts_with(b"q\n1 0 0 1 10.5 -20 cm\n"));
    }

    #[test]
    fn edited_operations_are_written() {
        let mut ops = parse(b"BT /F1 12 Tf 3 Tr (hidden) Tj 0 Tr (shown) Tj ET");
        // Drop the invisible run, as a sanitizer would.
        ops.retain(|o| o.operands.first() != Some(&Object::String(PdfString::literal("hidden"))));
        assert_eq!(
            String::from_utf8(write_content(&ops).unwrap()).unwrap(),
            "BT\n/F1 12 Tf\n3 Tr\n0 Tr\n(shown) Tj\nET\n"
        );
        assert!(write_content(&[]).unwrap().is_empty());
    }

    #[test]
    fn inline_images_round_trip() {
        let ops = parse(b"q BI /W 2 /H 1 /CS /G /BPC 8 ID \x00EI\xff EI Q");
        let bytes = round_trip(&ops);
        assert!(bytes.windows(4).any(|w| w == b"ID \x00"));
        // Data ending in whitespace keeps it.
        round_trip(&[op(
            "BI",
            vec![
                Object::Dict(Dict::new()),
                Object::String(PdfString::literal(&b"ab \n"[..])),
            ],
        )]);
    }

    #[test]
    fn binary_operators_round_trip() {
        // Junk the parser read as operators is kept as it was.
        round_trip(&[op("\u{1}", vec![Object::Int(0)]), op("gs\u{7}é", vec![])]);
    }

    #[test]
    fn binary_after_ei_is_not_ambiguous() {
        // The reader skips an "EI" followed by binary, so this is written raw.
        let data = &b"\x01 EI \x02"[..];
        assert!(is_safe_image_data(data));
        assert!(!is_safe_image_data(b"\x01 EI Q"));
        assert!(!is_safe_image_data(b"EI"));
    }

    #[test]
    fn ambiguous_image_data_is_hex_encoded() {
        let image = |dict: Dict| {
            op(
                "BI",
                vec![
                    Object::Dict(dict),
                    // " EI Q" inside the data would end the image early.
                    Object::String(PdfString::literal(&b"\x01 EI Q"[..])),
                ],
            )
        };
        let read_back = |o: &Operation| {
            let bytes = write_content(std::slice::from_ref(o)).unwrap();
            let parsed = parse(&bytes);
            assert_eq!(parsed.len(), 1, "{}", String::from_utf8_lossy(&bytes));
            parsed.into_iter().next().unwrap()
        };

        let got = read_back(&image(Dict::new()));
        let dict = got.operands[0].as_dict().unwrap();
        assert_eq!(dict.get_name(b"F"), Some(&b"AHx"[..]));
        // The data is read back still encoded: the hex of 01 " EI Q".
        assert_eq!(got.operands[1].as_string(), Some(&b"012045492051>"[..]));

        // An existing filter and its parameters shift along.
        let mut d = Dict::new();
        d.insert("F", Name::from("Fl"));
        d.insert("DP", Dict::new());
        let got = read_back(&image(d));
        let dict = got.operands[0].as_dict().unwrap();
        assert_eq!(
            dict.get_array(b"F").unwrap(),
            [
                Object::Name(Name::from("AHx")),
                Object::Name(Name::from("Fl"))
            ]
        );
        assert_eq!(
            dict.get_array(b"DP").unwrap(),
            [Object::Null, Object::Dict(Dict::new())]
        );

        // Long names and filter arrays.
        let mut d = Dict::new();
        d.insert(
            "Filter",
            vec![
                Object::Name(Name::from("A85")),
                Object::Name(Name::from("Fl")),
            ],
        );
        let got = read_back(&image(d));
        let dict = got.operands[0].as_dict().unwrap();
        assert_eq!(dict.get_array(b"Filter").unwrap().len(), 3);
        assert!(dict.get(b"F").is_none());
    }

    #[test]
    fn operations_that_would_not_read_back_are_refused() {
        for bad in [
            "", "a b", "Tj\nBT", "(x", "/N", "%x", "1x", "-x", ".x", "true", "null", "R", "ID",
            "EI",
        ] {
            let mut out = b"keep".to_vec();
            assert!(
                matches!(
                    write_operation(&op(bad, vec![]), &mut out),
                    Err(Error::WrongType { .. })
                ),
                "{bad:?}"
            );
            assert_eq!(out, b"keep", "{bad:?}: nothing appended");
        }
        let stream = Object::Stream(Stream {
            dict: Dict::new(),
            raw: std::sync::Arc::from(&b""[..]),
        });
        for operands in [
            vec![stream.clone()],
            vec![Object::Array(vec![stream.clone()])],
        ] {
            assert!(write_content(&[op("Do", operands)]).is_err());
        }
        // A BI needs exactly a dictionary and a string.
        assert!(write_content(&[op("BI", vec![])]).is_err());
        assert!(write_content(&[op("BI", vec![Object::Ref(ObjId::new(1, 0))])]).is_err());
        let mut d = Dict::new();
        d.insert("X", stream);
        assert!(
            write_content(&[op(
                "BI",
                vec![Object::Dict(d), Object::String(PdfString::literal(""))]
            )])
            .is_err()
        );
        // A failure stops the whole write.
        assert!(write_content(&[op("q", vec![]), op("", vec![])]).is_err());
    }
}
