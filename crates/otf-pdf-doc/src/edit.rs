//! Editing operations (docs/PLAN.md Phase 2): pages, outlines, named
//! destinations, metadata and attachments. Every edit goes into the
//! copy-on-write overlay, so the base file is untouched and an incremental
//! save appends exactly what changed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use otf_pdf_syntax::{Dict, Error, Name, ObjId, Object, OpenOptions, PdfString, Result, Stream};

use crate::document::{Document, Info, OutlineItem, name_tree};
use crate::pages::{Rect, standalone_page_dict};
use crate::store::Store;
use crate::text::{decode_text_string, encode_text_string};
use crate::xmp;

/// The bytes of an empty document (a catalog and an empty page tree) that
/// [`Document::new`] opens; offsets are computed, not hand-counted.
fn empty_pdf() -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for body in [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [] /Count 0 >>",
    ] {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", offsets.len()).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// Options for [`Document::add_attachment`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentOptions {
    /// MIME type, written as the embedded file's `/Subtype`.
    pub mime_type: Option<String>,
    /// `/Desc` of the file specification.
    pub description: Option<String>,
    /// Creation date as a PDF date string (`D:...`); no clock is read.
    pub creation_date: Option<String>,
    /// Modification date as a PDF date string.
    pub mod_date: Option<String>,
}

fn text(s: &str) -> Object {
    Object::String(PdfString::literal(encode_text_string(s)))
}

impl Document {
    /// A new, empty document (one empty page tree, no pages).
    pub fn new() -> Result<Self> {
        Self::open(empty_pdf(), OpenOptions::default())
    }

    /// Replaces the catalog with `catalog`, keeping its object number.
    fn set_catalog(&mut self, catalog: Dict) -> Result<()> {
        match self.catalog_id() {
            Some(id) => self.store_mut().set(id, Object::Dict(catalog)),
            None => {
                let id = self.store_mut().add(Object::Dict(catalog))?;
                self.store_mut().set_trailer_entry("Root", Object::Ref(id));
            }
        }
        Ok(())
    }

    // ---- Pages -------------------------------------------------------------

    /// Makes the page tree exactly `pages` (dictionaries already standalone,
    /// with their ids where they have one), as one flat `/Pages` node.
    fn write_page_tree(&mut self, pages: Vec<(Option<ObjId>, Dict)>) -> Result<()> {
        let catalog = self.catalog();
        let tree = match catalog.get(b"Pages") {
            Some(Object::Ref(id)) => *id,
            _ => self.store_mut().allocate()?,
        };
        let mut kids = Vec::with_capacity(pages.len());
        let mut used = HashSet::new();
        for (id, mut dict) in pages {
            dict.insert("Parent", tree);
            // A page listed twice becomes two objects.
            let id = match id {
                Some(id) if used.insert(id) => id,
                _ => self.store_mut().allocate()?,
            };
            self.store_mut().set(id, Object::Dict(dict));
            kids.push(Object::Ref(id));
        }
        let mut node = Dict::new();
        node.insert("Type", Name::from("Pages"));
        node.insert("Count", kids.len() as i64);
        node.insert("Kids", kids);
        self.store_mut().set(tree, Object::Dict(node));
        if catalog.get_ref(b"Pages") != Some(tree) {
            let mut catalog = catalog;
            catalog.insert("Pages", tree);
            self.set_catalog(catalog)?;
        }
        self.invalidate();
        Ok(())
    }

    /// Keeps the pages at `indices` (0-based), in that order. Pages left out
    /// are removed along with outline items and named destinations that
    /// point to them; a page listed twice is duplicated.
    pub fn select_pages(&mut self, indices: &[usize]) -> Result<()> {
        let pages = self.pages()?;
        let mut chosen = Vec::with_capacity(indices.len());
        for &i in indices {
            let page = pages
                .get(i)
                .ok_or(Error::syntax(0, "page index out of range"))?;
            chosen.push((page.id, standalone_page_dict(page)));
        }
        let kept: HashSet<ObjId> = chosen.iter().filter_map(|(id, _)| *id).collect();
        let removed: HashSet<ObjId> = pages
            .iter()
            .filter_map(|p| p.id)
            .filter(|id| !kept.contains(id))
            .collect();
        self.write_page_tree(chosen)?;
        if !removed.is_empty() {
            for id in &removed {
                self.store_mut().delete(*id);
            }
            self.remove_destinations_to(&removed)?;
        }
        Ok(())
    }

