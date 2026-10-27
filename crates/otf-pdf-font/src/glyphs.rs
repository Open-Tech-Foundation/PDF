//! Glyph names to Unicode (the Adobe Glyph List conventions), encodings by
//! name, and the standard 14 fonts' widths.

use crate::tables::{
    DINGBATS_GLYPH_LIST, GLYPH_LIST, MAC_EXPERT_ENCODING, MAC_ROMAN_ENCODING, STANDARD_14_WIDTHS,
    STANDARD_ENCODING, STANDARD_FONT_ALIASES, SYMBOL_ENCODING, WIN_ANSI_ENCODING, Widths,
    ZAPF_DINGBATS_ENCODING,
};

fn lookup<'a>(table: &'a [(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    table
        .binary_search_by(|(k, _)| k.as_bytes().cmp(name.as_bytes()))
        .ok()
        .and_then(|i| table.get(i))
        .map(|(_, v)| *v)
        // `.notdef` maps to U+0000: no character.
        .filter(|v| *v != "\0")
}

/// The Unicode text of a glyph name: the glyph list, `uniXXXX…` and
/// `uXXXX[XX]` names, `name.suffix` variants and `a_b` ligatures.
pub fn glyph_to_unicode(name: &str, dingbats: bool) -> Option<String> {
    if dingbats && let Some(s) = lookup(DINGBATS_GLYPH_LIST, name) {
        return Some(s.to_owned());
    }
    if let Some(s) = lookup(GLYPH_LIST, name) {
        return Some(s.to_owned());
    }
    // "a.sc", "one.oldstyle": the part before the first period.
    if let Some((base, _)) = name.split_once('.') {
        if !base.is_empty() {
            return glyph_to_unicode(base, dingbats);
        }
        return None;
    }
    // "f_f_i": each component.
    if name.contains('_') {
        return name
            .split('_')
            .map(|part| glyph_to_unicode(part, dingbats))
            .collect::<Option<String>>();
    }
    let hex_chars = |hex: &str| -> Option<String> {
        if hex.is_empty()
            || !hex.len().is_multiple_of(4)
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        let units: Vec<u16> = (0..hex.len() / 4)
            .map(|i| u16::from_str_radix(hex.get(i * 4..i * 4 + 4).unwrap_or("0"), 16).unwrap_or(0))
            .collect();
        char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .ok()
    };
    if let Some(hex) = name.strip_prefix("uni") {
        return hex_chars(hex);
    }
    if let Some(hex) = name.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .map(String::from);
    }
    None
}

/// A predefined encoding by its name.
pub fn encoding_by_name(name: &[u8]) -> Option<&'static [&'static str; 256]> {
    match name {
        b"StandardEncoding" => Some(&STANDARD_ENCODING),
        b"WinAnsiEncoding" => Some(&WIN_ANSI_ENCODING),
        b"MacRomanEncoding" => Some(&MAC_ROMAN_ENCODING),
        b"MacExpertEncoding" => Some(&MAC_EXPERT_ENCODING),
        b"SymbolSetEncoding" => Some(&SYMBOL_ENCODING),
        b"ZapfDingbatsEncoding" => Some(&ZAPF_DINGBATS_ENCODING),
        _ => None,
    }
}

/// The standard 14 font a font name stands for, if any: the name itself,
/// an alias (Arial → Helvetica), or a style-suffixed form of one.
pub fn standard_font(name: &str) -> Option<&'static str> {
    let find = |n: &str| -> Option<&'static str> {
        STANDARD_14_WIDTHS
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(k, _)| *k)
            .or_else(|| lookup(STANDARD_FONT_ALIASES, n))
    };
    find(name).or_else(|| find(&name.replace(',', "-")))
}

/// The width of `glyph` in a standard 14 font, in 1/1000 em.
pub fn standard_width(font: &str, glyph: &str) -> Option<f64> {
    let (_, widths) = STANDARD_14_WIDTHS.iter().find(|(k, _)| *k == font)?;
    match widths {
        Widths::Fixed(w) => Some(f64::from(*w)),
        Widths::Table(table) => table
            .binary_search_by(|(k, _)| k.as_bytes().cmp(glyph.as_bytes()))
            .ok()
            .and_then(|i| table.get(i))
            .map(|(_, w)| f64::from(*w)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names() {
        assert_eq!(glyph_to_unicode("A", false).as_deref(), Some("A"));
        assert_eq!(glyph_to_unicode("eacute", false).as_deref(), Some("é"));
        assert_eq!(glyph_to_unicode("Euro", false).as_deref(), Some("€"));
        assert_eq!(glyph_to_unicode("uni0041", false).as_deref(), Some("A"));
        assert_eq!(
            glyph_to_unicode("uni00410042", false).as_deref(),
            Some("AB")
        );
        assert_eq!(glyph_to_unicode("u1F600", false).as_deref(), Some("😀"));
        assert_eq!(glyph_to_unicode("a.sc", false).as_deref(), Some("a"));
        assert_eq!(glyph_to_unicode("T_h", false).as_deref(), Some("Th"));
        assert_eq!(
            glyph_to_unicode("f_f_i", false).as_deref(),
            Some("\u{fb03}")
        );
        assert_eq!(glyph_to_unicode("a1", true).as_deref(), Some("✁"));
        assert_eq!(glyph_to_unicode("g123", false), None);
        assert_eq!(glyph_to_unicode(".notdef", false), None);
        assert_eq!(glyph_to_unicode("uniZZZZ", false), None);
    }

    #[test]
    fn encodings_and_standard_fonts() {
        assert_eq!(encoding_by_name(b"WinAnsiEncoding").unwrap()[0x80], "Euro");
        assert_eq!(encoding_by_name(b"StandardEncoding").unwrap()[0x41], "A");
        assert!(encoding_by_name(b"Bogus").is_none());
        assert_eq!(standard_font("Helvetica"), Some("Helvetica"));
        assert_eq!(standard_font("Arial"), Some("Helvetica"));
        assert_eq!(standard_font("Arial,Bold"), Some("Helvetica-Bold"));
        assert_eq!(standard_font("NotAFont"), None);
        assert_eq!(standard_width("Helvetica", "A"), Some(667.0));
        assert_eq!(standard_width("Courier", "anything"), Some(600.0));
        assert_eq!(standard_width("Helvetica", "nosuch"), None);
    }
}
