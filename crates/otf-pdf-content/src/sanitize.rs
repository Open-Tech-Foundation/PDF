//! The content half of sanitizing: hidden layers and invisible text, which
//! live inside content streams and so need the parser, the writer and font
//! widths. [`sanitize`] runs it, then the document-level pass
//! ([`Document::sanitize`]).
//!
//! Removed text-showing operations are replaced by a `TJ` that moves the
//! text position by the same amount and draws nothing, so text drawn after
//! them in the same text object stays where it was. Inside hidden layers,
//! operators that change the graphics or text state are kept (state carries
//! past the end of the hidden section) and only drawing is cut; a path that
//! ends in a clip is kept with its clip.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use otf_pdf_doc::{Document, SanitizeOptions, SanitizeReport, Store};
use otf_pdf_syntax::{Dict, ObjId, Object, Result, Stream};

use crate::device::{Font, FontLoader};
use crate::interp::page_content;
use crate::ops::{Operation, parse_content};
use crate::write::write_content;

/// Runs every category of `options`: the content passes (hidden layers,
/// invisible text) with fonts from `fonts`, then
/// [`Document::sanitize`] for the rest. Save in full afterwards.
pub fn sanitize(
    doc: &mut Document,
    options: &SanitizeOptions,
    fonts: &dyn FontLoader,
) -> Result<SanitizeReport> {
    let mut report = SanitizeReport::default();
    if options.rewrites_content() {
        clean_content(doc, options, fonts, &mut report)?;
    }
    let rest = doc.sanitize(&options.without_content())?;
    Ok(merge(report, rest))
}

fn merge(content: SanitizeReport, rest: SanitizeReport) -> SanitizeReport {
    SanitizeReport {
        hidden_layers: content.hidden_layers,
        invisible_text: content.invisible_text,
        ..rest
    }
}

/// Which optional content groups are off in the default view.
struct Layers {
    off: HashSet<ObjId>,
}

impl Layers {
    /// The default configuration's state, or `None` when the document has
    /// no optional content (its `/OC` entries then mean nothing).
    fn of(doc: &Document) -> Option<Self> {
        let store = doc.store();
        let props = store.resolve_dict(doc.catalog().get(b"OCProperties")?)?;
        let all: Vec<ObjId> = refs(store, props.get(b"OCGs"));
        let config = props
            .get(b"D")
            .and_then(|d| store.resolve_dict(d))
            .unwrap_or_default();
        let on: HashSet<ObjId> = refs(store, config.get(b"ON")).into_iter().collect();
        let listed_off: HashSet<ObjId> = refs(store, config.get(b"OFF")).into_iter().collect();
        let off = if config.get_name(b"BaseState") == Some(b"OFF") {
            all.into_iter().filter(|g| !on.contains(g)).collect()
        } else {
            listed_off
        };
        Some(Self { off })
    }

    /// Whether `oc` (an `/OC` value: a group or a membership dictionary)
    /// is hidden.
    fn hidden(&self, store: &Store, oc: &Object) -> bool {
        !self.visible(store, oc, 0)
    }

    fn visible(&self, store: &Store, oc: &Object, depth: u32) -> bool {
        if depth > 32 {
            return true;
        }
        let Some(dict) = store.resolve_dict(oc) else {
            return true;
        };
        if dict.get_name(b"Type") == Some(b"OCMD") {
            if let Some(ve) = dict.get(b"VE") {
                return self.expression(store, ve, depth + 1);
            }
            let groups = refs(store, dict.get(b"OCGs"));
            if groups.is_empty() {
                return true;
            }
            let on = |g: &ObjId| !self.off.contains(g);
            return match dict.get_name(b"P") {
                Some(b"AllOn") => groups.iter().all(on),
                Some(b"AnyOff") => !groups.iter().all(on),
                Some(b"AllOff") => !groups.iter().any(on),
                _ => groups.iter().any(on),
            };
        }
        oc.as_ref().is_none_or(|id| !self.off.contains(&id))
    }

