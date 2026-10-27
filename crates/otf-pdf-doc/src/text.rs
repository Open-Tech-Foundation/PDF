//! Text strings (ISO 32000-2 §7.9.2.2): the encoding used by metadata,
//! outline titles, annotation text and other human-readable strings.

/// Decodes a text string: UTF-16BE with a BOM, UTF-8 with a BOM (PDF 2.0),
/// otherwise PDFDocEncoding. UTF-16LE with a BOM, which some writers emit,
/// is accepted too. Undecodable units become U+FFFD.
pub fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return utf16(rest, u16::from_be_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| pdf_doc_char(b)).collect()
}

/// Encodes a text string: PDFDocEncoding when every character has a byte
/// there, otherwise UTF-16BE with a byte order mark (works in every version).
pub fn encode_text_string(text: &str) -> Vec<u8> {
    let doc: Option<Vec<u8>> = text.chars().map(pdf_doc_byte).collect();
    match doc {
        // A PDFDocEncoding string must not start like a byte order mark.
        Some(bytes)
            if !bytes.starts_with(&[0xfe, 0xff]) && !bytes.starts_with(&[0xef, 0xbb, 0xbf]) =>
        {
            bytes
        }
        _ => {
            let mut out = vec![0xfe, 0xff];
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
            out
        }
    }
}

/// The PDFDocEncoding byte for `c`, if there is one.
fn pdf_doc_byte(c: char) -> Option<u8> {
    if c == '\u{fffd}' {
        return None;
    }
    (0u8..=255).find(|&b| pdf_doc_char(b) == c)
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units = bytes.chunks(2).map(|c| {
        unit([
            c.first().copied().unwrap_or(0),
            c.get(1).copied().unwrap_or(0),
        ])
    });
    // A language escape (U+001B ... U+001B) carries a language tag, not text.
    let mut out = String::new();
    let mut in_escape = false;
    for c in char::decode_utf16(units) {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        if c == '\u{1b}' {
            in_escape = !in_escape;
            continue;
        }
        if !in_escape {
            out.push(c);
        }
    }
    out
}

/// One PDFDocEncoding byte (Annex D.2).
pub fn pdf_doc_char(b: u8) -> char {
    const LOW: [char; 8] = [
        '\u{02d8}', '\u{02c7}', '\u{02c6}', '\u{02d9}', '\u{02dd}', '\u{02db}', '\u{02da}',
        '\u{02dc}',
    ];
    const HIGH: [char; 33] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}',
        '\u{2044}', '\u{2039}', '\u{203a}', '\u{2212}', '\u{2030}', '\u{201e}', '\u{201c}',
        '\u{201d}', '\u{2018}', '\u{2019}', '\u{201a}', '\u{2122}', '\u{fb01}', '\u{fb02}',
        '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017d}', '\u{0131}', '\u{0142}',
        '\u{0153}', '\u{0161}', '\u{017e}', '\u{fffd}', '\u{20ac}',
    ];
    match b {
        0x18..=0x1f => LOW
            .get(usize::from(b - 0x18))
            .copied()
            .unwrap_or('\u{fffd}'),
        0x7f | 0xad => '\u{fffd}',
        0x80..=0xa0 => HIGH
            .get(usize::from(b - 0x80))
            .copied()
            .unwrap_or('\u{fffd}'),
        _ => char::from(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_doc_encoding() {
        assert_eq!(decode_text_string(b"Hello"), "Hello");
        assert_eq!(decode_text_string(&[0x80, 0x92, 0xa0, 0xe9]), "•™€é");
        assert_eq!(decode_text_string(&[0x18, 0x1f]), "˘˜");
        assert_eq!(
            decode_text_string(&[0x7f, 0x9f, 0xad]),
            "\u{fffd}\u{fffd}\u{fffd}"
        );
    }

    #[test]
    fn utf16_be_and_le() {
        assert_eq!(
            decode_text_string(&[0xfe, 0xff, 0x00, 0x41, 0x04, 0x14]),
            "AД"
        );
        assert_eq!(decode_text_string(&[0xff, 0xfe, 0x41, 0x00]), "A");
        // A surrogate pair.
        assert_eq!(
            decode_text_string(&[0xfe, 0xff, 0xd8, 0x3d, 0xde, 0x00]),
            "😀"
        );
        // A lone surrogate and an odd trailing byte.
        assert_eq!(decode_text_string(&[0xfe, 0xff, 0xd8, 0x3d]), "\u{fffd}");
        assert_eq!(
            decode_text_string(&[0xfe, 0xff, 0x00, 0x41, 0x00]),
            "A\u{0}"
        );
    }

    #[test]
    fn language_escape_is_dropped() {
        let bytes = [
            0xfe, 0xff, 0x00, 0x1b, 0x00, b'e', 0x00, b'n', 0x00, 0x1b, 0x00, b'H',
        ];
        assert_eq!(decode_text_string(&bytes), "H");
    }

    #[test]
    fn encoding_round_trips() {
        for text in [
            "Hello",
            "café €",
            "Titre Δ",
            "日本語",
            "",
            "þÿ",
            "\u{1f600}",
        ] {
            assert_eq!(
                decode_text_string(&encode_text_string(text)),
                text,
                "{text}"
            );
        }
        assert_eq!(encode_text_string("A€"), vec![b'A', 0xa0]);
        assert_eq!(&encode_text_string("Δ")[..2], &[0xfe, 0xff]);
    }

    #[test]
    fn utf8_bom() {
        assert_eq!(decode_text_string("\u{feff}ünï".as_bytes()), "ünï");
    }
}
