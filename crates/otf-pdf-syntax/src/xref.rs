//! Cross-reference sections: classic tables and xref streams (§7.5.4,
//! §7.5.8).

use std::collections::BTreeMap;

use crate::diag::{DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};
use crate::lexer::{Lexer, Token};
use crate::limits::{Limits, Strictness};
use crate::object::{Dict, Object};
use crate::parser::Parser;

/// Where an object lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    /// A free (deleted or never used) object number.
    Free,
    /// An object written directly in the file.
    InUse {
        /// Byte offset of `num gen obj`.
        offset: u64,
        /// Generation number.
        generation: u16,
    },
    /// An object inside an object stream.
    Compressed {
        /// Object number of the object stream.
        stream: u32,
        /// Index of the object within the stream.
        index: u32,
    },
}

/// The entries of one xref section, by object number.
pub type XrefMap = BTreeMap<u32, XrefEntry>;

/// Parses a classic `xref` table and the trailer after it. `buf` starts at
/// the `xref` keyword, at file offset `base`.
pub fn parse_table(
    buf: &[u8],
    base: u64,
    limits: &Limits,
    strictness: Strictness,
    diagnostics: &Diagnostics,
) -> Result<(XrefMap, Dict)> {
    let mut lexer = Lexer::new(buf);
    if lexer.next_token()? != Token::Keyword(b"xref") {
        return Err(Error::syntax(base, "expected `xref`"));
    }
    let mut entries = XrefMap::new();
    let mut first_subsection = true;
    loop {
        let at = base + lexer.pos() as u64;
        match lexer.next_token()? {
            Token::Keyword(b"trailer") => break,
            Token::Int(first) => {
                let Token::Int(count) = lexer.next_token()? else {
                    return Err(Error::syntax(at, "expected xref subsection count"));
                };
                let (Ok(first), Ok(count)) = (u32::try_from(first), u32::try_from(count)) else {
                    return Err(Error::syntax(at, "invalid xref subsection"));
                };
                if u64::from(first) + u64::from(count) > u64::from(limits.max_objects) + 1 {
                    return Err(Error::syntax(at, "xref subsection too large"));
                }
                let mut parsed = Vec::new();
                for _ in 0..count {
                    let save = lexer.pos();
                    match (
                        lexer.next_token()?,
                        lexer.next_token()?,
                        lexer.next_token()?,
                    ) {
                        (Token::Int(offset), Token::Int(generation), Token::Keyword(kind))
                            if kind == b"n" || kind == b"f" =>
                        {
                            parsed.push((offset, generation, kind == b"n"));
                        }
                        _ => {
                            // Fewer entries than claimed.
                            lexer.set_pos(save);
                            if strictness == Strictness::Strict {
                                return Err(Error::syntax(at, "short xref subsection"));
                            }
                            diagnostics.push(DiagnosticKind::BadXrefSection, Some(at), None);
                            break;
                        }
                    }
                }
                // A common writer bug: the first subsection starts at 1 but
                // its first entry is object 0's free-list head.
                let mut first = first;
                if first_subsection
                    && first == 1
                    && parsed
                        .first()
                        .is_some_and(|&(o, g, used)| o == 0 && g == 65535 && !used)
                {
                    first = 0;
                }
                first_subsection = false;
                for (i, (offset, generation, used)) in parsed.into_iter().enumerate() {
                    let Some(num) = first.checked_add(i as u32) else {
                        break;
                    };
                    let entry = if used && offset > 0 {
                        XrefEntry::InUse {
                            offset: offset as u64,
                            generation: generation.clamp(0, 65535) as u16,
                        }
                    } else {
                        XrefEntry::Free
                    };
                    entries.insert(num, entry);
                }
            }
            Token::Eof => return Err(Error::UnexpectedEof),
            _ => return Err(Error::syntax(at, "expected xref subsection or `trailer`")),
        }
    }
    let pos = lexer.pos();
    let rest = buf.get(pos..).unwrap_or_default();
    let trailer = Parser::new(rest, base + pos as u64, limits, strictness)
        .with_diagnostics(diagnostics)
        .parse_dict()?;
    Ok((entries, trailer))
}