    /// Moves pages into `order`, a permutation of `0..page_count`.
    pub fn reorder_pages(&mut self, order: &[usize]) -> Result<()> {
        let count = self.page_count()?;
        let mut sorted = order.to_vec();
        sorted.sort_unstable();
        if sorted != (0..count).collect::<Vec<_>>() {
            return Err(Error::syntax(
                0,
                "page order must be a permutation of every page",
            ));
        }
        self.select_pages(order)
    }

    /// Deletes the pages at `indices`, cleaning up outline items and named
    /// destinations that point to them.
    pub fn delete_pages(&mut self, indices: &[usize]) -> Result<()> {
        let count = self.page_count()?;
        let drop: HashSet<usize> = indices.iter().copied().collect();
        if drop.iter().any(|&i| i >= count) {
            return Err(Error::syntax(0, "page index out of range"));
        }
        let keep: Vec<usize> = (0..count).filter(|i| !drop.contains(i)).collect();
        self.select_pages(&keep)
    }

    /// Edits one page's own dictionary through `f`.
    fn edit_page(&mut self, index: usize, f: impl FnOnce(&mut Dict)) -> Result<()> {
        let pages = self.pages()?;
        let page = pages
            .get(index)
            .ok_or(Error::syntax(0, "page index out of range"))?;
        let mut dict = page.dict.clone();
        f(&mut dict);
        match page.id {
            Some(id) => self.store_mut().set(id, Object::Dict(dict)),
            // A direct page dictionary: rebuild the tree to give it a number.
            None => {
                let mut all: Vec<_> = pages
                    .iter()
                    .map(|p| (p.id, standalone_page_dict(p)))
                    .collect();
                if let Some(slot) = all.get_mut(index) {
                    slot.1 = dict;
                }
                self.write_page_tree(all)?;
            }
        }
        self.invalidate();
        Ok(())
    }

    /// Rotates a page clockwise by `degrees` (a multiple of 90) on top of
    /// its current rotation.
    pub fn rotate_page(&mut self, index: usize, degrees: i32) -> Result<()> {
        if degrees % 90 != 0 {
            return Err(Error::syntax(0, "rotation must be a multiple of 90"));
        }
        let current = i32::from(
            self.pages()?
                .get(index)
                .ok_or(Error::syntax(0, "page index out of range"))?
                .rotate,
        );
        let rotate = (current + degrees).rem_euclid(360);
        self.edit_page(index, |d| {
            d.insert("Rotate", i64::from(rotate));
        })
    }

    /// Sets a page's crop box (the visible region).
    pub fn set_crop_box(&mut self, index: usize, rect: Rect) -> Result<()> {
        self.edit_page(index, |d| {
            d.insert("CropBox", rect.to_object());
        })
    }

    /// Sets a page's media box (the physical page).
    pub fn set_media_box(&mut self, index: usize, rect: Rect) -> Result<()> {
        self.edit_page(index, |d| {
            d.insert("MediaBox", rect.to_object());
        })
    }

    /// Appends pages of `other` (all of them when `indices` is `None`),
    /// copying every object they use. Objects shared by several imported
    /// pages are copied once.
    pub fn append_pages_from(&mut self, other: &Document, indices: Option<&[usize]>) -> Result<()> {
        let theirs = other.pages()?;
        let wanted: Vec<usize> = match indices {
            Some(i) => i.to_vec(),
            None => (0..theirs.len()).collect(),
        };
        let mut map: HashMap<ObjId, ObjId> = HashMap::new();
        // Pages first, so annotations' /P and links resolve to the copies.
        let mut new_pages = Vec::with_capacity(wanted.len());
        for &i in &wanted {
            let page = theirs
                .get(i)
                .ok_or(Error::syntax(0, "page index out of range"))?;
            let id = self.store_mut().allocate()?;
            if let Some(src) = page.id {
                map.entry(src).or_insert(id);
            }
            new_pages.push((id, page));
        }
        let mut pages: Vec<(Option<ObjId>, Dict)> = self
            .pages()?
            .iter()
            .map(|p| (p.id, standalone_page_dict(p)))
            .collect();
        for (id, page) in new_pages {
            let mut dict = standalone_page_dict(page);
            // The structure tree and article threads are not copied.
            for key in [&b"StructParents"[..], b"B"] {
                dict.remove(key);
            }
            let copied = import(
                &mut self.store,
                other.store(),
                &Object::Dict(dict),
                &mut map,
            )?;
            let Object::Dict(dict) = copied else {
                return Err(Error::syntax(0, "page did not copy as a dictionary"));
            };
            pages.push((Some(id), dict));
        }
        self.write_page_tree(pages)
    }

