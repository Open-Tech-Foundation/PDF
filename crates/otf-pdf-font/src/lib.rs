//! Font layer: loads simple (Type1, TrueType, Type3) and composite (Type0)
//! fonts from their dictionaries and implements the content layer's
//! [`otf_pdf_content::Font`]: code splitting through encodings and CMaps,
//! widths (including the standard 14 fonts' metrics), and Unicode through
//! ToUnicode, glyph names or the embedded font's own cmap.
#![forbid(unsafe_code)]

#[allow(missing_docs)]
mod cid_tables;
mod cmap;
mod fonts;
mod glyphs;
#[allow(missing_docs)]
mod tables;
mod type1;

pub use cmap::CMap;
pub use fonts::{FontCache, load_font};
pub use glyphs::{encoding_by_name, glyph_to_unicode, standard_font, standard_width};