/// Parses the decoded data of an xref stream with dictionary `dict`.
pub fn parse_stream(dict: &Dict, data: &[u8], limits: &Limits) -> Result<XrefMap> {
    let widths: Vec<usize> = dict
        .get_array(b"W")
        .ok_or(Error::syntax(0, "xref stream without /W"))?
        .iter()
        .map(|w| {
            w.as_int()
                .and_then(|w| usize::try_from(w).ok())
                .unwrap_or(usize::MAX)
        })
        .collect();
    let [w0, w1, w2] = widths[..] else {
        return Err(Error::syntax(0, "xref stream /W must have three entries"));
    };
    if w0 > 8 || w1 > 8 || w2 > 8 {
        return Err(Error::syntax(0, "xref stream field wider than 8 bytes"));
    }
    let width = w0 + w1 + w2;
    if width == 0 {
        return Err(Error::syntax(0, "xref stream entries have zero width"));
    }
    let size = dict.get_int(b"Size").unwrap_or(0);
    let index: Vec<(i64, i64)> = match dict.get_array(b"Index") {
        Some(a) => a
            .chunks(2)
            .filter_map(|p| match p {
                [a, b] => Some((a.as_int()?, b.as_int()?)),
                _ => None,
            })
            .collect(),
        None => vec![(0, size)],
    };

    let mut entries = XrefMap::new();
    let mut rows = data.chunks_exact(width);
    for (first, count) in index {
        let (Ok(first), Ok(count)) = (u32::try_from(first), u32::try_from(count)) else {
            return Err(Error::syntax(0, "invalid xref stream /Index"));
        };
        for i in 0..count {
            let Some(num) = first.checked_add(i) else {
                break;
            };
            if num > limits.max_objects {
                return Err(Error::syntax(0, "xref stream object number too large"));
            }
            // Fewer rows than /Index claims: stop at the data's end.
            let Some(row) = rows.next() else {
                return Ok(entries);
            };
            let field = |from: usize, len: usize| -> u64 {
                row.get(from..from + len)
                    .unwrap_or_default()
                    .iter()
                    .fold(0u64, |acc, &b| acc << 8 | u64::from(b))
            };
            let kind = if w0 == 0 { 1 } else { field(0, w0) };
            let a = field(w0, w1);
            let b = field(w0 + w1, w2);
            let entry = match kind {
                0 => XrefEntry::Free,
                1 if a > 0 => XrefEntry::InUse {
                    offset: a,
                    generation: b.min(65535) as u16,
                },
                1 => XrefEntry::Free,
                2 => XrefEntry::Compressed {
                    stream: u32::try_from(a).unwrap_or(u32::MAX),
                    index: u32::try_from(b).unwrap_or(u32::MAX),
                },
                // Unknown types are references to the null object (§7.5.8.3).
                _ => XrefEntry::Free,
            };
            entries.insert(num, entry);
        }
    }
    Ok(entries)
}