    /// A new document holding copies of the pages at `indices`.
    pub fn extract_pages(&self, indices: &[usize]) -> Result<Document> {
        let mut out = Document::new()?;
        out.append_pages_from(self, Some(indices))?;
        Ok(out)
    }

    // ---- Destinations ------------------------------------------------------

    /// Removes outline items and named destinations that target `pages`.
    fn remove_destinations_to(&mut self, pages: &HashSet<ObjId>) -> Result<()> {
        let named = self.named_destinations()?;
        let dead_names: HashSet<Vec<u8>> = named
            .iter()
            .filter(|(_, dest)| targets(dest, pages, &HashMap::new(), self.store()))
            .map(|(k, _)| k.clone())
            .collect();
        let named_map: HashMap<Vec<u8>, Object> = named.into_iter().collect();

        let outlines = self.outlines()?;
        let (kept, changed) = prune_outlines(outlines, &|item: &OutlineItem| {
            let dest = item.dest.clone().or_else(|| {
                item.action
                    .as_ref()
                    .filter(|a| a.get_name(b"S") == Some(b"GoTo"))
                    .and_then(|a| a.get(b"D").cloned())
            });
            dest.is_some_and(|d| targets(&d, pages, &named_map, self.store()))
        });
        if changed {
            self.set_outlines(&kept)?;
        }
        if !dead_names.is_empty() {
            self.remove_named_destinations(&dead_names)?;
        }
        Ok(())
    }

    /// Named destinations from `/Dests` (PDF 1.1) and the `/Names /Dests`
    /// name tree, keyed by name.
    pub fn named_destinations(&self) -> Result<Vec<(Vec<u8>, Object)>> {
        let catalog = self.catalog();
        let store = self.store();
        let mut out = Vec::new();
        if let Some(dests) = catalog.get(b"Dests").and_then(|d| store.resolve_dict(d)) {
            for (k, v) in dests.iter() {
                out.push((k.as_bytes().to_vec(), store.resolve(v)));
            }
        }
        if let Some(tree) = catalog
            .get(b"Names")
            .and_then(|n| store.resolve_dict(n))
            .and_then(|n| n.get(b"Dests").cloned())
        {
            for (k, v) in name_tree(store, &tree)? {
                out.push((k, store.resolve(&v)));
            }
        }
        Ok(out)
    }

    fn remove_named_destinations(&mut self, dead: &HashSet<Vec<u8>>) -> Result<()> {
        let mut catalog = self.catalog();
        if let Some(dests_obj) = catalog.get(b"Dests").cloned()
            && let Some(mut dests) = self.store().resolve_dict(&dests_obj)
        {
            for name in dead {
                dests.remove(name);
            }
            match dests_obj {
                Object::Ref(id) => self.store_mut().set(id, Object::Dict(dests)),
                _ => {
                    catalog.insert("Dests", dests);
                }
            }
        }
        let names_tree = catalog
            .get(b"Names")
            .and_then(|n| self.store().resolve_dict(n))
            .and_then(|n| n.get(b"Dests").cloned());
        if let Some(tree) = names_tree {
            let entries: Vec<_> = name_tree(self.store(), &tree)?
                .into_iter()
                .filter(|(k, _)| !dead.contains(k))
                .collect();
            catalog = self.with_name_tree(catalog, b"Dests", entries)?;
        }
        self.set_catalog(catalog)
    }

    /// Returns `catalog` with `/Names /<tree>` replaced by a flat tree of
    /// `entries` (sorted by key), or removed when empty.
    fn with_name_tree(
        &mut self,
        mut catalog: Dict,
        tree: &[u8],
        mut entries: Vec<(Vec<u8>, Object)>,
    ) -> Result<Dict> {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        let names_obj = catalog.get(b"Names").cloned();
        let mut names = names_obj
            .as_ref()
            .and_then(|n| self.store().resolve_dict(n))
            .unwrap_or_default();
        if entries.is_empty() {
            names.remove(tree);
        } else {
            let mut flat = Vec::with_capacity(entries.len() * 2);
            for (k, v) in entries {
                flat.push(Object::String(PdfString::literal(k)));
                flat.push(v);
            }
            let mut node = Dict::new();
            node.insert("Names", flat);
            let id = self.store_mut().add(Object::Dict(node))?;
            names.insert(Name::new(tree.to_vec()), Object::Ref(id));
        }
        match names_obj {
            Some(Object::Ref(id)) => self.store_mut().set(id, Object::Dict(names)),
            _ if names.is_empty() => {
                catalog.remove(b"Names");
            }
            _ => {
                catalog.insert("Names", names);
            }
        }
        Ok(catalog)
    }

