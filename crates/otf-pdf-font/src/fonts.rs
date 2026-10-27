//! Loading fonts from font dictionaries (ISO 32000-2 §9.6, §9.7): simple
//! fonts (Type1, TrueType, Type3) and composite fonts (Type0 with a CIDFont).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use otf_pdf_content::{Font, FontLoader};
use otf_pdf_doc::Store;
use otf_pdf_syntax::{Dict, ObjId, Object};
use skrifa::{FontRef, MetadataProvider};

use crate::cmap::CMap;
use crate::glyphs::{encoding_by_name, glyph_to_unicode, standard_font, standard_width};
use crate::tables::{
    STANDARD_ENCODING, SYMBOL_ENCODING, WIN_ANSI_ENCODING, ZAPF_DINGBATS_ENCODING,
};

/// A font name without its subset tag (`ABCDEF+Name` → `Name`).
fn base_name(dict: &Dict) -> String {
    let raw = dict.get_name(b"BaseFont").unwrap_or_default();
    let name = String::from_utf8_lossy(raw).into_owned();
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) => {
            rest.to_owned()
        }
        _ => name,
    }
}

/// The decoded bytes of an embedded font program, if any.
fn font_file(store: &Store, descriptor: Option<&Dict>) -> Option<(Vec<u8>, &'static [u8])> {
    let descriptor = descriptor?;
    for key in [&b"FontFile2"[..], b"FontFile3", b"FontFile"] {
        if let Some(Object::Stream(s)) = descriptor.get(key).map(|f| store.resolve(f)) {
            let data = store.decode_stream(&s).ok()?.data;
            let kind: &'static [u8] = match key {
                b"FontFile2" => b"TrueType",
                b"FontFile" => b"Type1",
                _ => match s.dict.get_name(b"Subtype") {
                    Some(b"OpenType") => b"OpenType",
                    _ => b"CFF",
                },
            };
            return Some((data, kind));
        }
    }
    None
}

/// Glyph id → Unicode from an embedded TrueType/OpenType font's cmap.
fn reverse_cmap(data: &[u8]) -> HashMap<u32, char> {
    let mut out = HashMap::new();
    if let Ok(font) = FontRef::new(data) {
        for (ch, gid) in font.charmap().mappings() {
            if let Some(c) = char::from_u32(ch) {
                out.entry(u32::from(gid)).or_insert(c);
            }
        }
    }
    out
}

/// A simple font: one byte per code.
#[derive(Debug)]
struct SimpleFont {
    name: String,
    names: Vec<Option<String>>,
    widths: Vec<f64>,
    to_unicode: Option<CMap>,
    /// Code → Unicode from the embedded font's own cmap.
    program_unicode: HashMap<u32, char>,
    dingbats: bool,
    symbolic: bool,
}

impl Font for SimpleFont {
    fn codes(&self, bytes: &[u8]) -> Vec<(u32, usize)> {
        bytes.iter().map(|&b| (u32::from(b), 1)).collect()
    }

    fn width(&self, code: u32) -> f64 {
        self.widths.get(code as usize).copied().unwrap_or(0.0)
    }