    /// A visibility expression: a group, or `[/And …]`, `[/Or …]`,
    /// `[/Not x]`.
    fn expression(&self, store: &Store, ve: &Object, depth: u32) -> bool {
        if depth > 32 {
            return true;
        }
        let resolved = store.resolve(ve);
        let Some(items) = resolved.as_array() else {
            return ve.as_ref().is_none_or(|id| !self.off.contains(&id));
        };
        let (op, args) = match items.split_first() {
            Some((Object::Name(n), rest)) => (n.as_bytes().to_vec(), rest),
            _ => return true,
        };
        let mut values = args.iter().map(|a| self.expression(store, a, depth + 1));
        match op.as_slice() {
            b"And" => values.all(|v| v),
            b"Or" => values.any(|v| v),
            b"Not" => !values.next().unwrap_or(false),
            _ => true,
        }
    }
}

/// The object ids in a reference or array of references.
fn refs(store: &Store, object: Option<&Object>) -> Vec<ObjId> {
    match object {
        Some(Object::Ref(id)) => match store.resolve(&Object::Ref(*id)) {
            Object::Array(items) => items.iter().filter_map(Object::as_ref).collect(),
            _ => vec![*id],
        },
        Some(Object::Array(items)) => items.iter().filter_map(Object::as_ref).collect(),
        _ => Vec::new(),
    }
}

/// What a content stream drew and what was cut from it.
#[derive(Default)]
struct Walk {
    /// Form XObjects drawn where they are visible, with the resources they
    /// inherit when they have none of their own.
    forms: Vec<(ObjId, Dict)>,
    /// XObjects drawn somewhere visible.
    shown: HashSet<ObjId>,
    /// XObjects whose drawing was cut.
    cut: HashSet<ObjId>,
}

struct Cleaner<'a> {
    store: &'a Store,
    fonts: &'a dyn FontLoader,
    layers: Option<&'a Layers>,
    invisible_text: bool,
    report: SanitizeReport,
    walk: Walk,
}

/// Text state, saved and restored with the graphics state.
#[derive(Clone, Default)]
struct TextState {
    font: Option<Arc<dyn Font>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    render_mode: i64,
}

fn num(o: Option<&Object>) -> Option<f64> {
    o.and_then(Object::as_f64).filter(|v| v.is_finite())
}

fn op(operator: &[u8], operands: Vec<Object>) -> Operation {
    Operation {
        operator: operator.to_vec(),
        operands,
    }
}

