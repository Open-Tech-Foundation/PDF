//! A document opened for reading.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use otf_pdf_syntax::{
    Diagnostic, Dict, Error, File, LimitKind, ObjId, Object, OpenOptions, Result, SharedSource,
    Source, Version,
};

use crate::pages::{Page, collect_pages};
use crate::store::Store;
use crate::text::decode_text_string;

/// The document information dictionary (§14.3.3), decoded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    /// `/Title`.
    pub title: Option<String>,
    /// `/Author`.
    pub author: Option<String>,
    /// `/Subject`.
    pub subject: Option<String>,
    /// `/Keywords`.
    pub keywords: Option<String>,
    /// `/Creator`.
    pub creator: Option<String>,
    /// `/Producer`.
    pub producer: Option<String>,
    /// `/CreationDate`, as written (`D:YYYYMMDDHHmmSSOHH'mm`).
    pub creation_date: Option<String>,
    /// `/ModDate`, as written.
    pub mod_date: Option<String>,
    /// `/Trapped`, as written.
    pub trapped: Option<String>,
}

/// One outline (bookmark) item.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    /// The item's id.
    pub id: Option<ObjId>,
    /// `/Title`, decoded.
    pub title: String,
    /// `/Dest`, resolved one level, if present.
    pub dest: Option<Object>,
    /// `/A`, the action, resolved one level, if present.
    pub action: Option<Dict>,
    /// Whether the item starts open (`/Count` positive).
    pub open: bool,
    /// Child items, in order.
    pub children: Vec<OutlineItem>,
}

/// An embedded file from the `/EmbeddedFiles` name tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// The key in the name tree.
    pub name: String,
    /// `/UF` or `/F` from the file specification.
    pub filename: Option<String>,
    /// `/Desc`.
    pub description: Option<String>,
    /// `/Subtype` of the embedded file stream (a MIME type, `#2F`-escaped).
    pub mime_type: Option<String>,
    /// `/Params /Size`, the declared uncompressed size.
    pub size: Option<u64>,
    /// The embedded file stream.
    pub stream: Option<ObjId>,
}

/// A document opened for reading.
#[derive(Debug)]
pub struct Document {
    pub(crate) store: Store,
    pages: OnceLock<std::result::Result<Arc<Vec<Page>>, Error>>,
}

impl Document {
    /// Opens a document from `source`.
    pub fn open(source: impl Source + Send + Sync + 'static, options: OpenOptions) -> Result<Self> {
        Self::open_shared(Arc::new(source), options)
    }

    /// Opens a document from a shared source.
    pub fn open_shared(source: SharedSource, options: OpenOptions) -> Result<Self> {
        let file = File::open(source, options)?;
        let root = file.trailer().get(b"Root").cloned().unwrap_or_default();
        file.resolve_dict(&root).ok_or(Error::NoRoot)?;
        Ok(Self {
            store: Store::new(file),
            pages: OnceLock::new(),
        })
    }

    /// The base file as read, without edits.
    pub fn file(&self) -> &File {
        self.store.file()
    }

    /// The object store: the base file plus edits.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Mutable access to the store for low-level edits. Cached derived data
    /// (the page list) is dropped, since any edit may change it.
    pub fn store_mut(&mut self) -> &mut Store {
        self.invalidate();
        &mut self.store
    }

    /// Drops cached derived data after an edit.
    pub(crate) fn invalidate(&mut self) {
        self.pages = OnceLock::new();
    }

    /// The catalog's id, if it is an indirect object (it always is in a
    /// valid file).
    pub fn catalog_id(&self) -> Option<ObjId> {
        self.store.trailer().get_ref(b"Root")
    }

    /// The document catalog, with edits.
    pub fn catalog(&self) -> Dict {
        self.store
            .trailer()
            .get(b"Root")
            .and_then(|r| self.store.resolve_dict(r))
            .unwrap_or_default()
    }

