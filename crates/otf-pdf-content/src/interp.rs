//! The content-stream interpreter: runs a page's operations, tracking the
//! graphics and text state, resources and Form XObjects, and reports what is
//! drawn to a [`Device`].

use std::sync::Arc;

use otf_pdf_doc::{Page, Store};
use otf_pdf_syntax::{Dict, Error, LimitKind, ObjId, Object, Result};

use crate::device::{Device, FillRule, FontLoader, Glyph, GraphicsState, Image, PathSegment};
use crate::matrix::Matrix;
use crate::ops::{Operation, name_at, parse_content};

/// Deepest `q` nesting kept; deeper saves are ignored (and their `Q`s).
const MAX_SAVE_DEPTH: usize = 1024;

/// Runs `page` through `device`.
pub fn run_page(
    store: &Store,
    page: &Page,
    fonts: &dyn FontLoader,
    device: &mut dyn Device,
) -> Result<()> {
    let content = page_content(store, page)?;
    let resources = page.resources.clone().unwrap_or_default();
    let mut interp = Interpreter {
        store,
        fonts,
        device,
        forms: Vec::new(),
    };
    let mut state = GraphicsState::default();
    interp.run(&content, &resources, &mut state)?;
    interp.run_annotations(page, &resources)
}

/// The page's content: its content streams decoded and joined.
pub fn page_content(store: &Store, page: &Page) -> Result<Vec<u8>> {
    let contents = store.resolve(page.dict.get(b"Contents").unwrap_or(&Object::Null));
    let parts: Vec<Object> = match contents {
        Object::Array(items) => items.iter().map(|i| store.resolve(i)).collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for part in parts {
        if let Object::Stream(s) = part {
            // A broken part loses its own content, not the page's.
            if let Ok(decoded) = store.decode_stream(&s) {
                out.extend_from_slice(&decoded.data);
                out.push(b'\n');
            }
        }
    }
    Ok(out)
}

struct Interpreter<'a> {
    store: &'a Store,
    fonts: &'a dyn FontLoader,
    device: &'a mut dyn Device,
    /// Form XObjects being run, innermost last: guards cycles and depth.
    forms: Vec<ObjId>,
}

/// The text object state (`BT` … `ET`).
#[derive(Debug, Clone, Copy)]
struct TextObject {
    tm: Matrix,
    tlm: Matrix,
}

fn nums<const N: usize>(operands: &[Object]) -> Option<[f64; N]> {
    let mut out = [0.0; N];
    let tail = operands.get(operands.len().checked_sub(N)?..)?;
    for (slot, o) in out.iter_mut().zip(tail) {
        *slot = o.as_f64().filter(|v| v.is_finite())?;
    }
    Some(out)
}

