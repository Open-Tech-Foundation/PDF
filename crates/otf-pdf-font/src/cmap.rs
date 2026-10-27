//! CMaps (ISO 32000-2 §9.7.5) and ToUnicode CMaps (§9.10.3): how strings
//! split into codes, codes to CIDs, and codes to Unicode.
//!
//! Ranges are kept as ranges, not expanded, so a hostile `<0000> <FFFF>`
//! costs one entry.

use otf_pdf_syntax::lexer::{Lexer, Token};

/// A parsed CMap.
#[derive(Debug, Clone, Default)]
pub struct CMap {
    /// Code space ranges: (low, high) of equal length.
    codespace: Vec<(Vec<u8>, Vec<u8>)>,
    /// (first code, last code, first CID).
    cid_ranges: Vec<(u32, u32, u32)>,
    /// (first code, last code, destination).
    bf_ranges: Vec<(u32, u32, Dest)>,
    /// Writing mode 1: vertical.
    pub vertical: bool,
}

#[derive(Debug, Clone)]
enum Dest {
    /// UTF-16 units; the last is incremented across the range.
    Base(Vec<u16>),
    /// One string per code.
    Array(Vec<Vec<u16>>),
}

/// Entries kept from one CMap; more is ignored.
const MAX_ENTRIES: usize = 1 << 20;

fn bytes_to_u32(b: &[u8]) -> u32 {
    b.iter().take(4).fold(0, |acc, &x| acc << 8 | u32::from(x))
}

fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    if bytes.len() == 1 {
        return vec![u16::from(bytes.first().copied().unwrap_or(0))];
    }
    bytes
        .chunks(2)
        .map(|c| {
            u16::from_be_bytes([
                c.first().copied().unwrap_or(0),
                c.get(1).copied().unwrap_or(0),
            ])
        })
        .collect()
}

fn units_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