    /// The PDF version: the header's, or the catalog's `/Version` when that
    /// is later (§7.2.2).
    pub fn version(&self) -> Option<Version> {
        let header = self.file().header().map(|h| h.version);
        let catalog = self
            .catalog()
            .get_name(b"Version")
            .and_then(|v| otf_pdf_syntax::find_header(&[b"%PDF-", v].concat()[..]).ok())
            .map(|h| h.version);
        match (header, catalog) {
            (Some(h), Some(c)) => Some(h.max(c)),
            (h, c) => h.or(c),
        }
    }

    /// Repairs made while reading, so far.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.file().diagnostics()
    }

    /// Whether the file is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.file().security().is_some()
    }

    /// The pages, in order. Computed once.
    pub fn pages(&self) -> Result<Arc<Vec<Page>>> {
        self.pages
            .get_or_init(|| collect_pages(&self.store, &self.catalog()).map(Arc::new))
            .clone()
    }

    /// The number of pages.
    pub fn page_count(&self) -> Result<usize> {
        self.pages().map(|p| p.len())
    }

    /// The document information dictionary, decoded.
    pub fn info(&self) -> Info {
        let Some(dict) = self
            .store
            .trailer()
            .get(b"Info")
            .and_then(|i| self.store.resolve_dict(i))
        else {
            return Info::default();
        };
        let text = |key: &[u8]| match self.store.resolve(dict.get(key)?) {
            Object::String(s) => Some(decode_text_string(&s.bytes)),
            Object::Name(n) => Some(String::from_utf8_lossy(n.as_bytes()).into_owned()),
            _ => None,
        };
        Info {
            title: text(b"Title"),
            author: text(b"Author"),
            subject: text(b"Subject"),
            keywords: text(b"Keywords"),
            creator: text(b"Creator"),
            producer: text(b"Producer"),
            creation_date: text(b"CreationDate"),
            mod_date: text(b"ModDate"),
            trapped: text(b"Trapped"),
        }
    }

    /// The catalog's XMP metadata packet, decoded, if present.
    pub fn xmp_metadata(&self) -> Result<Option<Vec<u8>>> {
        let Some(meta) = self.catalog().get(b"Metadata").cloned() else {
            return Ok(None);
        };
        let Object::Stream(stream) = self.store.resolve(&meta) else {
            return Ok(None);
        };
        Ok(Some(self.store.decode_stream(&stream)?.data))
    }

    /// The outline (bookmark) tree.
    pub fn outlines(&self) -> Result<Vec<OutlineItem>> {
        let Some(root) = self
            .catalog()
            .get(b"Outlines")
            .and_then(|o| self.store.resolve_dict(o))
        else {
            return Ok(Vec::new());
        };
        let mut seen = HashSet::new();
        let mut budget = self.store.limits().max_objects as usize;
        self.outline_children(&root, 0, &mut seen, &mut budget)
    }

    fn outline_children(
        &self,
        parent: &Dict,
        depth: u32,
        seen: &mut HashSet<ObjId>,
        budget: &mut usize,
    ) -> Result<Vec<OutlineItem>> {
        if depth > self.store.limits().max_depth {
            return Err(Error::Limit(LimitKind::Depth));
        }
        let mut items = Vec::new();
        let mut next = parent.get(b"First").cloned();
        while let Some(node) = next.take() {
            let id = node.as_ref();
            // Cycles through /Next or /First end the walk there.
            if id.is_some_and(|id| !seen.insert(id)) {
                break;
            }
            *budget = budget
                .checked_sub(1)
                .ok_or(Error::Limit(LimitKind::Objects))?;
            self.store.limits().charge(1)?;
            let Some(dict) = self.store.resolve_dict(&node) else {
                break;
            };
            let title = match self
                .store
                .resolve(dict.get(b"Title").unwrap_or(&Object::Null))
            {
                Object::String(s) => decode_text_string(&s.bytes),
                _ => String::new(),
            };
            let children = self.outline_children(&dict, depth + 1, seen, budget)?;
            items.push(OutlineItem {
                id,
                title,
                dest: dict.get(b"Dest").map(|d| self.store.resolve(d)),
                action: dict.get(b"A").and_then(|a| self.store.resolve_dict(a)),
                open: dict.get_int(b"Count").is_some_and(|c| c > 0),
                children,
            });
            next = dict.get(b"Next").cloned();
        }
        Ok(items)
    }

    /// The embedded files listed in the `/EmbeddedFiles` name tree.
    pub fn attachments(&self) -> Result<Vec<Attachment>> {
        let Some(tree) = self
            .catalog()
            .get(b"Names")
            .and_then(|n| self.store.resolve_dict(n))
            .and_then(|names| names.get(b"EmbeddedFiles").cloned())
        else {
            return Ok(Vec::new());
        };
        let entries = name_tree(&self.store, &tree)?;
        Ok(entries
            .into_iter()
            .filter_map(|(key, value)| {
                let spec = self.store.resolve_dict(&value)?;
                let text = |k: &[u8]| match self.store.resolve(spec.get(k)?) {
                    Object::String(s) => Some(decode_text_string(&s.bytes)),
                    _ => None,
                };
                let ef = spec.get(b"EF").and_then(|e| self.store.resolve_dict(e));
                let stream_ref = ef
                    .as_ref()
                    .and_then(|ef| ef.get(b"UF").or_else(|| ef.get(b"F")))
                    .and_then(Object::as_ref);
                let stream_dict = stream_ref.and_then(|r| self.store.resolve_dict(&Object::Ref(r)));
                let params = stream_dict
                    .as_ref()
                    .and_then(|d| d.get(b"Params"))
                    .and_then(|p| self.store.resolve_dict(p));
                Some(Attachment {
                    name: decode_text_string(&key),
                    filename: text(b"UF").or_else(|| text(b"F")),
                    description: text(b"Desc"),
                    mime_type: stream_dict
                        .as_ref()
                        .and_then(|d| d.get_name(b"Subtype"))
                        .map(|n| String::from_utf8_lossy(n).into_owned()),
                    size: params
                        .and_then(|p| p.get_int(b"Size"))
                        .and_then(|s| u64::try_from(s).ok()),
                    stream: stream_ref,
                })
            })
            .collect())
    }

    /// The decoded contents of an attachment.
    pub fn attachment_data(&self, attachment: &Attachment) -> Result<Vec<u8>> {
        let id = attachment.stream.ok_or(Error::WrongType {
            expected: "embedded file stream",
            found: "null",
        })?;
        if let Some(sec) = self.file().security()
            && !sec.is_authenticated()
        {
            return Err(Error::InvalidPassword);
        }
        let obj = self.store.get(id)?;
        let stream = obj.as_stream().ok_or(Error::WrongType {
            expected: "stream",
            found: obj.type_name(),
        })?;
        Ok(self.store.decode_stream(stream)?.data)
    }
}

/// Every (key, value) pair of a name tree (§7.9.6), in tree order.
pub fn name_tree(file: &Store, root: &Object) -> Result<Vec<(Vec<u8>, Object)>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![(root.clone(), 0u32)];
    while let Some((node, depth)) = stack.pop() {
        if depth > file.limits().max_depth {
            return Err(Error::Limit(LimitKind::Depth));
        }
        if node.as_ref().is_some_and(|id| !seen.insert(id)) {
            continue;
        }
        file.limits().charge(1)?;
        let Some(dict) = file.resolve_dict(&node) else {
            continue;
        };
        if let Some(names) = dict.get(b"Names").map(|n| file.resolve(n)) {
            for pair in names.as_array().unwrap_or_default().chunks(2) {
                if let [key, value] = pair
                    && let Object::String(k) = file.resolve(key)
                {
                    out.push((k.bytes, value.clone()));
                }
            }
        }
        if let Some(kids) = dict.get(b"Kids").map(|k| file.resolve(k)) {
            for kid in kids.as_array().unwrap_or_default().iter().rev() {
                stack.push((kid.clone(), depth + 1));
            }
        }
    }
    Ok(out)
}