impl Cleaner<'_> {
    /// The cleaned operations, or `None` when nothing changed.
    fn clean(&mut self, ops: Vec<Operation>, resources: &Dict) -> Result<Option<Vec<Operation>>> {
        let limits = self.store.limits();
        let mut out = Vec::with_capacity(ops.len());
        let mut changed = false;
        let mut text = TextState::default();
        let mut saved: Vec<TextState> = Vec::new();
        // One entry per open marked-content section: whether it is hidden.
        let mut marked: Vec<bool> = Vec::new();
        // Path operators inside hidden content, held until the path ends.
        let mut path: Vec<Operation> = Vec::new();

        for o in ops {
            limits.charge(1)?;
            let hidden = marked.iter().any(|h| *h);
            let operator = o.operator.as_slice();
            // State first: it applies whether or not the operation is cut.
            match operator {
                b"q" => saved.push(text.clone()),
                b"Q" => {
                    if let Some(t) = saved.pop() {
                        text = t;
                    }
                }
                b"Tc" => text.char_spacing = num(o.operands.first()).unwrap_or(text.char_spacing),
                b"Tw" => text.word_spacing = num(o.operands.first()).unwrap_or(text.word_spacing),
                b"Tr" => {
                    if let Some(v) = num(o.operands.first()) {
                        text.render_mode = v as i64;
                    }
                }
                b"Tf" => {
                    if let Some(size) = num(o.operands.get(1)) {
                        text.size = size;
                    }
                    if let Some(Object::Name(name)) = o.operands.first() {
                        text.font = resources
                            .get(b"Font")
                            .and_then(|f| self.store.resolve_dict(f))
                            .and_then(|fonts| fonts.get(name.as_bytes()).cloned())
                            .and_then(|font| self.fonts.load(self.store, &font));
                    }
                }
                _ => {}
            }
            match operator {
                b"BMC" => {
                    marked.push(false);
                    out.push(o);
                    continue;
                }
                b"BDC" => {
                    let cut = !hidden && self.section_hidden(&o, resources);
                    if cut {
                        self.report.hidden_layers += 1;
                    }
                    marked.push(cut);
                    out.push(o);
                    continue;
                }
                b"EMC" => {
                    marked.pop();
                    out.push(o);
                    continue;
                }
                _ => {}
            }
            let shows = matches!(operator, b"Tj" | b"TJ" | b"'" | b"\"");
            if hidden {
                if shows {
                    out.extend(self.replace_show(o, &mut text));
                    changed = true;
                } else if is_path_construction(operator) || matches!(operator, b"W" | b"W*") {
                    path.push(o);
                } else if operator == b"n" {
                    // A clip: keep it, since it applies after the section.
                    if path
                        .iter()
                        .any(|p| matches!(p.operator.as_slice(), b"W" | b"W*"))
                    {
                        out.append(&mut path);
                        out.push(o);
                    } else {
                        path.clear();
                        changed = true;
                    }
                } else if is_painting(operator) || matches!(operator, b"sh" | b"BI") {
                    path.clear();
                    changed = true;
                } else if operator == b"Do" {
                    if let Some(id) = self.xobject(&o, resources).and_then(|(id, _)| id) {
                        self.walk.cut.insert(id);
                    }
                    changed = true;
                } else {
                    out.push(o);
                }
                continue;
            }
            if shows && self.invisible_text && text.render_mode == 3 {
                self.report.invisible_text += 1;
                out.extend(self.replace_show(o, &mut text));
                changed = true;
                continue;
            }
            if operator == b"Do"
                && let Some((id, stream)) = self.xobject(&o, resources)
            {
                let oc_hidden = self
                    .layers
                    .zip(stream.dict.get(b"OC"))
                    .is_some_and(|(l, oc)| l.hidden(self.store, oc));
                if oc_hidden {
                    self.report.hidden_layers += 1;
                    if let Some(id) = id {
                        self.walk.cut.insert(id);
                    }
                    changed = true;
                    continue;
                }
                if let Some(id) = id {
                    self.walk.shown.insert(id);
                    if stream.dict.get_name(b"Subtype") == Some(b"Form") {
                        self.walk.forms.push((id, resources.clone()));
                    }
                }
            }
            if operator == b"\"" {
                text.word_spacing = num(o.operands.first()).unwrap_or(text.word_spacing);
                text.char_spacing = num(o.operands.get(1)).unwrap_or(text.char_spacing);
            }
            out.push(o);
        }
        Ok(changed.then_some(out))
    }

    /// Whether a `BDC` opens an optional-content section that is hidden.
    fn section_hidden(&self, o: &Operation, resources: &Dict) -> bool {
        let Some(layers) = self.layers else {
            return false;
        };
        let [Object::Name(tag), props] = o.operands.as_slice() else {
            return false;
        };
        if tag.as_bytes() != b"OC" {
            return false;
        }
        let oc = match props {
            Object::Name(name) => resources
                .get(b"Properties")
                .and_then(|p| self.store.resolve_dict(p))
                .and_then(|p| p.get(name.as_bytes()).cloned()),
            other => Some(other.clone()),
        };
        oc.is_some_and(|oc| layers.hidden(self.store, &oc))
    }

    /// The XObject a `Do` draws: its id and stream.
    fn xobject(&self, o: &Operation, resources: &Dict) -> Option<(Option<ObjId>, Stream)> {
        let Some(Object::Name(name)) = o.operands.first() else {
            return None;
        };
        let reference = resources
            .get(b"XObject")
            .and_then(|x| self.store.resolve_dict(x))
            .and_then(|x| x.get(name.as_bytes()).cloned())?;
        match self.store.resolve(&reference) {
            Object::Stream(s) => Some((reference.as_ref(), s)),
            _ => None,
        }
    }

    /// A text-showing operation replaced by what it does besides drawing:
    /// the line moves of `'` and `"`, the spacing `"` sets, and the text
    /// advance, as a `TJ` of numbers only.
    fn replace_show(&self, o: Operation, text: &mut TextState) -> Vec<Operation> {
        let mut out = Vec::new();
        let mut strings: Vec<&[u8]> = Vec::new();
        let mut shift = 0.0;
        match o.operator.as_slice() {
            b"Tj" => {
                if let Some(Object::String(s)) = o.operands.last() {
                    strings.push(&s.bytes);
                }
            }
            b"'" => {
                out.push(op(b"T*", Vec::new()));
                if let Some(Object::String(s)) = o.operands.last() {
                    strings.push(&s.bytes);
                }
            }
            b"\"" => {
                if let [aw, ac, Object::String(s)] = o.operands.as_slice() {
                    out.push(op(b"Tw", vec![aw.clone()]));
                    out.push(op(b"Tc", vec![ac.clone()]));
                    out.push(op(b"T*", Vec::new()));
                    text.word_spacing = num(Some(aw)).unwrap_or(text.word_spacing);
                    text.char_spacing = num(Some(ac)).unwrap_or(text.char_spacing);
                    strings.push(&s.bytes);
                }
            }
            _ => {
                if let Some(Object::Array(items)) = o.operands.last() {
                    for item in items {
                        match item {
                            Object::String(s) => strings.push(&s.bytes),
                            other => shift += num(Some(other)).unwrap_or(0.0),
                        }
                    }
                }
            }
        }
        if text.size != 0.0
            && let Some(font) = &text.font
        {
            let vertical = font.is_vertical();
            let mut advance = 0.0;
            for bytes in strings {
                for (code, len) in font.codes(bytes) {
                    let word = if len == 1 && code == 32 {
                        text.word_spacing
                    } else {
                        0.0
                    };
                    let w = if vertical {
                        font.vertical_advance(code)
                    } else {
                        font.width(code)
                    };
                    advance += w * text.size + text.char_spacing + word;
                }
            }
            // A TJ number n moves by -n/1000 of the font size (times the
            // horizontal scaling, which the advance also carries).
            shift -= advance * 1000.0 / text.size;
        }
        if shift != 0.0 && shift.is_finite() {
            out.push(op(b"TJ", vec![Object::Array(vec![Object::Real(shift)])]));
        }
        out
    }
}

