//! What the interpreter reports: the [`Device`] trait and what it receives,
//! and the [`Font`] trait the interpreter needs to lay out text.

use std::fmt;
use std::sync::Arc;

use otf_pdf_doc::Store;
use otf_pdf_syntax::{Dict, Name, ObjId, Object};

use crate::matrix::Matrix;

/// A loaded font, as the interpreter needs it: how a string splits into
/// character codes, how far each code advances, and (for extraction) what
/// text it stands for. Implemented by `otf-pdf-font`.
pub trait Font: Send + Sync + fmt::Debug {
    /// Splits a shown string into `(code, byte length)` pairs.
    fn codes(&self, bytes: &[u8]) -> Vec<(u32, usize)>;
    /// Horizontal advance of `code`, in text space units per unit of font
    /// size (glyph width / 1000 for most fonts).
    fn width(&self, code: u32) -> f64;
    /// Whether the font is written vertically (an `-V` CMap).
    fn is_vertical(&self) -> bool {
        false
    }
    /// Vertical advance of `code` in vertical writing (negative is down).
    fn vertical_advance(&self, _code: u32) -> f64 {
        -1.0
    }
    /// The Unicode text for `code`, if known.
    fn unicode(&self, code: u32) -> Option<String>;
    /// The font's name (`/BaseFont`), for reporting.
    fn name(&self) -> &str;
}

/// Loads fonts from font dictionaries; implementations cache them.
pub trait FontLoader {
    /// The font for `font` (a reference to, or a, font dictionary).
    fn load(&self, store: &Store, font: &Object) -> Option<Arc<dyn Font>>;
}

/// The text state (§9.3).
#[derive(Debug, Clone)]
pub struct TextState {
    /// The current font.
    pub font: Option<Arc<dyn Font>>,
    /// Font size (`Tf`).
    pub size: f64,
    /// Character spacing (`Tc`).
    pub char_spacing: f64,
    /// Word spacing (`Tw`).
    pub word_spacing: f64,
    /// Horizontal scaling as a fraction (`Tz` / 100).
    pub horizontal_scaling: f64,
    /// Leading (`TL`).
    pub leading: f64,
    /// Rise (`Ts`).
    pub rise: f64,
    /// Rendering mode (`Tr`); 3 is invisible (OCR text layers).
    pub render_mode: i64,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scaling: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
        }
    }
}

/// The parts of the graphics state the interpreter tracks.
#[derive(Debug, Clone)]
pub struct GraphicsState {
    /// Current transformation matrix: user space to page space.
    pub ctm: Matrix,
    /// Line width.
    pub line_width: f64,
    /// Text state.
    pub text: TextState,
    /// The clip, approximated by its bounding box in page space
    /// (`[x0, y0, x1, y1]`); `None` is unclipped. Form and annotation
    /// bounding boxes and `W`/`W*` paths narrow it.
    pub clip: Option<[f64; 4]>,
}

impl GraphicsState {
    /// Narrows the clip to the page-space bounding box of `points`
    /// (user space).
    pub fn clip_to(&mut self, points: impl IntoIterator<Item = (f64, f64)>) {
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for (x, y) in points {
            let (px, py) = self.ctm.apply(x, y);
            b = [b[0].min(px), b[1].min(py), b[2].max(px), b[3].max(py)];
        }
        if b[0] > b[2] {
            return;
        }
        self.clip = Some(match self.clip {
            Some(c) => [
                c[0].max(b[0]),
                c[1].max(b[1]),
                c[2].min(b[2]),
                c[3].min(b[3]),
            ],
            None => b,
        });
    }

    /// Whether a page-space point is inside the clip (with a tolerance of
    /// `slack` points).
    pub fn in_clip(&self, (x, y): (f64, f64), slack: f64) -> bool {
        self.clip.is_none_or(|c| {
            x >= c[0] - slack && x <= c[2] + slack && y >= c[1] - slack && y <= c[3] + slack
        })
    }
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            line_width: 1.0,
            text: TextState::default(),
            clip: None,
        }
    }
}

/// One shown glyph.
#[derive(Debug, Clone)]
pub struct Glyph {
    /// The character code.
    pub code: u32,
    /// Its Unicode text, if the font knows it.
    pub unicode: Option<String>,
    /// The font.
    pub font: Arc<dyn Font>,
    /// Text rendering matrix: glyph space (scaled by the font size) to page
    /// space.
    pub trm: Matrix,
    /// Where the glyph starts on the baseline, in page space.
    pub origin: (f64, f64),
    /// Where the next glyph would start, in page space.
    pub end: (f64, f64),
    /// The font size in page space (the height of one em).
    pub size: f64,
    /// Text rendering mode.
    pub render_mode: i64,
}

/// A path segment, in user space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathSegment {
    /// Start a subpath.
    MoveTo(f64, f64),
    /// Straight line.
    LineTo(f64, f64),
    /// Cubic Bézier curve: two control points and the end point.
    CurveTo(f64, f64, f64, f64, f64, f64),
    /// Close the subpath.
    Close,
}

/// How a path's inside is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    /// Nonzero winding.
    NonZero,
    /// Even-odd.
    EvenOdd,
}

/// An image to draw: an XObject or an inline image.
#[derive(Debug, Clone)]
pub struct Image {
    /// The image XObject's id, or `None` for an inline image.
    pub id: Option<ObjId>,
    /// The image dictionary.
    pub dict: Dict,
    /// The inline image's (still encoded) data; empty for an XObject.
    pub inline_data: Vec<u8>,
}

/// Receives what a content stream draws. Every method has an empty default,
/// so a device implements only what it needs: text extraction takes glyphs
/// and marked content; a renderer takes everything.
#[allow(unused_variables)]
pub trait Device {
    /// Fills `path` (user space) with the current fill colour.
    fn fill_path(&mut self, path: &[PathSegment], rule: FillRule, state: &GraphicsState) {}
    /// Strokes `path` (user space).
    fn stroke_path(&mut self, path: &[PathSegment], state: &GraphicsState) {}
    /// Intersects the clip with `path`.
    fn clip(&mut self, path: &[PathSegment], rule: FillRule, state: &GraphicsState) {}
    /// Draws one glyph.
    fn draw_glyph(&mut self, glyph: &Glyph, state: &GraphicsState) {}
    /// Draws an image into the unit square of `state.ctm`.
    fn draw_image(&mut self, image: &Image, state: &GraphicsState) {}
    /// Starts a marked-content sequence (`BMC`/`BDC`).
    fn begin_marked_content(&mut self, tag: &Name, properties: Option<&Dict>) {}
    /// Ends the innermost marked-content sequence (`EMC`).
    fn end_marked_content(&mut self) {}
    /// Starts a Form XObject (a group when it has `/Group`).
    fn begin_group(&mut self, form: Option<ObjId>, dict: &Dict, state: &GraphicsState) {}
    /// Ends the Form XObject started last.
    fn end_group(&mut self) {}
}