    // ---- Outlines ----------------------------------------------------------

    /// Replaces the outline tree with `items` (ids in the items are ignored;
    /// fresh objects are written). An empty list removes the outline.
    pub fn set_outlines(&mut self, items: &[OutlineItem]) -> Result<()> {
        let mut catalog = self.catalog();
        if items.is_empty() {
            catalog.remove(b"Outlines");
            return self.set_catalog(catalog);
        }
        let root = self.store_mut().allocate()?;
        let (first, last, visible) = self.write_outline_level(root, items)?;
        let mut dict = Dict::new();
        dict.insert("Type", Name::from("Outlines"));
        dict.insert("First", first);
        dict.insert("Last", last);
        dict.insert("Count", visible as i64);
        self.store_mut().set(root, Object::Dict(dict));
        catalog.insert("Outlines", root);
        self.set_catalog(catalog)
    }

    /// Writes one sibling list under `parent`; returns its first and last
    /// ids and how many items it shows when its parent is open.
    fn write_outline_level(
        &mut self,
        parent: ObjId,
        items: &[OutlineItem],
    ) -> Result<(ObjId, ObjId, usize)> {
        let ids: Vec<ObjId> = items
            .iter()
            .map(|_| self.store_mut().allocate())
            .collect::<Result<_>>()?;
        let mut visible = 0;
        for (i, (item, &id)) in items.iter().zip(&ids).enumerate() {
            let mut dict = item.extra.clone();
            dict.insert("Title", text(&item.title));
            dict.insert("Parent", parent);
            if i > 0
                && let Some(prev) = ids.get(i - 1)
            {
                dict.insert("Prev", *prev);
            }
            if let Some(next) = ids.get(i + 1) {
                dict.insert("Next", *next);
            }
            if let Some(dest) = &item.dest {
                dict.insert("Dest", dest.clone());
            }
            if let Some(action) = &item.action {
                dict.insert("A", action.clone());
            }
            visible += 1;
            if !item.children.is_empty() {
                let (first, last, below) = self.write_outline_level(id, &item.children)?;
                dict.insert("First", first);
                dict.insert("Last", last);
                // Positive: open, showing `below` descendants; negative:
                // closed, hiding them (§12.3.3).
                if item.open {
                    dict.insert("Count", below as i64);
                    visible += below;
                } else {
                    dict.insert("Count", -(below as i64));
                }
            }
            self.store_mut().set(id, Object::Dict(dict));
        }
        let first = *ids.first().ok_or(Error::syntax(0, "empty outline level"))?;
        let last = *ids.last().ok_or(Error::syntax(0, "empty outline level"))?;
        Ok((first, last, visible))
    }

    // ---- Metadata ----------------------------------------------------------

    /// Replaces the document information: fields that are `None` are
    /// removed, other entries of the Info dictionary are kept. The XMP
    /// packet is rewritten to match (D23).
    pub fn set_info(&mut self, info: &Info) -> Result<()> {
        let info_obj = self.store().trailer().get(b"Info").cloned();
        let mut dict = info_obj
            .as_ref()
            .and_then(|i| self.store().resolve_dict(i))
            .unwrap_or_default();
        for (key, value) in [
            ("Title", &info.title),
            ("Author", &info.author),
            ("Subject", &info.subject),
            ("Keywords", &info.keywords),
            ("Creator", &info.creator),
            ("Producer", &info.producer),
            ("CreationDate", &info.creation_date),
            ("ModDate", &info.mod_date),
        ] {
            match value {
                // Dates are ASCII by construction; keep them plain.
                Some(v) => {
                    dict.insert(key, text(v));
                }
                None => {
                    dict.remove(key.as_bytes());
                }
            }
        }
        match &info.trapped {
            Some(t) => {
                dict.insert("Trapped", Name::from(t.as_str()));
            }
            None => {
                dict.remove(b"Trapped");
            }
        }
        match info_obj {
            Some(Object::Ref(id)) => self.store_mut().set(id, Object::Dict(dict)),
            _ => {
                let id = self.store_mut().add(Object::Dict(dict))?;
                self.store_mut().set_trailer_entry("Info", Object::Ref(id));
            }
        }
        let previous = self.xmp_metadata()?;
        let packet = xmp::packet(info, previous.as_deref());
        let mut meta = Dict::new();
        meta.insert("Type", Name::from("Metadata"));
        meta.insert("Subtype", Name::from("XML"));
        let stream = Object::Stream(Stream {
            dict: meta,
            raw: packet.into_bytes().into(),
        });
        let mut catalog = self.catalog();
        match catalog.get(b"Metadata") {
            Some(Object::Ref(id)) => {
                let id = *id;
                self.store_mut().set(id, stream);
            }
            _ => {
                let id = self.store_mut().add(stream)?;
                catalog.insert("Metadata", id);
                self.set_catalog(catalog)?;
            }
        }
        Ok(())
    }