    fn unicode(&self, code: u32) -> Option<String> {
        if let Some(s) = self.to_unicode.as_ref().and_then(|c| c.unicode(code)) {
            return Some(s);
        }
        if let Some(s) = self
            .names
            .get(code as usize)
            .and_then(Option::as_ref)
            .and_then(|n| glyph_to_unicode(n, self.dingbats))
        {
            return Some(s);
        }
        if let Some(c) = self.program_unicode.get(&code) {
            return Some(c.to_string());
        }
        // Nothing better: printable codes read as Latin-1 (what most simple
        // fonts without an encoding mean).
        let c = char::from_u32(code)?;
        (!self.symbolic && (' '..='~').contains(&c) || (!self.symbolic && code >= 0xa0))
            .then(|| c.to_string())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// A composite (Type0) font: a CMap and a CIDFont.
#[derive(Debug)]
struct CompositeFont {
    name: String,
    cmap: CMap,
    to_unicode: Option<CMap>,
    /// (first CID, last CID, width), from `/W`.
    widths: Vec<(u32, u32, f64)>,
    default_width: f64,
    /// (CID, vertical advance), from `/W2`, and `/DW2`'s advance.
    vertical_widths: Vec<(u32, u32, f64)>,
    default_vertical: f64,
    /// CID → Unicode via the embedded font's cmap (for Identity mappings).
    program_unicode: HashMap<u32, char>,
    cid_to_gid: Option<Vec<u16>>,
    /// CID → Unicode for an Adobe CJK collection (`/CIDSystemInfo`).
    collection: Option<&'static [(u32, u32, u16)]>,
}

/// The code point for `cid` in a collection table.
fn collection_unicode(table: &[(u32, u32, u16)], cid: u32) -> Option<char> {
    let i = table
        .partition_point(|(start, _, _)| *start <= cid)
        .checked_sub(1)?;
    let (start, cp, len) = *table.get(i)?;
    (cid < start + u32::from(len))
        .then(|| char::from_u32(cp + (cid - start)))
        .flatten()
}

impl Font for CompositeFont {
    fn codes(&self, bytes: &[u8]) -> Vec<(u32, usize)> {
        self.cmap.codes(bytes)
    }

    fn width(&self, code: u32) -> f64 {
        let Some(cid) = self.cmap.cid(code) else {
            return self.default_width / 1000.0;
        };
        self.widths
            .iter()
            .rev()
            .find(|(lo, hi, _)| *lo <= cid && cid <= *hi)
            .map_or(self.default_width, |(_, _, w)| *w)
            / 1000.0
    }

    fn is_vertical(&self) -> bool {
        self.cmap.vertical
    }

    fn vertical_advance(&self, code: u32) -> f64 {
        let cid = self.cmap.cid(code).unwrap_or(code);
        self.vertical_widths
            .iter()
            .rev()
            .find(|(lo, hi, _)| *lo <= cid && cid <= *hi)
            .map_or(self.default_vertical, |(_, _, w)| *w)
            / 1000.0
    }

    fn unicode(&self, code: u32) -> Option<String> {
        if let Some(s) = self.to_unicode.as_ref().and_then(|c| c.unicode(code)) {
            return Some(s);
        }
        if let Some(s) = self.cmap.unicode(code) {
            return Some(s);
        }
        let cid = self.cmap.cid(code)?;
        if let Some(c) = self.collection.and_then(|t| collection_unicode(t, cid)) {
            return Some(c.to_string());
        }
        let gid = match &self.cid_to_gid {
            Some(map) => u32::from(*map.get(cid as usize)?),
            None => cid,
        };
        self.program_unicode.get(&gid).map(|c| c.to_string())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

fn number(o: &Object) -> Option<f64> {
    o.as_f64().filter(|v| v.is_finite())
}

/// Parses `/W` (`c [w1 w2 …]` and `cfirst clast w`) or, with `triple`,
/// `/W2` (`c [w1y vx vy …]`, `cfirst clast w1y vx vy`), keeping the first
/// number of each entry.
fn cid_widths(store: &Store, array: &[Object], triple: bool) -> Vec<(u32, u32, f64)> {
    let step = if triple { 3 } else { 1 };
    let mut out = Vec::new();
    let mut i = 0;
    while i < array.len() && out.len() < 1 << 20 {
        let Some(first) = array.get(i).and_then(Object::as_int) else {
            break;
        };
        let first = first.clamp(0, i64::from(u32::MAX)) as u32;
        match array.get(i + 1).map(|o| store.resolve(o)) {
            Some(Object::Array(ws)) => {
                for (k, chunk) in ws.chunks(step).enumerate() {
                    if let Some(w) = chunk.first().and_then(number) {
                        let cid = first.saturating_add(k as u32);
                        out.push((cid, cid, w));
                    }
                }
                i += 2;
            }
            Some(last) => {
                let last = last.as_int().unwrap_or(0).clamp(0, i64::from(u32::MAX)) as u32;
                if let Some(w) = array.get(i + 2).and_then(number) {
                    out.push((first, last, w));
                }
                i += 2 + step;
            }
            None => break,
        }
    }
    out
}

fn load_simple(store: &Store, dict: &Dict) -> SimpleFont {
    let name = base_name(dict);
    let subtype = dict.get_name(b"Subtype").unwrap_or_default();
    let descriptor = dict
        .get(b"FontDescriptor")
        .and_then(|d| store.resolve_dict(d));
    let flags = descriptor
        .as_ref()
        .and_then(|d| d.get_int(b"Flags"))
        .unwrap_or(0);
    let standard = standard_font(&name);
    let symbol_like = matches!(standard, Some("Symbol" | "ZapfDingbats"));
    let symbolic = (flags & 4 != 0 && flags & 32 == 0) || symbol_like;
    let program = font_file(store, descriptor.as_ref());

    // Base encoding: named, built into the program, or the default.
    let encoding = dict.get(b"Encoding").map(|e| store.resolve(e));
    let named_base = match &encoding {
        Some(Object::Name(n)) => encoding_by_name(n.as_bytes()),
        Some(Object::Dict(d)) => d.get_name(b"BaseEncoding").and_then(encoding_by_name),
        _ => None,
    };
    let builtin = match &program {
        Some((data, b"Type1")) => crate::type1::builtin_encoding(data),
        _ => None,
    };
    let mut names: Vec<Option<String>> = vec![None; 256];
    let fallback: &[&str; 256] = match standard {
        Some("Symbol") => &SYMBOL_ENCODING,
        Some("ZapfDingbats") => &ZAPF_DINGBATS_ENCODING,
        _ if subtype == b"TrueType" && !symbolic => &WIN_ANSI_ENCODING,
        _ => &STANDARD_ENCODING,
    };
    for (code, slot) in names.iter_mut().enumerate() {
        let from_builtin = builtin
            .as_ref()
            .filter(|_| named_base.is_none())
            .and_then(|b| b.get(code).cloned().flatten());
        let base = named_base.unwrap_or(fallback);
        *slot = from_builtin.or_else(|| {
            base.get(code)
                .filter(|n| !n.is_empty())
                .map(|n| (*n).to_owned())
        });
        // Codes StandardEncoding leaves empty usually mean WinAnsi.
        if slot.is_none() && named_base.is_none() && builtin.is_none() && !symbolic {
            *slot = WIN_ANSI_ENCODING
                .get(code)
                .filter(|n| !n.is_empty())
                .map(|n| (*n).to_owned());
        }
    }
    if let Some(Object::Dict(enc)) = &encoding
        && let Some(diffs) = enc.get_array(b"Differences")
    {
        let mut code = 0usize;
        for item in diffs {
            match item {
                Object::Int(n) => code = (*n).clamp(0, 255) as usize,
                Object::Name(n) => {
                    if let Some(slot) = names.get_mut(code) {
                        *slot = Some(String::from_utf8_lossy(n.as_bytes()).into_owned());
                    }
                    code += 1;
                }
                _ => {}
            }
        }
    }

    // Widths: /Widths, else the standard 14 metrics, else /MissingWidth.
    let font_matrix_scale = if subtype == b"Type3" {
        dict.get_array(b"FontMatrix")
            .and_then(|m| m.first())
            .and_then(number)
            .unwrap_or(0.001)
    } else {
        0.001
    };
    let missing = descriptor
        .as_ref()
        .and_then(|d| d.get(b"MissingWidth"))
        .and_then(number)
        .unwrap_or(0.0);
    let first = dict.get_int(b"FirstChar").unwrap_or(0).clamp(0, 255) as usize;
    let declared = dict.get(b"Widths").map(|w| store.resolve(w));
    let mut widths = vec![missing * font_matrix_scale; 256];
    match declared.as_ref().and_then(Object::as_array) {
        Some(ws) => {
            for (i, w) in ws.iter().enumerate() {
                if let (Some(slot), Some(w)) =
                    (widths.get_mut(first + i), number(&store.resolve(w)))
                {
                    *slot = w * font_matrix_scale;
                }
            }
        }
        None => {
            let std = standard.unwrap_or("Helvetica");
            for (code, slot) in widths.iter_mut().enumerate() {
                if let Some(w) = names
                    .get(code)
                    .and_then(Option::as_ref)
                    .and_then(|n| standard_width(std, n))
                {
                    *slot = w / 1000.0;
                } else if *slot == 0.0 {
                    *slot = 0.5;
                }
            }
        }
    }

    // The embedded TrueType's own cmap, for symbolic fonts without names.
    let program_unicode = match &program {
        Some((data, b"TrueType" | b"OpenType")) => {
            let by_gid = reverse_cmap(data);
            let mut out = HashMap::new();
            if let Ok(font) = FontRef::new(data) {
                let charmap = font.charmap();
                for code in 0u32..256 {
                    let gid = charmap.map(code).or_else(|| charmap.map(0xf000 + code));
                    if let Some(c) = gid.and_then(|g| by_gid.get(&u32::from(g)))
                        && u32::from(*c) != code + 0xf000
                    {
                        out.insert(code, *c);
                    }
                }
            }
            out
        }
        _ => HashMap::new(),
    };

    SimpleFont {
        dingbats: standard == Some("ZapfDingbats"),
        name,
        names,
        widths,
        to_unicode: to_unicode(store, dict),
        program_unicode,
        symbolic,
    }
}

fn to_unicode(store: &Store, dict: &Dict) -> Option<CMap> {
    match dict.get(b"ToUnicode").map(|t| store.resolve(t)) {
        Some(Object::Stream(s)) => {
            let cmap = CMap::parse(&store.decode_stream(&s).ok()?.data);
            cmap.has_unicode().then_some(cmap)
        }
        // `/Identity-H` as a ToUnicode name: codes are UTF-16.
        Some(Object::Name(n)) if n.as_bytes().starts_with(b"Identity") => {
            Some(CMap::predefined(b"UniGB-UCS2-H"))
        }
        _ => None,
    }
}

fn load_composite(store: &Store, dict: &Dict) -> CompositeFont {
    let cmap = match dict.get(b"Encoding").map(|e| store.resolve(e)) {
        Some(Object::Name(n)) => CMap::predefined(n.as_bytes()),
        Some(Object::Stream(s)) => {
            let parsed = store
                .decode_stream(&s)
                .map(|d| CMap::parse(&d.data))
                .unwrap_or_default();
            if parsed.has_codespace() {
                parsed
            } else {
                CMap::predefined(b"Identity-H")
            }
        }
        _ => CMap::predefined(b"Identity-H"),
    };
    let descendant = dict
        .get(b"DescendantFonts")
        .map(|d| store.resolve(d))
        .and_then(|d| d.as_array().and_then(|a| a.first().cloned()))
        .and_then(|d| store.resolve_dict(&d))
        .unwrap_or_default();
    let descriptor = descendant
        .get(b"FontDescriptor")
        .and_then(|d| store.resolve_dict(d));
    let widths = descendant
        .get(b"W")
        .map(|w| store.resolve(w))
        .and_then(|w| w.as_array().map(|a| cid_widths(store, a, false)))
        .unwrap_or_default();
    let vertical_widths = descendant
        .get(b"W2")
        .map(|w| store.resolve(w))
        .and_then(|w| w.as_array().map(|a| cid_widths(store, a, true)))
        .unwrap_or_default();
    let default_vertical = descendant
        .get_array(b"DW2")
        .and_then(|a| a.get(1))
        .and_then(number)
        .unwrap_or(-1000.0);
    let cid_to_gid = match descendant.get(b"CIDToGIDMap").map(|m| store.resolve(m)) {
        Some(Object::Stream(s)) => store.decode_stream(&s).ok().map(|d| {
            d.data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| {
                    u16::from_be_bytes([
                        c.first().copied().unwrap_or(0),
                        c.get(1).copied().unwrap_or(0),
                    ])
                })
                .collect()
        }),
        _ => None,
    };
    let program_unicode = match font_file(store, descriptor.as_ref()) {
        Some((data, b"TrueType" | b"OpenType")) => reverse_cmap(&data),
        _ => HashMap::new(),
    };
    let collection = descendant
        .get(b"CIDSystemInfo")
        .and_then(|i| store.resolve_dict(i))
        .and_then(
            |info| match info.get(b"Ordering").map(|o| store.resolve(o)) {
                Some(Object::String(s)) => Some(s.bytes),
                _ => None,
            },
        )
        .and_then(|ordering| match ordering.as_slice() {
            b"Japan1" => Some(crate::cid_tables::JAPAN1),
            b"GB1" => Some(crate::cid_tables::GB1),
            b"CNS1" => Some(crate::cid_tables::CNS1),
            b"Korea1" | b"KR" => Some(crate::cid_tables::KOREA1),
            _ => None,
        });
    CompositeFont {
        collection,
        name: base_name(dict),
        cmap,
        to_unicode: to_unicode(store, dict),
        widths,
        default_width: descendant.get(b"DW").and_then(number).unwrap_or(1000.0),
        vertical_widths,
        default_vertical,
        program_unicode,
        cid_to_gid,
    }
}

/// Loads a font from its dictionary.
pub fn load_font(store: &Store, dict: &Dict) -> Arc<dyn Font> {
    match dict.get_name(b"Subtype") {
        Some(b"Type0") => Arc::new(load_composite(store, dict)),
        _ => Arc::new(load_simple(store, dict)),
    }
}

/// A [`FontLoader`] that caches fonts by object id, so a font used on many
/// pages is loaded once.
#[derive(Debug, Default)]
pub struct FontCache {
    fonts: Mutex<HashMap<ObjId, Arc<dyn Font>>>,
}

impl FontCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FontLoader for FontCache {
    fn load(&self, store: &Store, font: &Object) -> Option<Arc<dyn Font>> {
        let id = font.as_ref();
        if let Some(id) = id
            && let Some(f) = self.fonts.lock().ok().and_then(|m| m.get(&id).cloned())
        {
            return Some(f);
        }
        let dict = store.resolve_dict(font)?;
        let loaded = load_font(store, &dict);
        if let (Some(id), Ok(mut m)) = (id, self.fonts.lock()) {
            m.insert(id, Arc::clone(&loaded));
        }
        Some(loaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collections() {
        // Adobe-Japan1 CID 843 is HIRAGANA LETTER A (U+3042).
        assert_eq!(
            collection_unicode(crate::cid_tables::JAPAN1, 843),
            Some('あ')
        );
        // CID 34 is "A" in Adobe-Japan1.
        assert_eq!(collection_unicode(crate::cid_tables::JAPAN1, 34), Some('A'));
        assert_eq!(
            collection_unicode(crate::cid_tables::JAPAN1, u32::MAX),
            None
        );
        assert_eq!(collection_unicode(&[], 5), None);
    }
}
