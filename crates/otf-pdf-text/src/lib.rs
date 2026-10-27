//! Text extraction: runs each page through the content interpreter with a
//! device that collects glyphs, then groups them into lines and words.
//!
//! * Coordinates are in the page's displayed space: origin at the crop
//!   box's lower-left corner, `/Rotate` applied, y up, in points.
//! * Reading order is the content stream's, with glyphs that continue the
//!   same baseline joined into one line and gaps wider than a fifth of the
//!   font size read as spaces (like `pdftotext -raw`).
//! * `/ActualText` replaces the glyphs it covers; invisible text (rendering
//!   mode 3, as in OCR layers) is included.
#![forbid(unsafe_code)]

use otf_pdf_content::{Device, Glyph, GraphicsState, run_page};
use otf_pdf_doc::{Document, Page, Rect, decode_text_string};
use otf_pdf_font::FontCache;
use otf_pdf_syntax::{Dict, Error, Name, Result};

/// One character (or `/ActualText` replacement) as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct TextChar {
    /// The text: usually one character, sometimes a ligature's letters.
    pub text: String,
    /// Baseline start, in displayed page space.
    pub origin: (f64, f64),
    /// Baseline end (where the next character would start).
    pub end: (f64, f64),
    /// Font size in page space.
    pub size: f64,
    /// The font's name.
    pub font: String,
    /// Whether it is drawn invisibly (rendering mode 3).
    pub invisible: bool,
}

/// A word: characters between spaces on one line.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    /// The word's text.
    pub text: String,
    /// Its bounding box in displayed page space (approximated from the
    /// font size: 0.8 em above the baseline, 0.2 em below).
    pub bbox: Rect,
}

/// A line of text.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    /// The line's text, words separated by single spaces where the gaps
    /// call for them.
    pub text: String,
    /// The line's bounding box.
    pub bbox: Rect,
    /// The characters, in order.
    pub chars: Vec<TextChar>,
}

impl TextLine {
    /// The line's words, split at whitespace.
    pub fn words(&self) -> Vec<Word> {
        let mut words = Vec::new();
        let mut current: Vec<&TextChar> = Vec::new();
        let flush = |current: &mut Vec<&TextChar>, words: &mut Vec<Word>| {
            if !current.is_empty() {
                words.push(Word {
                    text: current.iter().map(|c| c.text.as_str()).collect(),
                    bbox: bounds(current.iter().copied()),
                });
                current.clear();
            }
        };
        for c in &self.chars {
            if c.text.trim().is_empty() || c.text == " " {
                flush(&mut current, &mut words);
            } else {
                current.push(c);
            }
        }
        flush(&mut current, &mut words);
        words
    }
}

/// The text of one page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageText {
    /// Lines in reading order.
    pub lines: Vec<TextLine>,
}

impl PageText {
    /// The page's text, one line per line.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&line.text);
            out.push('\n');
        }
        out
    }

    /// Every word on the page, in reading order.
    pub fn words(&self) -> Vec<Word> {
        self.lines.iter().flat_map(TextLine::words).collect()
    }
}

/// Extracts text from a document. Fonts are loaded once and shared across
/// pages.
pub struct TextExtractor<'a> {
    doc: &'a Document,
    fonts: FontCache,
}

impl<'a> TextExtractor<'a> {
    /// An extractor for `doc`.
    pub fn new(doc: &'a Document) -> Self {
        Self {
            doc,
            fonts: FontCache::new(),
        }
    }

    /// The text of page `index` (0-based), with positions.
    pub fn page(&self, index: usize) -> Result<PageText> {
        let pages = self.doc.pages()?;
        let page = pages
            .get(index)
            .ok_or(Error::syntax(0, "page index out of range"))?;
        let mut device = Collector::new(page);
        run_page(self.doc.store(), page, &self.fonts, &mut device)?;
        Ok(PageText {
            lines: build_lines(device.chars),
        })
    }

    /// The plain text of page `index`.
    pub fn page_text(&self, index: usize) -> Result<String> {
        self.page(index).map(|p| p.text())
    }