impl Interpreter<'_> {
    fn run(&mut self, content: &[u8], resources: &Dict, state: &mut GraphicsState) -> Result<()> {
        let limits = self.store.limits();
        let ops = parse_content(content, limits)?;
        let mut stack: Vec<GraphicsState> = Vec::new();
        let mut ignored_saves = 0usize;
        let mut text = TextObject {
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
        };
        let mut path: Vec<PathSegment> = Vec::new();
        let mut pending_clip: Option<FillRule> = None;

        for Operation { operator, operands } in &ops {
            match operator.as_slice() {
                b"q" => {
                    if stack.len() < MAX_SAVE_DEPTH {
                        stack.push(state.clone());
                    } else {
                        ignored_saves += 1;
                    }
                }
                b"Q" => {
                    if ignored_saves > 0 {
                        ignored_saves -= 1;
                    } else if let Some(s) = stack.pop() {
                        *state = s;
                    }
                }
                b"cm" => {
                    if let Some(m) = nums::<6>(operands).and_then(|v| Matrix::from_slice(&v)) {
                        state.ctm = m.then(&state.ctm);
                    }
                }
                b"w" => {
                    if let Some([w]) = nums::<1>(operands) {
                        state.line_width = w;
                    }
                }
                // Paths.
                b"m" => {
                    if let Some([x, y]) = nums::<2>(operands) {
                        path.push(PathSegment::MoveTo(x, y));
                    }
                }
                b"l" => {
                    if let Some([x, y]) = nums::<2>(operands) {
                        path.push(PathSegment::LineTo(x, y));
                    }
                }
                b"c" => {
                    if let Some([a, b, c, d, e, f]) = nums::<6>(operands) {
                        path.push(PathSegment::CurveTo(a, b, c, d, e, f));
                    }
                }
                b"v" | b"y" => {
                    if let Some([a, b, c, d]) = nums::<4>(operands) {
                        let current = current_point(&path);
                        path.push(if operator.as_slice() == b"v" {
                            PathSegment::CurveTo(current.0, current.1, a, b, c, d)
                        } else {
                            PathSegment::CurveTo(a, b, c, d, c, d)
                        });
                    }
                }
                b"h" => path.push(PathSegment::Close),
                b"re" => {
                    if let Some([x, y, w, h]) = nums::<4>(operands) {
                        path.extend([
                            PathSegment::MoveTo(x, y),
                            PathSegment::LineTo(x + w, y),
                            PathSegment::LineTo(x + w, y + h),
                            PathSegment::LineTo(x, y + h),
                            PathSegment::Close,
                        ]);
                    }
                }
                b"W" => pending_clip = Some(FillRule::NonZero),
                b"W*" => pending_clip = Some(FillRule::EvenOdd),
                op @ (b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" | b"n") => {
                    let closes = matches!(op, b"s" | b"b" | b"b*");
                    if closes {
                        path.push(PathSegment::Close);
                    }
                    let even_odd = matches!(op, b"f*" | b"B*" | b"b*");
                    let rule = if even_odd {
                        FillRule::EvenOdd
                    } else {
                        FillRule::NonZero
                    };
                    if matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*") {
                        self.device.fill_path(&path, rule, state);
                    }
                    if matches!(op, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*") {
                        self.device.stroke_path(&path, state);
                    }
                    if let Some(clip_rule) = pending_clip.take() {
                        self.device.clip(&path, clip_rule, state);
                        state.clip_to(path.iter().flat_map(|seg| match *seg {
                            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => vec![(x, y)],
                            PathSegment::CurveTo(a, b, c, d, e, f) => vec![(a, b), (c, d), (e, f)],
                            PathSegment::Close => Vec::new(),
                        }));
                    }
                    path.clear();
                }
                // Text objects and state.
                b"BT" => {
                    text = TextObject {
                        tm: Matrix::IDENTITY,
                        tlm: Matrix::IDENTITY,
                    };
                }
                b"ET" => {}
                b"Tc" => {
                    if let Some([v]) = nums::<1>(operands) {
                        state.text.char_spacing = v;
                    }
                }
                b"Tw" => {
                    if let Some([v]) = nums::<1>(operands) {
                        state.text.word_spacing = v;
                    }
                }
                b"Tz" => {
                    if let Some([v]) = nums::<1>(operands) {
                        state.text.horizontal_scaling = v / 100.0;
                    }
                }
                b"TL" => {
                    if let Some([v]) = nums::<1>(operands) {
                        state.text.leading = v;
                    }
                }
                b"Ts" => {
                    if let Some([v]) = nums::<1>(operands) {
                        state.text.rise = v;
                    }
                }
                b"Tr" => {
                    if let Some([v]) = nums::<1>(operands) {
                        state.text.render_mode = v as i64;
                    }
                }
                b"Tf" => {
                    if let Some([size]) = nums::<1>(operands) {
                        state.text.size = size;
                    }
                    if let Some(name) = name_at(operands, 0) {
                        state.text.font = resources
                            .get(b"Font")
                            .and_then(|f| self.store.resolve_dict(f))
                            .and_then(|fonts| fonts.get(name.as_bytes()).cloned())
                            .and_then(|font| self.fonts.load(self.store, &font));
                    }
                }
                b"Td" => {
                    if let Some([x, y]) = nums::<2>(operands) {
                        text.tlm = Matrix::translate(x, y).then(&text.tlm);
                        text.tm = text.tlm;
                    }
                }
                b"TD" => {
                    if let Some([x, y]) = nums::<2>(operands) {
                        state.text.leading = -y;
                        text.tlm = Matrix::translate(x, y).then(&text.tlm);
                        text.tm = text.tlm;
                    }
                }
                b"Tm" => {
                    if let Some(m) = nums::<6>(operands).and_then(|v| Matrix::from_slice(&v)) {
                        text.tlm = m;
                        text.tm = m;
                    }
                }
                b"T*" => next_line(&mut text, state),
                b"Tj" => {
                    if let Some(Object::String(s)) = operands.last() {
                        self.show(&s.bytes, &mut text, state);
                    }
                }
                b"'" => {
                    next_line(&mut text, state);
                    if let Some(Object::String(s)) = operands.last() {
                        self.show(&s.bytes, &mut text, state);
                    }
                }
                b"\"" => {
                    if let [aw, ac, Object::String(s)] = operands.as_slice() {
                        if let (Some(aw), Some(ac)) = (aw.as_f64(), ac.as_f64()) {
                            state.text.word_spacing = aw;
                            state.text.char_spacing = ac;
                        }
                        next_line(&mut text, state);
                        self.show(&s.bytes, &mut text, state);
                    }
                }
                b"TJ" => {
                    if let Some(Object::Array(items)) = operands.last() {
                        for item in items {
                            match item {
                                Object::String(s) => self.show(&s.bytes, &mut text, state),
                                other => {
                                    if let Some(adjust) = other.as_f64().filter(|v| v.is_finite()) {
                                        let t = &state.text;
                                        let shift = -adjust / 1000.0 * t.size;
                                        let vertical =
                                            t.font.as_ref().is_some_and(|f| f.is_vertical());
                                        let m = if vertical {
                                            Matrix::translate(0.0, shift)
                                        } else {
                                            Matrix::translate(shift * t.horizontal_scaling, 0.0)
                                        };
                                        text.tm = m.then(&text.tm);
                                    }
                                }
                            }
                        }
                    }
                }
                // XObjects and images.
                b"Do" => {
                    if let Some(name) = name_at(operands, 0) {
                        self.do_xobject(name.as_bytes(), resources, state)?;
                    }
                }
                b"BI" => {
                    if let [Object::Dict(dict), Object::String(data)] = operands.as_slice() {
                        let image = Image {
                            id: None,
                            dict: dict.clone(),
                            inline_data: data.bytes.clone(),
                        };
                        self.device.draw_image(&image, state);
                    }
                }
                // Marked content.
                b"BMC" => {
                    if let Some(tag) = name_at(operands, 0) {
                        self.device.begin_marked_content(tag, None);
                    }
                }
                b"BDC" => {
                    if let Some(tag) = name_at(operands, 0) {
                        let props = match operands.get(1) {
                            Some(Object::Dict(d)) => Some(d.clone()),
                            Some(Object::Name(n)) => resources
                                .get(b"Properties")
                                .and_then(|p| self.store.resolve_dict(p))
                                .and_then(|p| p.get(n.as_bytes()).cloned())
                                .and_then(|p| self.store.resolve_dict(&p)),
                            _ => None,
                        };
                        self.device.begin_marked_content(tag, props.as_ref());
                    }
                }
                b"EMC" => self.device.end_marked_content(),
                _ => {}
            }
        }
        Ok(())
    }

    /// Shows a string: one glyph per character code, advancing the text
    /// matrix (§9.4.4).
    fn show(&mut self, bytes: &[u8], text: &mut TextObject, state: &GraphicsState) {
        let t = &state.text;
        let Some(font) = t.font.clone() else { return };
        let vertical = font.is_vertical();
        for (code, len) in font.codes(bytes) {
            let scale = Matrix::new(t.size * t.horizontal_scaling, 0.0, 0.0, t.size, 0.0, t.rise);
            let to_page = text.tm.then(&state.ctm);
            let trm = scale.then(&to_page);
            let word = if len == 1 && code == 32 {
                t.word_spacing
            } else {
                0.0
            };
            let advance = if vertical {
                Matrix::translate(
                    0.0,
                    font.vertical_advance(code) * t.size + t.char_spacing + word,
                )
            } else {
                let tx = (font.width(code) * t.size + t.char_spacing + word) * t.horizontal_scaling;
                Matrix::translate(tx, 0.0)
            };
            let next = advance.then(&text.tm);
            let origin = to_page.apply(0.0, t.rise);
            let end = next.then(&state.ctm).apply(0.0, t.rise);
            let (vx, vy) = to_page.apply_vector(0.0, t.size);
            let glyph = Glyph {
                code,
                unicode: font.unicode(code),
                font: Arc::clone(&font),
                trm,
                origin,
                end,
                size: (vx * vx + vy * vy).sqrt(),
                render_mode: t.render_mode,
            };
            self.device.draw_glyph(&glyph, state);
            text.tm = next;
        }
    }

    fn do_xobject(
        &mut self,
        name: &[u8],
        resources: &Dict,
        state: &mut GraphicsState,
    ) -> Result<()> {
        let Some(reference) = resources
            .get(b"XObject")
            .and_then(|x| self.store.resolve_dict(x))
            .and_then(|x| x.get(name).cloned())
        else {
            return Ok(());
        };
        let id = reference.as_ref();
        let Object::Stream(stream) = self.store.resolve(&reference) else {
            return Ok(());
        };
        match stream.dict.get_name(b"Subtype") {
            Some(b"Image") => {
                let image = Image {
                    id,
                    dict: stream.dict.clone(),
                    inline_data: Vec::new(),
                };
                self.device.draw_image(&image, state);
            }
            Some(b"Form") => {
                // A form drawing itself, or nested deeper than the limit.
                if id.is_some_and(|id| self.forms.contains(&id)) {
                    return Ok(());
                }
                if self.forms.len() as u32 >= self.store.limits().max_depth.min(64) {
                    return Err(Error::Limit(LimitKind::Depth));
                }
                let Ok(decoded) = self.store.decode_stream(&stream) else {
                    return Ok(());
                };
                let form_resources = stream
                    .dict
                    .get(b"Resources")
                    .and_then(|r| self.store.resolve_dict(r))
                    .unwrap_or_else(|| resources.clone());
                let mut inner = state.clone();
                if let Some(m) = stream
                    .dict
                    .get_array(b"Matrix")
                    .and_then(|a| a.iter().map(Object::as_f64).collect::<Option<Vec<_>>>())
                    .and_then(|v| Matrix::from_slice(&v))
                {
                    inner.ctm = m.then(&inner.ctm);
                }
                if let Some([x0, y0, x1, y1]) = rect_of(stream.dict.get_array(b"BBox")) {
                    inner.clip_to([(x0, y0), (x1, y0), (x0, y1), (x1, y1)]);
                }
                self.forms.extend(id);
                self.device.begin_group(id, &stream.dict, &inner);
                let result = self.run(&decoded.data, &form_resources, &mut inner);
                self.device.end_group();
                if id.is_some() {
                    self.forms.pop();
                }
                result?;
            }
            _ => {}
        }
        Ok(())
    }
}

