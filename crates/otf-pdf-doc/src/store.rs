//! The copy-on-write object store: an immutable base [`File`] plus an
//! overlay of changed, added and deleted objects (docs/PLAN.md §4, "Overlay
//! layer"). Reads check the overlay first; the base is never modified, which
//! is what lets an incremental save copy its bytes unchanged.

use std::collections::BTreeMap;
use std::sync::Arc;

use otf_pdf_syntax::filters::Decoded;
use otf_pdf_syntax::xref::XrefEntry;
use otf_pdf_syntax::{
    Diagnostics, Dict, Error, File, LimitKind, Limits, ObjId, Object, Result, Stream, Strictness,
};

/// One overlay entry.
#[derive(Debug, Clone)]
pub(crate) enum Change {
    /// The object was replaced or added.
    Set(Arc<Object>),
    /// The object was deleted.
    Deleted,
}

/// Base file plus overlay.
#[derive(Debug)]
pub struct Store {
    file: File,
    overlay: BTreeMap<ObjId, Change>,
    trailer: Dict,
    next_num: u32,
}

impl Store {
    pub(crate) fn new(file: File) -> Self {
        let trailer = file.trailer().clone();
        let highest = file.entries().keys().next_back().copied().unwrap_or(0);
        let size = trailer
            .get_int(b"Size")
            .and_then(|s| u32::try_from(s).ok())
            .unwrap_or(0);
        Self {
            file,
            overlay: BTreeMap::new(),
            trailer,
            next_num: highest.saturating_add(1).max(size).max(1),
        }
    }

    /// The base file.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// The working trailer: the base trailer plus edits.
    pub fn trailer(&self) -> &Dict {
        &self.trailer
    }

    /// Sets a trailer entry (`/Info`, `/Root`, `/ID`). `null` removes it.
    pub fn set_trailer_entry(&mut self, key: &str, value: Object) {
        if value == Object::Null {
            self.trailer.remove(key.as_bytes());
        } else {
            self.trailer.insert(key, value);
        }
    }

    pub(crate) fn overlay(&self) -> &BTreeMap<ObjId, Change> {
        &self.overlay
    }

    /// Whether anything was changed.
    pub fn is_modified(&self) -> bool {
        !self.overlay.is_empty() || &self.trailer != self.file.trailer()
    }

    /// The object `id`, from the overlay or the base.
    pub fn get(&self, id: ObjId) -> Result<Arc<Object>> {
        match self.overlay.get(&id) {
            Some(Change::Set(obj)) => Ok(Arc::clone(obj)),
            Some(Change::Deleted) => Err(Error::MissingObject(id)),
            None => self.file.get(id),
        }
    }

    /// Follows references until a direct object; missing objects and overlong
    /// chains resolve to `null`.
    pub fn resolve(&self, object: &Object) -> Object {
        let mut current = object.clone();
        for _ in 0..self.file.limits().max_depth {
            match current {
                Object::Ref(id) => match self.get(id) {
                    Ok(o) => current = (*o).clone(),
                    Err(_) => return Object::Null,
                },
                other => return other,
            }
        }
        Object::Null
    }

    /// Resolves `object` as a dictionary (or a stream's dictionary).
    pub fn resolve_dict(&self, object: &Object) -> Option<Dict> {
        match self.resolve(object) {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(s.dict),
            _ => None,
        }
    }

    /// Decodes a stream through its filters, resolving indirect filter
    /// parameters through the overlay.
    pub fn decode_stream(&self, stream: &Stream) -> Result<Decoded> {
        let mut dict = stream.dict.clone();
        for key in [&b"Filter"[..], b"DecodeParms"] {
            if let Some(v) = dict.get(key).cloned() {
                let resolved = match self.resolve(&v) {
                    Object::Array(items) => {
                        Object::Array(items.iter().map(|i| self.resolve(i)).collect())
                    }
                    other => other,
                };
                dict.insert(key, resolved);
            }
        }
        otf_pdf_syntax::filters::decode(
            &stream.raw,
            &otf_pdf_syntax::filters::filter_chain(&dict),
            self.file.limits(),
            self.file.strictness(),
            Some(self.file.diagnostics_sink()),
        )
    }