    /// The plain text of every page, pages separated by form feeds (`\x0c`)
    /// as `pdftotext` does. A page whose content cannot be read contributes
    /// nothing rather than failing the document.
    pub fn text(&self) -> Result<String> {
        let count = self.doc.page_count()?;
        let mut out = String::new();
        for i in 0..count {
            match self.page_text(i) {
                Ok(t) => out.push_str(&t),
                Err(e @ Error::Limit(_)) => return Err(e),
                Err(_) => {}
            }
            out.push('\x0c');
        }
        Ok(out)
    }
}

/// Maps page space to displayed space: crop box origin, then `/Rotate`.
#[derive(Debug, Clone, Copy)]
struct Display {
    crop: Rect,
    rotate: u16,
}

impl Display {
    fn point(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let Rect { llx, lly, urx, ury } = self.crop;
        match self.rotate {
            90 => (y - lly, urx - x),
            180 => (urx - x, ury - y),
            270 => (ury - y, x - llx),
            _ => (x - llx, y - lly),
        }
    }
}

/// The device collecting characters.
struct Collector {
    display: Display,
    chars: Vec<TextChar>,
    /// Marked content: `Some(text)` for an `/ActualText` span, and whether
    /// its character was emitted yet.
    marked: Vec<Option<(String, bool)>>,
}

impl Collector {
    fn new(page: &Page) -> Self {
        Self {
            display: Display {
                crop: page.crop_box,
                rotate: page.rotate,
            },
            chars: Vec::new(),
            marked: Vec::new(),
        }
    }
}

impl Device for Collector {
    fn draw_glyph(&mut self, glyph: &Glyph, state: &GraphicsState) {
        // Clipped away: not visible, not extracted (a field's text past its
        // box). Half a point of slack absorbs rounding.
        if !state.in_clip(glyph.origin, 0.5) {
            return;
        }
        let origin = self.display.point(glyph.origin);
        let end = self.display.point(glyph.end);
        // Inside /ActualText: the first glyph carries the replacement, the
        // rest only extend it.
        if let Some(Some((text, emitted))) = self.marked.iter_mut().rev().find(|m| m.is_some()) {
            if *emitted {
                if let Some(last) = self.chars.last_mut() {
                    last.end = end;
                }
                return;
            }
            *emitted = true;
            let text = text.clone();
            self.chars.push(TextChar {
                text,
                origin,
                end,
                size: glyph.size,
                font: glyph.font.name().to_owned(),
                invisible: glyph.render_mode == 3,
            });
            return;
        }
        let Some(text) = glyph.unicode.clone() else {
            return;
        };
        // Control characters carry no text.
        let text: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        if text.is_empty() {
            return;
        }
        self.chars.push(TextChar {
            text,
            origin,
            end,
            size: glyph.size,
            font: glyph.font.name().to_owned(),
            invisible: glyph.render_mode == 3,
        });
    }

    fn begin_marked_content(&mut self, _tag: &Name, properties: Option<&Dict>) {
        let actual = properties
            .and_then(|p| p.get(b"ActualText"))
            .and_then(|t| t.as_string())
            .map(decode_text_string);
        self.marked.push(actual.map(|t| (t, false)));
    }

    fn end_marked_content(&mut self) {
        self.marked.pop();
    }
}

fn bounds<'c>(chars: impl Iterator<Item = &'c TextChar>) -> Rect {
    let mut r = Rect {
        llx: f64::INFINITY,
        lly: f64::INFINITY,
        urx: f64::NEG_INFINITY,
        ury: f64::NEG_INFINITY,
    };
    for c in chars {
        let (dx, dy) = direction(c);
        // Up is the baseline direction turned left.
        let (ux, uy) = (-dy, dx);
        for (x, y) in [c.origin, c.end] {
            for k in [0.8, -0.2] {
                let px = x + ux * k * c.size;
                let py = y + uy * k * c.size;
                r.llx = r.llx.min(px);
                r.lly = r.lly.min(py);
                r.urx = r.urx.max(px);
                r.ury = r.ury.max(py);
            }
        }
    }
    if !r.llx.is_finite() {
        return Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 0.0,
            ury: 0.0,
        };
    }
    r
}