impl CMap {
    /// Parses a CMap or ToUnicode stream's decoded bytes.
    pub fn parse(data: &[u8]) -> CMap {
        let mut cmap = CMap::default();
        let mut lexer = Lexer::new(data);
        let mut operands: Vec<Token<'_>> = Vec::new();
        let mut section: Option<&[u8]> = None;
        let mut entries = 0usize;
        loop {
            let token = match lexer.next_token() {
                Ok(Token::Eof) | Err(_) => break,
                Ok(t) => t,
            };
            match token {
                Token::Keyword(
                    k @ (b"begincodespacerange"
                    | b"begincidrange"
                    | b"begincidchar"
                    | b"beginbfrange"
                    | b"beginbfchar"),
                ) => {
                    section = Some(k);
                    operands.clear();
                }
                Token::Keyword(
                    b"endcodespacerange" | b"endcidrange" | b"endcidchar" | b"endbfrange"
                    | b"endbfchar",
                ) => {
                    section = None;
                    operands.clear();
                }
                Token::Keyword(b"def") => {
                    if let [Token::Name(n), Token::Int(v)] = operands.as_slice()
                        && n.as_bytes() == b"WMode"
                    {
                        cmap.vertical = *v == 1;
                    }
                    operands.clear();
                }
                Token::ArrayStart if section == Some(b"beginbfrange") => {
                    // An array destination: read its strings now.
                    let mut items = Vec::new();
                    loop {
                        match lexer.next_token() {
                            Ok(Token::String(s)) => items.push(utf16_units(&s.bytes)),
                            Ok(Token::ArrayEnd | Token::Eof) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                    if let [Token::String(lo), Token::String(hi)] = operands.as_slice()
                        && entries < MAX_ENTRIES
                    {
                        cmap.bf_ranges.push((
                            bytes_to_u32(&lo.bytes),
                            bytes_to_u32(&hi.bytes),
                            Dest::Array(items),
                        ));
                        entries += 1;
                    }
                    operands.clear();
                }
                t => {
                    operands.push(t);
                    let want = match section {
                        Some(b"begincodespacerange" | b"beginbfchar" | b"begincidchar") => 2,
                        Some(b"begincidrange" | b"beginbfrange") => 3,
                        _ => {
                            if operands.len() > 8 {
                                operands.remove(0);
                            }
                            continue;
                        }
                    };
                    if operands.len() < want {
                        continue;
                    }
                    if entries >= MAX_ENTRIES {
                        operands.clear();
                        continue;
                    }
                    entries += 1;
                    match (section, operands.as_slice()) {
                        (Some(b"begincodespacerange"), [Token::String(lo), Token::String(hi)]) => {
                            if lo.bytes.len() == hi.bytes.len() && (1..=4).contains(&lo.bytes.len())
                            {
                                cmap.codespace.push((lo.bytes.clone(), hi.bytes.clone()));
                            }
                        }
                        (Some(b"begincidchar"), [Token::String(code), Token::Int(cid)]) => {
                            let c = bytes_to_u32(&code.bytes);
                            cmap.cid_ranges.push((
                                c,
                                c,
                                (*cid).clamp(0, i64::from(u32::MAX)) as u32,
                            ));
                        }
                        (
                            Some(b"begincidrange"),
                            [Token::String(lo), Token::String(hi), Token::Int(cid)],
                        ) => {
                            cmap.cid_ranges.push((
                                bytes_to_u32(&lo.bytes),
                                bytes_to_u32(&hi.bytes),
                                (*cid).clamp(0, i64::from(u32::MAX)) as u32,
                            ));
                        }
                        (Some(b"beginbfchar"), [Token::String(code), Token::String(dst)]) => {
                            let c = bytes_to_u32(&code.bytes);
                            cmap.bf_ranges
                                .push((c, c, Dest::Base(utf16_units(&dst.bytes))));
                        }
                        (Some(b"beginbfchar"), [Token::String(code), Token::Name(glyph)]) => {
                            let name = String::from_utf8_lossy(glyph.as_bytes());
                            if let Some(s) = crate::glyphs::glyph_to_unicode(&name, false) {
                                let c = bytes_to_u32(&code.bytes);
                                cmap.bf_ranges
                                    .push((c, c, Dest::Base(s.encode_utf16().collect())));
                            }
                        }
                        (
                            Some(b"beginbfrange"),
                            [Token::String(lo), Token::String(hi), Token::String(dst)],
                        ) => {
                            cmap.bf_ranges.push((
                                bytes_to_u32(&lo.bytes),
                                bytes_to_u32(&hi.bytes),
                                Dest::Base(utf16_units(&dst.bytes)),
                            ));
                        }
                        _ => {}
                    }
                    operands.clear();
                }
            }
        }
        // Later entries win; searching from the back gives that.
        cmap
    }

    /// The predefined CMap `name` (§9.7.5.2), where this crate can build it
    /// without data files: `Identity-H/V` and the Unicode (`UCS2`, `UTF16`)
    /// ones. Others need their CMap data and ToUnicode for text; they get a
    /// best-effort one- or two-byte code space.
    pub fn predefined(name: &[u8]) -> CMap {
        let vertical = name.ends_with(b"-V");
        let two_byte = || vec![(vec![0x00, 0x00], vec![0xff, 0xff])];
        let mut cmap = CMap {
            vertical,
            ..CMap::default()
        };
        let is_unicode =
            name.windows(4).any(|w| w == b"UCS2") || name.windows(5).any(|w| w == b"UTF16");
        if name.starts_with(b"Identity-") || is_unicode {
            cmap.codespace = two_byte();
            if is_unicode {
                // The code is Unicode, not a CID: widths fall back to /DW.
                cmap.bf_ranges.push((0, 0xffff, Dest::Base(vec![0])));
            } else {
                cmap.cid_ranges.push((0, 0xffff, 0));
            }
        } else {
            // Legacy multi-byte encodings (Shift-JIS, EUC, GBK, Big5): ASCII
            // is one byte, everything else two.
            cmap.codespace = vec![
                (vec![0x00], vec![0x80]),
                (vec![0x81, 0x00], vec![0xff, 0xff]),
            ];
            cmap.cid_ranges.push((0, 0xffff, 0));
        }
        cmap
    }

    /// Splits `bytes` into codes by the code space; a byte that starts no
    /// valid code is taken alone.
    pub fn codes(&self, bytes: &[u8]) -> Vec<(u32, usize)> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        let shortest = self
            .codespace
            .iter()
            .map(|(lo, _)| lo.len())
            .min()
            .unwrap_or(1);
        while i < bytes.len() {
            let rest = bytes.get(i..).unwrap_or_default();
            let len = (1..=4)
                .find(|&n| {
                    rest.len() >= n
                        && self.codespace.iter().any(|(lo, hi)| {
                            lo.len() == n
                                && rest
                                    .iter()
                                    .zip(lo.iter().zip(hi))
                                    .take(n)
                                    .all(|(b, (l, h))| l <= b && b <= h)
                        })
                })
                .unwrap_or(shortest.min(rest.len()).max(1));
            out.push((bytes_to_u32(rest.get(..len).unwrap_or_default()), len));
            i += len;
        }
        out
    }

    /// Whether the CMap defines a code space.
    pub fn has_codespace(&self) -> bool {
        !self.codespace.is_empty()
    }

    /// The CID for `code`.
    pub fn cid(&self, code: u32) -> Option<u32> {
        self.cid_ranges
            .iter()
            .rev()
            .find(|(lo, hi, _)| *lo <= code && code <= *hi)
            .map(|(lo, _, start)| start.wrapping_add(code - lo))
    }

    /// The Unicode text for `code`.
    pub fn unicode(&self, code: u32) -> Option<String> {
        let (lo, _, dest) = self
            .bf_ranges
            .iter()
            .rev()
            .find(|(lo, hi, _)| *lo <= code && code <= *hi)?;
        let offset = code - lo;
        match dest {
            Dest::Base(units) => {
                let mut units = units.clone();
                if let Some(last) = units.last_mut() {
                    // Unicode CMaps: the code itself, as one UTF-16 unit.
                    *last = last.wrapping_add(offset as u16);
                }
                if units.is_empty() {
                    return None;
                }
                Some(units_to_string(&units))
            }
            Dest::Array(items) => items.get(offset as usize).map(|u| units_to_string(u)),
        }
    }

    /// Whether any code maps to Unicode.
    pub fn has_unicode(&self) -> bool {
        !self.bf_ranges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TO_UNICODE: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
        /CMapName /Adobe-Identity-UCS def /WMode 0 def\n\
        1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
        2 beginbfchar <0003> <0020> <0024> <00660069> endbfchar\n\
        2 beginbfrange <0010> <0012> <0041> <0020> <0021> [<03B1> <D83DDE00>] endbfrange\n\
        endcmap CMapName currentdict /CMap defineresource pop end end";

    #[test]
    fn to_unicode() {
        let c = CMap::parse(TO_UNICODE);
        assert_eq!(c.unicode(0x03).as_deref(), Some(" "));
        assert_eq!(c.unicode(0x24).as_deref(), Some("fi"));
        assert_eq!(c.unicode(0x10).as_deref(), Some("A"));
        assert_eq!(c.unicode(0x12).as_deref(), Some("C"));
        assert_eq!(c.unicode(0x20).as_deref(), Some("α"));
        assert_eq!(c.unicode(0x21).as_deref(), Some("😀"));
        assert_eq!(c.unicode(0x99), None);
        assert_eq!(c.codes(b"\x00\x10\x00\x24"), vec![(0x10, 2), (0x24, 2)]);
        assert!(!c.vertical);
    }

    #[test]
    fn mixed_width_code_space_and_cids() {
        let c = CMap::parse(
            b"/WMode 1 def 2 begincodespacerange <00> <80> <8140> <9FFC> endcodespacerange\n\
              1 begincidrange <8140> <817E> 633 endcidrange 1 begincidchar <41> 34 endcidchar",
        );
        assert!(c.vertical);
        assert_eq!(
            c.codes(b"A\x81\x41B"),
            vec![(0x41, 1), (0x8141, 2), (0x42, 1)]
        );
        assert_eq!(c.cid(0x8141), Some(634));
        assert_eq!(c.cid(0x41), Some(34));
        assert_eq!(c.cid(0x42), None);
        // A byte that starts no code is taken alone.
        assert_eq!(c.codes(b"\xff"), vec![(0xff, 1)]);
    }

    #[test]
    fn predefined_cmaps() {
        let id = CMap::predefined(b"Identity-V");
        assert!(id.vertical);
        assert_eq!(id.codes(b"\x01\x02"), vec![(0x0102, 2)]);
        assert_eq!(id.cid(0x0102), Some(0x0102));
        assert_eq!(id.unicode(0x0102), None);
        let ucs = CMap::predefined(b"UniGB-UCS2-H");
        assert_eq!(ucs.unicode(0x4e2d).as_deref(), Some("中"));
        let sjis = CMap::predefined(b"90ms-RKSJ-H");
        assert_eq!(sjis.codes(b"a\x82\xa0"), vec![(0x61, 1), (0x82a0, 2)]);
    }

    #[test]
    fn garbage_does_not_panic() {
        let c =
            CMap::parse(b"beginbfrange <00> [ <> endbfrange begincidrange <> <> -5 endcidrange )");
        assert!(c.codes(b"").is_empty());
        let _ = c.unicode(0);
    }
}