    /// Every live object id, base and overlay, in order.
    pub fn object_ids(&self) -> Vec<ObjId> {
        let mut ids: BTreeMap<u32, ObjId> = BTreeMap::new();
        for (&num, entry) in self.file.entries() {
            match *entry {
                XrefEntry::InUse { generation, .. } => {
                    ids.insert(num, ObjId::new(num, generation));
                }
                XrefEntry::Compressed { .. } => {
                    ids.insert(num, ObjId::new(num, 0));
                }
                XrefEntry::Free => {}
            }
        }
        for (id, change) in &self.overlay {
            match change {
                Change::Set(_) => {
                    ids.insert(id.num, *id);
                }
                Change::Deleted => {
                    ids.remove(&id.num);
                }
            }
        }
        ids.into_values().collect()
    }

    /// Replaces (or adds) object `id`.
    pub fn set(&mut self, id: ObjId, object: Object) {
        if id.num >= self.next_num {
            self.next_num = id.num.saturating_add(1);
        }
        self.overlay.insert(id, Change::Set(Arc::new(object)));
    }

    /// Adds a new object and returns its id.
    pub fn add(&mut self, object: Object) -> Result<ObjId> {
        let id = self.allocate()?;
        self.set(id, object);
        Ok(id)
    }

    /// Reserves a new object number (generation 0) without an object yet;
    /// [`Store::set`] fills it. Used to break cycles while copying.
    pub fn allocate(&mut self) -> Result<ObjId> {
        if self.next_num > self.file.limits().max_objects {
            return Err(Error::Limit(LimitKind::Objects));
        }
        let id = ObjId::new(self.next_num, 0);
        self.next_num += 1;
        // Reserve with null so a cycle back to it during a copy resolves.
        self.overlay.insert(id, Change::Set(Arc::new(Object::Null)));
        Ok(id)
    }

    /// Deletes object `id`.
    pub fn delete(&mut self, id: ObjId) {
        self.overlay.insert(id, Change::Deleted);
    }

    /// The next object number that would be allocated.
    pub fn next_number(&self) -> u32 {
        self.next_num
    }

    /// The limits in force.
    pub fn limits(&self) -> &Limits {
        self.file.limits()
    }

    /// Whether repairs are allowed.
    pub fn strictness(&self) -> Strictness {
        self.file.strictness()
    }

    /// Where repairs are recorded.
    pub fn diagnostics_sink(&self) -> &Diagnostics {
        self.file.diagnostics_sink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use otf_pdf_syntax::OpenOptions;

    fn store() -> Store {
        let bytes = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
            2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n3 0 obj (base) endobj\n"
            .to_vec();
        Store::new(File::open(Arc::new(bytes), OpenOptions::default()).unwrap())
    }

    #[test]
    fn overlay_shadows_base() {
        let mut s = store();
        assert!(!s.is_modified());
        let three = ObjId::new(3, 0);
        assert_eq!(s.get(three).unwrap().as_string(), Some(&b"base"[..]));
        s.set(three, Object::Int(7));
        assert_eq!(s.get(three).unwrap().as_int(), Some(7));
        assert_eq!(s.file().get(three).unwrap().as_string(), Some(&b"base"[..]));
        assert!(s.is_modified());
        s.delete(three);
        assert!(s.get(three).is_err());
        assert_eq!(s.resolve(&Object::Ref(three)), Object::Null);
        assert_eq!(s.object_ids(), vec![ObjId::new(1, 0), ObjId::new(2, 0)]);
    }

    #[test]
    fn allocation_continues_after_the_base() {
        let mut s = store();
        let id = s.add(Object::Bool(true)).unwrap();
        assert_eq!(id, ObjId::new(4, 0));
        assert_eq!(s.next_number(), 5);
        let reserved = s.allocate().unwrap();
        assert_eq!(s.get(reserved).unwrap().as_ref(), &Object::Null);
        s.set(ObjId::new(10, 0), Object::Null);
        assert_eq!(s.next_number(), 11);
        assert_eq!(s.object_ids().len(), 6);
    }

    #[test]
    fn allocation_is_limited() {
        let bytes = b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n".to_vec();
        let options = OpenOptions {
            limits: Limits {
                max_objects: 3,
                ..Limits::default()
            },
            ..OpenOptions::default()
        };
        let mut s = Store::new(File::open(Arc::new(bytes), options).unwrap());
        assert!(s.add(Object::Null).is_ok());
        assert!(s.add(Object::Null).is_ok());
        assert_eq!(s.add(Object::Null), Err(Error::Limit(LimitKind::Objects)));
    }
}