/// The unit baseline direction of a character (right if it has no extent).
fn direction(c: &TextChar) -> (f64, f64) {
    let (dx, dy) = (c.end.0 - c.origin.0, c.end.1 - c.origin.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 {
        (1.0, 0.0)
    } else {
        (dx / len, dy / len)
    }
}

/// Fraction of the font size a gap must exceed to read as a space.
const SPACE_GAP: f64 = 0.2;

/// Groups characters, in drawing order, into lines.
fn build_lines(chars: Vec<TextChar>) -> Vec<TextLine> {
    let mut lines: Vec<TextLine> = Vec::new();
    let mut current: Vec<TextChar> = Vec::new();
    let mut text = String::new();
    let mut dir = (1.0, 0.0);

    let finish = |current: &mut Vec<TextChar>, text: &mut String, lines: &mut Vec<TextLine>| {
        let t = text.trim_end().to_owned();
        if !current.is_empty() && !t.is_empty() {
            lines.push(TextLine {
                text: t,
                bbox: bounds(current.iter()),
                chars: std::mem::take(current),
            });
        }
        current.clear();
        text.clear();
    };

    for c in chars {
        let d = direction(&c);
        if let Some(last) = current.last() {
            let size = c.size.max(last.size).max(1e-6);
            let same_direction = d.0 * dir.0 + d.1 * dir.1 > 0.985;
            let (gx, gy) = (c.origin.0 - last.end.0, c.origin.1 - last.end.1);
            let along = gx * dir.0 + gy * dir.1;
            let across = (-gx * dir.1 + gy * dir.0).abs();
            let continues = same_direction && across < 0.5 * size && along > -0.5 * size;
            if !continues {
                finish(&mut current, &mut text, &mut lines);
            } else if along > SPACE_GAP * size
                && !text.ends_with(char::is_whitespace)
                && !c.text.starts_with(char::is_whitespace)
            {
                text.push(' ');
                current.push(TextChar {
                    text: " ".into(),
                    origin: last.end,
                    end: c.origin,
                    size: c.size,
                    font: c.font.clone(),
                    invisible: c.invisible,
                });
            }
        }
        if current.is_empty() {
            dir = d;
        }
        text.push_str(&c.text);
        current.push(c);
    }
    finish(&mut current, &mut text, &mut lines);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(text: &str, x: f64, y: f64, w: f64) -> TextChar {
        TextChar {
            text: text.into(),
            origin: (x, y),
            end: (x + w, y),
            size: 10.0,
            font: "F".into(),
            invisible: false,
        }
    }

    #[test]
    fn lines_spaces_and_words() {
        let chars = vec![
            ch("H", 0.0, 100.0, 6.0),
            ch("i", 6.0, 100.0, 3.0),
            // A gap of 5 (> 0.2 em): a space.
            ch("y", 14.0, 100.0, 5.0),
            ch("o", 19.0, 100.0, 5.0),
            // Next line.
            ch("N", 0.0, 85.0, 7.0),
            ch(" ", 7.0, 85.0, 3.0),
            ch("o", 10.0, 85.0, 5.0),
        ];
        let lines = build_lines(chars);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Hi yo");
        assert_eq!(lines[1].text, "N o");
        let words = lines[0].words();
        assert_eq!(
            words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            ["Hi", "yo"]
        );
        assert_eq!(words[0].bbox.llx, 0.0);
        assert_eq!(words[0].bbox.urx, 9.0);
        assert!((words[0].bbox.ury - 108.0).abs() < 1e-9);
        let page = PageText { lines };
        assert_eq!(page.text(), "Hi yo\nN o\n");
        assert_eq!(page.words().len(), 4);
    }

    #[test]
    fn going_backwards_starts_a_line() {
        let lines = build_lines(vec![ch("a", 50.0, 10.0, 5.0), ch("b", 0.0, 10.0, 5.0)]);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn vertical_text_is_its_own_line() {
        let down = |t: &str, y: f64| TextChar {
            text: t.into(),
            origin: (0.0, y),
            end: (0.0, y - 10.0),
            size: 10.0,
            font: "F".into(),
            invisible: false,
        };
        let lines = build_lines(vec![
            down("縦", 100.0),
            down("書", 90.0),
            ch("x", 50.0, 50.0, 5.0),
        ]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "縦書");
    }

    #[test]
    fn rotation_to_display_space() {
        let crop = Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 100.0,
            ury: 200.0,
        };
        let at = |rotate| Display { crop, rotate }.point((0.0, 200.0));
        assert_eq!(at(0), (0.0, 200.0));
        assert_eq!(at(90), (200.0, 100.0));
        assert_eq!(at(180), (100.0, 0.0));
        assert_eq!(at(270), (0.0, 0.0));
    }
}