fn is_path_construction(op: &[u8]) -> bool {
    matches!(op, b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re")
}

fn is_painting(op: &[u8]) -> bool {
    matches!(
        op,
        b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*"
    )
}

fn clean_content(
    doc: &mut Document,
    options: &SanitizeOptions,
    fonts: &dyn FontLoader,
    report: &mut SanitizeReport,
) -> Result<()> {
    let layers = if options.hidden_layers {
        Layers::of(doc)
    } else {
        None
    };
    let pages = doc.pages()?;
    let mut page_content_out: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut streams_out: Vec<(ObjId, Stream, Vec<u8>)> = Vec::new();
    let mut annots_out: Vec<(usize, Vec<Object>)> = Vec::new();
    let dropped_xobjects;
    {
        let store = doc.store();
        let limits = store.limits();
        let mut cleaner = Cleaner {
            store,
            fonts,
            layers: layers.as_ref(),
            invisible_text: options.invisible_text,
            report: SanitizeReport::default(),
            walk: Walk::default(),
        };
        let mut removed = 0;
        // Appearance streams of the annotations that stay.
        let mut appearances: Vec<(ObjId, Dict)> = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            let resources = page.resources.clone().unwrap_or_default();
            let ops = parse_content(&page_content(store, page)?, limits)?;
            if let Some(clean) = cleaner.clean(ops, &resources)? {
                page_content_out.push((index, write_content(&clean)?));
            }
            let annots_obj = store.resolve(page.dict.get(b"Annots").unwrap_or(&Object::Null));
            let annots = annots_obj.as_array().unwrap_or_default();
            let mut kept = Vec::with_capacity(annots.len());
            for annot in annots {
                let dict = store.resolve_dict(annot).unwrap_or_default();
                let hidden = dict.get_name(b"Subtype") != Some(b"Widget")
                    && layers
                        .as_ref()
                        .zip(dict.get(b"OC"))
                        .is_some_and(|(l, oc)| l.hidden(store, oc));
                if hidden {
                    removed += 1;
                    continue;
                }
                kept.push(annot.clone());
                if let Some(ap) = dict.get(b"AP").and_then(|ap| store.resolve_dict(ap)) {
                    for (_, entry) in ap.iter() {
                        collect_streams(store, entry, &resources, &mut appearances);
                    }
                }
            }
            if kept.len() != annots.len() {
                annots_out.push((index, kept));
            }
        }
        // Forms and appearances, each once; forms found inside are queued.
        let mut done: HashSet<ObjId> = HashSet::new();
        let mut queue: Vec<(ObjId, Dict)> = appearances;
        loop {
            queue.append(&mut cleaner.walk.forms);
            let Some((id, inherited)) = queue.pop() else {
                break;
            };
            if !done.insert(id) {
                continue;
            }
            let Object::Stream(stream) = store.resolve(&Object::Ref(id)) else {
                continue;
            };
            let Ok(decoded) = store.decode_stream(&stream) else {
                continue;
            };
            let resources = stream
                .dict
                .get(b"Resources")
                .and_then(|r| store.resolve_dict(r))
                .unwrap_or(inherited);
            let ops = parse_content(&decoded.data, limits)?;
            if let Some(clean) = cleaner.clean(ops, &resources)? {
                streams_out.push((id, stream, write_content(&clean)?));
            }
        }
        report.hidden_layers = cleaner.report.hidden_layers + removed;
        report.invisible_text = cleaner.report.invisible_text;
        dropped_xobjects = cleaner
            .walk
            .cut
            .difference(&cleaner.walk.shown)
            .copied()
            .collect::<HashSet<ObjId>>();
    }

    for (index, bytes) in page_content_out {
        doc.set_page_content(index, bytes)?;
    }
    for (id, stream, bytes) in streams_out {
        let mut dict = stream.dict;
        for key in [&b"Filter"[..], b"DecodeParms", b"DL", b"Length"] {
            dict.remove(key);
        }
        doc.store_mut().set(
            id,
            Object::Stream(Stream {
                dict,
                raw: Arc::from(bytes),
            }),
        );
    }
    for (index, kept) in annots_out {
        let pages = doc.pages()?;
        let Some(page) = pages.get(index) else {
            continue;
        };
        match (page.dict.get(b"Annots"), page.id) {
            // An indirect /Annots array is replaced where it is.
            (Some(Object::Ref(id)), _) => doc.store_mut().set(*id, Object::Array(kept)),
            (_, Some(page_id)) => {
                let mut dict = page.dict.clone();
                dict.insert("Annots", kept);
                doc.store_mut().set(page_id, Object::Dict(dict));
            }
            _ => {}
        }
    }
    // XObjects drawn only where they were cut: drop them from every
    // resource dictionary so the full save leaves them out.
    if !dropped_xobjects.is_empty() {
        drop_xobjects(doc, &dropped_xobjects);
    }
    if layers.is_some() {
        let mut catalog = doc.catalog();
        if catalog.remove(b"OCProperties").is_some()
            && let Some(id) = doc.catalog_id()
        {
            doc.store_mut().set(id, Object::Dict(catalog));
            report.hidden_layers += 1;
        }
    }
    Ok(())
}