impl Interpreter<'_> {
    /// Draws each visible annotation's normal appearance over the page
    /// (§12.5.5), as viewers and `pdftotext` do: filled-in form fields are
    /// part of what the page shows.
    fn run_annotations(&mut self, page: &Page, resources: &Dict) -> Result<()> {
        let annots = self
            .store
            .resolve(page.dict.get(b"Annots").unwrap_or(&Object::Null));
        for annot in annots.as_array().unwrap_or_default() {
            let Some(dict) = self.store.resolve_dict(annot) else {
                continue;
            };
            let flags = dict.get_int(b"F").unwrap_or(0);
            // Hidden (bit 2) or NoView (bit 6), and popups, are not drawn.
            if flags & (2 | 32) != 0 || dict.get_name(b"Subtype") == Some(b"Popup") {
                continue;
            }
            let Some(normal) = dict
                .get(b"AP")
                .and_then(|ap| self.store.resolve_dict(ap))
                .and_then(|ap| ap.get(b"N").cloned())
            else {
                continue;
            };
            // A state dictionary picks the stream by /AS.
            let appearance = match self.store.resolve(&normal) {
                Object::Stream(_) => normal,
                Object::Dict(states) => match dict.get_name(b"AS").and_then(|s| states.get(s)) {
                    Some(o) => o.clone(),
                    None => continue,
                },
                _ => continue,
            };
            let id = appearance.as_ref();
            let Object::Stream(stream) = self.store.resolve(&appearance) else {
                continue;
            };
            let Some(rect) = dict
                .get_array(b"Rect")
                .and_then(|r| r.iter().map(Object::as_f64).collect::<Option<Vec<_>>>())
            else {
                continue;
            };
            let [rx0, ry0, rx1, ry1] = rect[..] else {
                continue;
            };
            let form_matrix = stream
                .dict
                .get_array(b"Matrix")
                .and_then(|a| a.iter().map(Object::as_f64).collect::<Option<Vec<_>>>())
                .and_then(|v| Matrix::from_slice(&v))
                .unwrap_or(Matrix::IDENTITY);
            let Some(bbox) = stream
                .dict
                .get_array(b"BBox")
                .and_then(|r| r.iter().map(Object::as_f64).collect::<Option<Vec<_>>>())
            else {
                continue;
            };
            let [bx0, by0, bx1, by1] = bbox[..] else {
                continue;
            };
            // The bbox, transformed by /Matrix, mapped onto /Rect.
            let corners = [(bx0, by0), (bx1, by0), (bx0, by1), (bx1, by1)]
                .map(|(x, y)| form_matrix.apply(x, y));
            let (mut tx0, mut ty0, mut tx1, mut ty1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for (x, y) in corners {
                tx0 = tx0.min(x);
                ty0 = ty0.min(y);
                tx1 = tx1.max(x);
                ty1 = ty1.max(y);
            }
            let (w, h) = (tx1 - tx0, ty1 - ty0);
            if !(w.abs() > 1e-9 && h.abs() > 1e-9) {
                continue;
            }
            let (sx, sy) = ((rx1 - rx0).abs() / w, (ry1 - ry0).abs() / h);
            let fit = Matrix::new(
                sx,
                0.0,
                0.0,
                sy,
                rx0.min(rx1) - tx0 * sx,
                ry0.min(ry1) - ty0 * sy,
            );
            let mut state = GraphicsState {
                ctm: fit,
                ..GraphicsState::default()
            };
            let Ok(decoded) = self.store.decode_stream(&stream) else {
                continue;
            };
            let form_resources = stream
                .dict
                .get(b"Resources")
                .and_then(|r| self.store.resolve_dict(r))
                .unwrap_or_else(|| resources.clone());
            state.ctm = form_matrix.then(&state.ctm);
            state.clip_to([(bx0, by0), (bx1, by0), (bx0, by1), (bx1, by1)]);
            self.forms.extend(id);
            self.device.begin_group(id, &stream.dict, &state);
            let result = self.run(&decoded.data, &form_resources, &mut state);
            self.device.end_group();
            if id.is_some() {
                self.forms.pop();
            }
            if let Err(e @ Error::Limit(_)) = result {
                return Err(e);
            }
        }
        Ok(())
    }
}

/// Four numbers of a rectangle array.
fn rect_of(array: Option<&[Object]>) -> Option<[f64; 4]> {
    let v: Vec<f64> = array?.iter().map(Object::as_f64).collect::<Option<_>>()?;
    match v[..] {
        [a, b, c, d] if v.iter().all(|x| x.is_finite()) => Some([a, b, c, d]),
        _ => None,
    }
}

fn next_line(text: &mut TextObject, state: &GraphicsState) {
    text.tlm = Matrix::translate(0.0, -state.text.leading).then(&text.tlm);
    text.tm = text.tlm;
}

fn current_point(path: &[PathSegment]) -> (f64, f64) {
    match path.last() {
        Some(PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y)) => (*x, *y),
        Some(PathSegment::CurveTo(_, _, _, _, x, y)) => (*x, *y),
        _ => (0.0, 0.0),
    }
}