    // ---- Attachments -------------------------------------------------------

    /// Embeds `data` as an attachment named `name`, replacing one of the same
    /// name.
    pub fn add_attachment(
        &mut self,
        name: &str,
        data: &[u8],
        options: &AttachmentOptions,
    ) -> Result<()> {
        let mut params = Dict::new();
        params.insert("Size", data.len() as i64);
        if let Some(d) = &options.creation_date {
            params.insert("CreationDate", text(d));
        }
        if let Some(d) = &options.mod_date {
            params.insert("ModDate", text(d));
        }
        let mut ef_dict = Dict::new();
        ef_dict.insert("Type", Name::from("EmbeddedFile"));
        if let Some(mime) = &options.mime_type {
            ef_dict.insert("Subtype", Name::from(mime.as_str()));
        }
        ef_dict.insert("Params", params);
        let stream = self.store_mut().add(Object::Stream(Stream {
            dict: ef_dict,
            raw: Arc::from(data),
        }))?;
        let mut ef = Dict::new();
        ef.insert("F", stream);
        ef.insert("UF", stream);
        let mut spec = Dict::new();
        spec.insert("Type", Name::from("Filespec"));
        // /F is a byte string for old readers; /UF the Unicode name.
        let ascii: String = name
            .chars()
            .map(|c| if c.is_ascii() { c } else { '_' })
            .collect();
        spec.insert("F", PdfString::literal(ascii.into_bytes()));
        spec.insert("UF", text(name));
        spec.insert("EF", ef);
        if let Some(desc) = &options.description {
            spec.insert("Desc", text(desc));
        }
        let spec = self.store_mut().add(Object::Dict(spec))?;
        let key = encode_text_string(name);
        let mut entries = self.embedded_files()?;
        entries.retain(|(k, _)| *k != key);
        entries.push((key, Object::Ref(spec)));
        let catalog = self.catalog();
        let catalog = self.with_name_tree(catalog, b"EmbeddedFiles", entries)?;
        self.set_catalog(catalog)
    }

    /// Removes the attachment named `name`; returns whether it existed.
    pub fn remove_attachment(&mut self, name: &str) -> Result<bool> {
        let mut entries = self.embedded_files()?;
        let before = entries.len();
        entries.retain(|(k, _)| decode_text_string(k) != name);
        if entries.len() == before {
            return Ok(false);
        }
        let catalog = self.catalog();
        let catalog = self.with_name_tree(catalog, b"EmbeddedFiles", entries)?;
        self.set_catalog(catalog)?;
        Ok(true)
    }

    fn embedded_files(&self) -> Result<Vec<(Vec<u8>, Object)>> {
        let tree = self
            .catalog()
            .get(b"Names")
            .and_then(|n| self.store().resolve_dict(n))
            .and_then(|n| n.get(b"EmbeddedFiles").cloned());
        match tree {
            Some(t) => name_tree(self.store(), &t),
            None => Ok(Vec::new()),
        }
    }
}

/// Whether a destination (explicit array, name or string) targets one of
/// `pages`.
fn targets(
    dest: &Object,
    pages: &HashSet<ObjId>,
    named: &HashMap<Vec<u8>, Object>,
    store: &Store,
) -> bool {
    let explicit = match dest {
        Object::Name(n) => named.get(n.as_bytes()).map(|d| store.resolve(d)),
        Object::String(s) => named.get(&s.bytes).map(|d| store.resolve(d)),
        other => Some(store.resolve(other)),
    };
    let array = match explicit {
        // A named destination may be a dictionary with /D.
        Some(Object::Dict(d)) => d.get(b"D").map(|x| store.resolve(x)),
        other => other,
    };
    array
        .as_ref()
        .and_then(Object::as_array)
        .and_then(|a| a.first())
        .and_then(Object::as_ref)
        .is_some_and(|page| pages.contains(&page))
}