/// The streams in an appearance entry: a stream, or a dictionary of
/// appearance states.
fn collect_streams(store: &Store, entry: &Object, resources: &Dict, out: &mut Vec<(ObjId, Dict)>) {
    match entry {
        Object::Ref(id) => match store.resolve(entry) {
            Object::Stream(_) => out.push((*id, resources.clone())),
            Object::Dict(states) => {
                for (_, s) in states.iter() {
                    if let (Some(id), Object::Stream(_)) = (s.as_ref(), store.resolve(s)) {
                        out.push((id, resources.clone()));
                    }
                }
            }
            _ => {}
        },
        Object::Dict(states) => {
            for (_, s) in states.iter() {
                if let (Some(id), Object::Stream(_)) = (s.as_ref(), store.resolve(s)) {
                    out.push((id, resources.clone()));
                }
            }
        }
        _ => {}
    }
}

/// Removes `ids` from every `/XObject` resource dictionary.
fn drop_xobjects(doc: &mut Document, ids: &HashSet<ObjId>) {
    let filter = |map: &mut Dict| -> bool {
        let names: Vec<_> = map
            .iter()
            .filter(|(_, v)| v.as_ref().is_some_and(|id| ids.contains(&id)))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &names {
            map.remove(k.as_bytes());
        }
        !names.is_empty()
    };
    let mut changes: HashMap<ObjId, Object> = HashMap::new();
    for id in doc.store().object_ids() {
        let Ok(obj) = doc.store().get(id) else {
            continue;
        };
        let mut obj = (*obj).clone();
        let mut changed = false;
        let dict = match &mut obj {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&mut s.dict),
            _ => None,
        };
        let Some(dict) = dict else { continue };
        // `/Resources` direct or indirect, and its `/XObject` direct or
        // indirect.
        let resources = dict.get(b"Resources").cloned();
        match resources {
            Some(Object::Dict(mut res)) => match res.get(b"XObject").cloned() {
                Some(Object::Dict(mut map)) => {
                    if filter(&mut map) {
                        res.insert("XObject", map);
                        dict.insert("Resources", res);
                        changed = true;
                    }
                }
                Some(Object::Ref(map_id)) => {
                    if let Some(mut map) = doc.store().resolve_dict(&Object::Ref(map_id))
                        && filter(&mut map)
                    {
                        changes.insert(map_id, Object::Dict(map));
                    }
                }
                _ => {}
            },
            Some(Object::Ref(res_id)) => {
                if let Some(mut res) = doc.store().resolve_dict(&Object::Ref(res_id)) {
                    match res.get(b"XObject").cloned() {
                        Some(Object::Dict(mut map)) => {
                            if filter(&mut map) {
                                res.insert("XObject", map);
                                changes.insert(res_id, Object::Dict(res));
                            }
                        }
                        Some(Object::Ref(map_id)) => {
                            if let Some(mut map) = doc.store().resolve_dict(&Object::Ref(map_id))
                                && filter(&mut map)
                            {
                                changes.insert(map_id, Object::Dict(map));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if changed {
            changes.insert(id, obj);
        }
    }
    for (id, obj) in changes {
        doc.store_mut().set(id, obj);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use otf_pdf_syntax::{Name, OpenOptions, PdfString};

    /// Every code is 0.5 em wide (or 1 em tall, down, when vertical).
    #[derive(Debug)]
    struct Fixed(bool);

    impl Font for Fixed {
        fn codes(&self, bytes: &[u8]) -> Vec<(u32, usize)> {
            bytes.iter().map(|&b| (u32::from(b), 1)).collect()
        }
        fn width(&self, _: u32) -> f64 {
            0.5
        }
        fn is_vertical(&self) -> bool {
            self.0
        }
        fn unicode(&self, _: u32) -> Option<String> {
            None
        }
        fn name(&self) -> &str {
            "Fixed"
        }
    }

    struct Loader;

    impl FontLoader for Loader {
        fn load(&self, _: &Store, font: &Object) -> Option<Arc<dyn Font>> {
            let vertical = store_free_name(font) == Some(b"V".to_vec());
            Some(Arc::new(Fixed(vertical)))
        }
    }

    /// The font dictionary's `/Name`, read without a store (tests use
    /// direct dictionaries).
    fn store_free_name(font: &Object) -> Option<Vec<u8>> {
        font.as_dict()?.get_name(b"Name").map(<[u8]>::to_vec)
    }

    fn doc(objects: &str) -> Document {
        let bytes = format!(
            "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n{objects}"
        );
        Document::open(bytes.into_bytes(), OpenOptions::default()).unwrap()
    }

    fn cleaner<'a>(store: &'a Store, layers: Option<&'a Layers>) -> Cleaner<'a> {
        Cleaner {
            store,
            fonts: &Loader,
            layers,
            invisible_text: true,
            report: SanitizeReport::default(),
            walk: Walk::default(),
        }
    }

    fn resources(vertical: bool) -> Dict {
        let mut font = Dict::new();
        if vertical {
            font.insert("Name", Name::from("V"));
        }
        let mut fonts = Dict::new();
        fonts.insert("F", font);
        let mut r = Dict::new();
        r.insert("Font", fonts);
        r
    }

    fn text(ops: &[Operation]) -> String {
        String::from_utf8(write_content(ops).unwrap()).unwrap()
    }

    fn run(content: &[u8], vertical: bool) -> String {
        let d = doc("");
        let mut c = cleaner(d.store(), None);
        let ops = parse_content(content, d.store().limits()).unwrap();
        text(&c.clean(ops, &resources(vertical)).unwrap().unwrap())
    }

    #[test]
    fn invisible_text_keeps_its_advance() {
        // Size 10, Tc 1, Tw 2: "a b" advances (5+1) + (5+1+2) + (5+1) = 20,
        // which is TJ -2000.
        assert_eq!(
            run(b"BT /F 10 Tf 1 Tc 2 Tw 3 Tr (a b) Tj 0 Tr (x) Tj ET", false),
            "BT\n/F 10 Tf\n1 Tc\n2 Tw\n3 Tr\n[-2000] TJ\n0 Tr\n(x) Tj\nET\n"
        );
        // TJ keeps its own adjustments: the string's -500 plus the -100.
        assert!(run(b"BT /F 10 Tf 3 Tr [(a) -100] TJ ET", false).contains("[-600] TJ"));
        // ' and " keep their line moves and spacing.
        assert!(run(b"BT /F 10 Tf 3 Tr (a) ' ET", false).contains("T*\n[-500] TJ"));
        let quoted = run(b"BT /F 10 Tf 3 Tr 4 1 (a) \" ET", false);
        assert!(quoted.contains("4 Tw\n1 Tc\nT*\n[-600] TJ"), "{quoted}");
        // Vertical: one em down per code, so the TJ moves down 10.
        assert!(run(b"BT /F 10 Tf 3 Tr (a) Tj ET", true).contains("[1000] TJ"));
        // With no font size there is no advance to keep.
        assert!(!run(b"BT 3 Tr (a) Tj ET", false).contains("TJ"));
    }

    #[test]
    fn text_state_follows_save_and_restore() {
        // The q/Q restores mode 0, so the second string stays.
        let out = run(b"BT /F 10 Tf q 3 Tr (a) Tj Q (b) Tj ET", false);
        assert!(out.contains("(b) Tj") && !out.contains("(a)"), "{out}");
    }

    #[test]
    fn unchanged_content_reports_none() {
        let d = doc("");
        let mut c = cleaner(d.store(), None);
        let ops =
            parse_content(b"BT /F 10 Tf (a) Tj ET 0 0 m 1 1 l S", d.store().limits()).unwrap();
        assert!(c.clean(ops, &resources(false)).unwrap().is_none());
        assert_eq!(c.report, SanitizeReport::default());
    }

    #[test]
    fn hidden_sections_cut_drawing_and_keep_state() {
        let d = doc("10 0 obj << /Type /OCG /Name (on) >> endobj\n\
             11 0 obj << /Type /OCG /Name (off) >> endobj\n");
        let layers = Layers {
            off: [ObjId::new(11, 0)].into_iter().collect(),
        };
        let mut c = cleaner(d.store(), Some(&layers));
        let mut props = Dict::new();
        props.insert("On", ObjId::new(10, 0));
        props.insert("Off", ObjId::new(11, 0));
        let mut r = resources(false);
        r.insert("Properties", props);
        let ops = parse_content(
            b"/OC /On BDC 0 0 1 1 re f EMC \
              /OC /Off BDC 0.5 g 0 0 1 1 re f 0 0 2 2 re W n BT /F 10 Tf (s) Tj ET \
              /Span << >> BDC 9 9 m 8 8 l S EMC sh0 sh BI /W 1 ID x EI EMC \
              BT (after) Tj ET",
            d.store().limits(),
        )
        .unwrap();
        let out = text(&c.clean(ops, &r).unwrap().unwrap());
        // The visible layer is untouched.
        assert!(
            out.starts_with("/OC /On BDC\n0 0 1 1 re\nf\nEMC\n"),
            "{out}"
        );
        // State and the clip are kept, drawing is not.
        assert!(out.contains("0.5 g\n0 0 2 2 re\nW\nn\n"), "{out}");
        for gone in ["(s)", "9 9 m", "sh\n", "BI"] {
            assert!(!out.contains(gone), "{gone:?} in {out}");
        }
        // Only the visible layer's fill is left.
        assert_eq!(out.matches("\nf\n").count(), 1, "{out}");
        assert!(
            out.contains("[-500] TJ") && out.contains("(after) Tj"),
            "{out}"
        );
        // One hidden section (the nested /Span is inside it).
        assert_eq!(c.report.hidden_layers, 1);
    }

    #[test]
    fn layer_states_and_membership() {
        let d = doc("10 0 obj << /Type /OCG >> endobj\n\
             11 0 obj << /Type /OCG >> endobj\n\
             12 0 obj << /Type /OCG >> endobj\n\
             20 0 obj << /OCProperties << /OCGs [10 0 R 11 0 R 12 0 R] \
               /D << /BaseState /OFF /ON [10 0 R] >> >> >> endobj\n");
        let store = d.store();
        // BaseState OFF: everything not listed ON is off.
        let props = store.resolve_dict(&Object::Ref(ObjId::new(20, 0))).unwrap();
        let mut catalog = d.catalog();
        catalog.insert("OCProperties", props.get(b"OCProperties").unwrap().clone());
        let mut d2 = doc("10 0 obj << /Type /OCG >> endobj\n\
             11 0 obj << /Type /OCG >> endobj\n\
             12 0 obj << /Type /OCG >> endobj\n");
        let root = d2.catalog_id().unwrap();
        d2.store_mut().set(root, Object::Dict(catalog));
        let layers = Layers::of(&d2).unwrap();
        let g = |n| Object::Ref(ObjId::new(n, 0));
        assert!(!layers.hidden(store, &g(10)));
        assert!(layers.hidden(store, &g(11)) && layers.hidden(store, &g(12)));

        let ocmd = |extra: &str| {
            let text = format!("<< /Type /OCMD {extra} >>");
            otf_pdf_syntax::parser::Parser::new(
                text.as_bytes(),
                0,
                store.limits(),
                otf_pdf_syntax::Strictness::Lenient,
            )
            .parse_object()
            .unwrap()
        };
        // AnyOn is the default policy.
        assert!(!layers.hidden(store, &ocmd("/OCGs [10 0 R 11 0 R]")));
        assert!(layers.hidden(store, &ocmd("/OCGs [10 0 R 11 0 R] /P /AllOn")));
        assert!(!layers.hidden(store, &ocmd("/OCGs [10 0 R 11 0 R] /P /AnyOff")));
        assert!(layers.hidden(store, &ocmd("/OCGs [11 0 R 12 0 R] /P /AnyOn")));
        assert!(!layers.hidden(store, &ocmd("/OCGs [11 0 R 12 0 R] /P /AllOff")));
        assert!(!layers.hidden(store, &ocmd("/OCGs 10 0 R")));
        assert!(!layers.hidden(store, &ocmd("")));
        // Visibility expressions win over /OCGs.
        assert!(layers.hidden(store, &ocmd("/OCGs [10 0 R] /VE [/And 10 0 R 11 0 R]")));
        assert!(!layers.hidden(store, &ocmd("/VE [/Or 11 0 R [/Not 12 0 R]]")));
        assert!(layers.hidden(store, &ocmd("/VE [/Not 10 0 R]")));
        // Unknown shapes are visible: only what is clearly off is cut.
        assert!(!layers.hidden(store, &ocmd("/VE [/Xor 11 0 R]")));
        assert!(!layers.hidden(store, &Object::Null));
        // A document without optional content has no layer state at all.
        assert!(Layers::of(&doc("")).is_none());
        let _ = PdfString::literal("");
    }
}