/// Whether a stream dictionary is an xref stream.
pub fn is_xref_stream(object: &Object) -> bool {
    object.as_stream().is_some_and(|s| {
        s.dict.is_type(b"XRef") || (s.dict.contains_key(b"W") && s.dict.contains_key(b"Size"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Name;

    fn table(input: &[u8]) -> Result<(XrefMap, Dict)> {
        parse_table(
            input,
            0,
            &Limits::default(),
            Strictness::Lenient,
            &Diagnostics::default(),
        )
    }

    #[test]
    fn classic_table() {
        let (entries, trailer) = table(
            b"xref\n0 3\n0000000000 65535 f \n0000000015 00000 n \n0000000079 00001 n \n\
              5 1\n0000000200 00000 n\r\ntrailer\n<< /Size 6 /Root 1 0 R >>",
        )
        .unwrap();
        assert_eq!(entries.get(&0), Some(&XrefEntry::Free));
        assert_eq!(
            entries.get(&1),
            Some(&XrefEntry::InUse {
                offset: 15,
                generation: 0
            })
        );
        assert_eq!(
            entries.get(&2),
            Some(&XrefEntry::InUse {
                offset: 79,
                generation: 1
            })
        );
        assert!(matches!(
            entries.get(&5),
            Some(XrefEntry::InUse { offset: 200, .. })
        ));
        assert_eq!(trailer.get_int(b"Size"), Some(6));
    }

    #[test]
    fn off_by_one_first_subsection() {
        let (entries, _) =
            table(b"xref\n1 2\n0000000000 65535 f \n0000000015 00000 n \ntrailer\n<<>>").unwrap();
        assert!(matches!(
            entries.get(&1),
            Some(XrefEntry::InUse { offset: 15, .. })
        ));
    }

    #[test]
    fn short_subsection() {
        let diags = Diagnostics::default();
        let (entries, _) = parse_table(
            b"xref\n0 5\n0000000000 65535 f \n0000000015 00000 n \ntrailer\n<<>>",
            0,
            &Limits::default(),
            Strictness::Lenient,
            &diags,
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(diags.len(), 1);
        assert!(
            parse_table(
                b"xref\n0 5\n0000000000 65535 f \ntrailer\n<<>>",
                0,
                &Limits::default(),
                Strictness::Strict,
                &diags,
            )
            .is_err()
        );
    }

    #[test]
    fn huge_claimed_count_is_rejected() {
        assert!(table(b"xref\n0 2000000000\n0000000000 65535 f \ntrailer\n<<>>").is_err());
    }

    #[test]
    fn zero_offset_in_use_is_free() {
        let (entries, _) =
            table(b"xref\n0 2\n0000000000 65535 f \n0000000000 00000 n \ntrailer\n<<>>").unwrap();
        assert_eq!(entries.get(&1), Some(&XrefEntry::Free));
    }

    #[test]
    fn malformed_tables() {
        assert!(table(b"xref\nfoo").is_err());
        assert!(table(b"xreff").is_err());
        assert_eq!(table(b"xref\n0 1\n"), Err(Error::UnexpectedEof));
        assert!(table(b"xref\n0 x").is_err());
        assert!(table(b"xref\n-1 1\n").is_err());
    }

    fn stream_dict(w: [i64; 3], index: Option<Vec<i64>>, size: i64) -> Dict {
        let mut d = Dict::new();
        d.insert("Type", Name::from("XRef"));
        d.insert("W", w.iter().map(|&x| Object::Int(x)).collect::<Vec<_>>());
        d.insert("Size", size);
        if let Some(index) = index {
            d.insert(
                "Index",
                index.into_iter().map(Object::Int).collect::<Vec<_>>(),
            );
        }
        d
    }

    #[test]
    fn xref_stream_entries() {
        let dict = stream_dict([1, 2, 1], None, 3);
        let data = [0, 0, 0, 255, 1, 0, 15, 0, 2, 0, 7, 3];
        let entries = parse_stream(&dict, &data, &Limits::default()).unwrap();
        assert_eq!(entries.get(&0), Some(&XrefEntry::Free));
        assert_eq!(
            entries.get(&1),
            Some(&XrefEntry::InUse {
                offset: 15,
                generation: 0
            })
        );
        assert_eq!(
            entries.get(&2),
            Some(&XrefEntry::Compressed {
                stream: 7,
                index: 3
            })
        );
    }

    #[test]
    fn xref_stream_index_and_defaults() {
        // /W [0 2 0]: type defaults to 1, generation to 0.
        let dict = stream_dict([0, 2, 0], Some(vec![10, 2]), 12);
        let entries = parse_stream(&dict, &[0, 5, 0, 9], &Limits::default()).unwrap();
        assert!(matches!(
            entries.get(&10),
            Some(XrefEntry::InUse { offset: 5, .. })
        ));
        assert!(matches!(
            entries.get(&11),
            Some(XrefEntry::InUse { offset: 9, .. })
        ));
        // Short data: stops early.
        let entries = parse_stream(&dict, &[0, 5], &Limits::default()).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn xref_stream_errors() {
        let limits = Limits::default();
        assert!(parse_stream(&Dict::new(), &[], &limits).is_err());
        assert!(parse_stream(&stream_dict([9, 1, 1], None, 1), &[], &limits).is_err());
        assert!(parse_stream(&stream_dict([0, 0, 0], None, 1), &[], &limits).is_err());
        let mut d = stream_dict([1, 1, 1], None, 1);
        d.insert("W", vec![Object::Int(1)]);
        assert!(parse_stream(&d, &[], &limits).is_err());
        let huge = stream_dict([1, 1, 1], Some(vec![u32::MAX as i64, 1]), 1);
        assert!(parse_stream(&huge, &[1, 1, 1], &limits).is_err());
    }
}