/// Drops items for which `dead` holds, promoting their children; returns the
/// pruned list and whether anything changed.
fn prune_outlines(
    items: Vec<OutlineItem>,
    dead: &dyn Fn(&OutlineItem) -> bool,
) -> (Vec<OutlineItem>, bool) {
    let mut out = Vec::with_capacity(items.len());
    let mut changed = false;
    for mut item in items {
        let (children, child_changed) = prune_outlines(std::mem::take(&mut item.children), dead);
        changed |= child_changed;
        if dead(&item) {
            changed = true;
            out.extend(children);
        } else {
            item.children = children;
            out.push(item);
        }
    }
    (out, changed)
}

/// Deep-copies `object` from `src` into `dst`, giving every referenced
/// object a new number in `dst`. `map` carries ids already copied (and
/// pre-assigned ones), so shared objects are copied once and cycles end.
pub fn import(
    dst: &mut Store,
    src: &Store,
    object: &Object,
    map: &mut HashMap<ObjId, ObjId>,
) -> Result<Object> {
    let mut queue: VecDeque<ObjId> = VecDeque::new();
    let top = remap_into(dst, object, map, &mut queue)?;
    while let Some(src_id) = queue.pop_front() {
        dst.limits().charge(1)?;
        let Some(&dst_id) = map.get(&src_id) else {
            continue;
        };
        let obj = match src.get(src_id) {
            Ok(o) => (*o).clone(),
            Err(_) => Object::Null,
        };
        let copied = remap_into(dst, &obj, map, &mut queue)?;
        dst.set(dst_id, copied);
    }
    Ok(top)
}

/// Copies `object`, rewriting references through `map` and allocating (and
/// queueing for copy) any not seen yet.
fn remap_into(
    dst: &mut Store,
    object: &Object,
    map: &mut HashMap<ObjId, ObjId>,
    queue: &mut VecDeque<ObjId>,
) -> Result<Object> {
    Ok(match object {
        Object::Ref(id) => {
            let new = match map.get(id) {
                Some(n) => *n,
                None => {
                    let n = dst.allocate()?;
                    map.insert(*id, n);
                    queue.push_back(*id);
                    n
                }
            };
            Object::Ref(new)
        }
        Object::Array(items) => Object::Array(
            items
                .iter()
                .map(|i| remap_into(dst, i, map, queue))
                .collect::<Result<_>>()?,
        ),
        Object::Dict(d) => Object::Dict(remap_dict_into(dst, d, map, queue)?),
        Object::Stream(s) => Object::Stream(Stream {
            dict: remap_dict_into(dst, &s.dict, map, queue)?,
            raw: Arc::clone(&s.raw),
        }),
        other => other.clone(),
    })
}

fn remap_dict_into(
    dst: &mut Store,
    dict: &Dict,
    map: &mut HashMap<ObjId, ObjId>,
    queue: &mut VecDeque<ObjId>,
) -> Result<Dict> {
    dict.iter()
        .map(|(k, v)| Ok((k.clone(), remap_into(dst, v, map, queue)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, children: Vec<OutlineItem>) -> OutlineItem {
        OutlineItem {
            id: None,
            title: title.into(),
            dest: None,
            action: None,
            open: true,
            children,
            extra: Dict::new(),
        }
    }

    #[test]
    fn empty_document() {
        let doc = Document::new().unwrap();
        assert_eq!(doc.page_count().unwrap(), 0);
        assert!(doc.diagnostics().is_empty(), "{:?}", doc.diagnostics());
    }

    #[test]
    fn pruning_promotes_children() {
        let tree = vec![
            item("a", vec![item("a1", vec![]), item("x", vec![])]),
            item("x", vec![item("x1", vec![])]),
        ];
        let (kept, changed) = prune_outlines(tree, &|i| i.title == "x");
        assert!(changed);
        let titles: Vec<_> = kept.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["a", "x1"]);
        assert_eq!(kept[0].children.len(), 1);
        let (_, unchanged) = prune_outlines(vec![item("a", vec![])], &|_| false);
        assert!(!unchanged);
    }
}
