//! Saving (docs/PLAN.md §4, "Save").
//!
//! * [`SaveMode::Full`] rewrites the document: only objects reachable from
//!   the trailer are kept, they are renumbered densely in a deterministic
//!   order, unfiltered streams are compressed, non-stream objects are packed
//!   into object streams, and an xref stream indexes it all. It can encrypt
//!   with AES-256.
//! * [`SaveMode::Incremental`] copies the original bytes unchanged and
//!   appends only what changed plus a new xref section, as signatures need.
//!
//! Output is deterministic: the same document and options give the same
//! bytes. The document ID defaults to an MD5 of the content written, and
//! encryption draws its randomness from the injected [`Entropy`].

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Write as _;
use std::sync::Arc;

use md5::{Digest, Md5};
use otf_pdf_syntax::crypt::{CryptMethod, SecurityHandler};
use otf_pdf_syntax::filters::filter_chain;
use otf_pdf_syntax::serialize::write_object;
use otf_pdf_syntax::{
    Dict, Entropy, Error, LimitKind, Name, ObjId, Object, PdfString, Permissions, Result, Sink,
    Stream, Version,
};

use crate::document::Document;
use crate::pages::standalone_page_dict;
use crate::store::{Change, Store};

/// How to save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SaveMode {
    /// Rewrite and compact the whole document.
    #[default]
    Full,
    /// Append changes to the original bytes.
    Incremental,
}

/// Encryption for a full save: always AES-256 (revision 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encryption {
    /// The password needed to open the document (may be empty).
    pub user_password: Vec<u8>,
    /// The password that lifts the permission restrictions.
    pub owner_password: Vec<u8>,
    /// What a user-password reader may do.
    pub permissions: Permissions,
    /// Whether the XMP metadata stream is encrypted too.
    pub encrypt_metadata: bool,
}

/// Options for [`Document::save`].
#[derive(Clone)]
pub struct SaveOptions {
    /// Full rewrite or incremental append.
    pub mode: SaveMode,
    /// Output version for a full save (1.7 by default, or 2.0). An
    /// incremental save keeps the original header.
    pub version: Version,
    /// Pack non-stream objects into object streams with an xref stream
    /// (full save). Without it, a classic xref table is written.
    pub object_streams: bool,
    /// Flate-compress streams that have no filter.
    pub compress: bool,
    /// Encrypt the output (full save only).
    pub encryption: Option<Encryption>,
    /// The document ID to write; by default it is derived from the content.
    pub document_id: Option<[u8; 16]>,
    /// Randomness for encryption keys, salts and IVs. Required whenever
    /// something is encrypted.
    pub entropy: Option<Arc<dyn Entropy>>,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self {
            mode: SaveMode::Full,
            version: Version::V1_7,
            object_streams: true,
            compress: true,
            encryption: None,
            document_id: None,
            entropy: None,
        }
    }
}

impl std::fmt::Debug for SaveOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveOptions")
            .field("mode", &self.mode)
            .field("version", &self.version)
            .field("object_streams", &self.object_streams)
            .field("compress", &self.compress)
            .field("encryption", &self.encryption.is_some())
            .field("document_id", &self.document_id)
            .field("entropy", &self.entropy.is_some())
            .finish()
    }
}

/// Objects per object stream.
const OBJSTM_SIZE: usize = 100;
/// Streams shorter than this are not worth compressing.
const MIN_COMPRESS: usize = 32;

/// Counts bytes and hashes them (for the default document ID) on their
/// way to the sink.
struct Out<'s> {
    sink: &'s mut dyn Sink,
    pos: u64,
    hash: Md5,
}

impl Out<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.sink.write_all(bytes)?;
        self.hash.update(bytes);
        self.pos += bytes.len() as u64;
        Ok(())
    }

    fn digest(&self) -> [u8; 16] {
        self.hash.clone().finalize().into()
    }
}

/// Where each written object went.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Free(u16),
    At(u64, u16),
    In(u32, u32),
}

/// The store as a full save sees it: plus objects synthesized for the
/// output (a catalog given directly in the trailer, a rebuilt page tree).
struct View<'a> {
    store: &'a Store,
    extra: HashMap<ObjId, Object>,
    /// The catalog's id (new if it was a direct dictionary).
    root: ObjId,
}

impl View<'_> {
    fn get(&self, id: ObjId) -> Result<Arc<Object>> {
        match self.extra.get(&id) {
            Some(obj) => Ok(Arc::new(obj.clone())),
            None => self.store.get(id),
        }
    }
}

impl Document {
    /// Saves the document into `sink`.
    pub fn save_to(&self, sink: &mut dyn Sink, options: &SaveOptions) -> Result<()> {
        let mut out = Out {
            sink,
            pos: 0,
            hash: Md5::new(),
        };
        match options.mode {
            SaveMode::Full => self.save_full(&mut out, options),
            SaveMode::Incremental => self.save_incremental(&mut out, options),
        }
    }

    /// Saves the document and returns the bytes.
    pub fn save(&self, options: &SaveOptions) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.save_to(&mut bytes, options)?;
        Ok(bytes)
    }

    fn save_full(&self, out: &mut Out<'_>, options: &SaveOptions) -> Result<()> {
        let store = &self.store;
        let entropy = options.entropy.as_deref();
        let security = match &options.encryption {
            Some(e) => {
                let entropy = entropy.ok_or_else(needs_entropy)?;
                Some(SecurityHandler::new_aes256(
                    &e.user_password,
                    &e.owner_password,
                    e.permissions,
                    e.encrypt_metadata,
                    entropy,
                ))
            }
            None => None,
        };

        let trailer = store.trailer();
        let view = self.save_view()?;
        let root = view.root;

        // Reachable objects, breadth-first from the trailer, numbered densely.
        let mut roots = vec![root];
        roots.extend(trailer.get_ref(b"Info"));
        let (order, numbers) = reachable(&view, &roots)?;

        let mut prepared: Vec<(u32, Object)> = Vec::with_capacity(order.len());
        for old in &order {
            let mut obj = (*view.get(*old)?).clone();
            remap(&mut obj, &numbers);
            if *old == root
                && let Object::Dict(catalog) = &mut obj
            {
                catalog.remove(b"Version");
                if security.is_some() && options.version < Version::V2_0 {
                    catalog.insert("Extensions", aes256_extension());
                }
            }
            if let Object::Stream(s) = &mut obj {
                prepare_stream(s, options.compress)?;
            }
            // Every id in `order` was numbered by `reachable`.
            let Some(&num) = numbers.get(old) else {
                continue;
            };
            prepared.push((num, obj));
        }

        let mut slots: BTreeMap<u32, Slot> = BTreeMap::new();
        slots.insert(0, Slot::Free(65535));
        // The comment of bytes ≥ 128 marks the file as binary (§7.5.2).
        let mut header = format!("%PDF-{}\n%", options.version).into_bytes();
        header.extend_from_slice(&[0xe2, 0xe3, 0xcf, 0xd3, b'\n']);
        out.write(&header)?;

        let (direct, packed): (Vec<_>, Vec<_>) = prepared
            .into_iter()
            .partition(|(_, o)| !options.object_streams || matches!(o, Object::Stream(_)));
        let sec = security.as_ref().map(|(h, _)| h);
        for (num, mut obj) in direct {
            if let Some(h) = sec {
                encrypt_object(h, &mut obj, entropy.ok_or_else(needs_entropy)?)?;
            }
            slots.insert(num, Slot::At(out.pos, 0));
            write_indirect(out, ObjId::new(num, 0), &obj)?;
        }

        let mut next = order.len() as u32 + 1;
        for chunk in packed.chunks(OBJSTM_SIZE) {
            let stm_num = next;
            next += 1;
            let mut header = Vec::new();
            let mut body = Vec::new();
            for (index, (num, obj)) in chunk.iter().enumerate() {
                header.extend_from_slice(format!("{num} {} ", body.len()).as_bytes());
                write_object(obj, &mut body);
                body.push(b'\n');
                slots.insert(*num, Slot::In(stm_num, index as u32));
            }
            let mut dict = Dict::new();
            dict.insert("Type", Name::from("ObjStm"));
            dict.insert("N", chunk.len() as i64);
            dict.insert("First", header.len() as i64);
            header.extend_from_slice(&body);
            let mut stream = Object::Stream(Stream {
                dict,
                raw: header.into(),
            });
            if let Object::Stream(s) = &mut stream {
                compress(s)?;
            }
            if let Some(h) = sec {
                encrypt_object(h, &mut stream, entropy.ok_or_else(needs_entropy)?)?;
            }
            slots.insert(stm_num, Slot::At(out.pos, 0));
            write_indirect(out, ObjId::new(stm_num, 0), &stream)?;
        }

        let mut trailer_out = Dict::new();
        let root_num = numbers.get(&root).copied().ok_or(Error::NoRoot)?;
        trailer_out.insert("Root", ObjId::new(root_num, 0));
        if let Some(info) = trailer.get_ref(b"Info").and_then(|i| numbers.get(&i)) {
            trailer_out.insert("Info", ObjId::new(*info, 0));
        }
        if let Some((_, encrypt_dict)) = &security {
            let num = next;
            next += 1;
            slots.insert(num, Slot::At(out.pos, 0));
            write_indirect(out, ObjId::new(num, 0), &Object::Dict(encrypt_dict.clone()))?;
            trailer_out.insert("Encrypt", ObjId::new(num, 0));
        }
        let id = options.document_id.unwrap_or_else(|| out.digest());
        trailer_out.insert(
            "ID",
            vec![
                Object::String(PdfString::hex(id.to_vec())),
                Object::String(PdfString::hex(id.to_vec())),
            ],
        );
        write_xref(out, &slots, trailer_out, next, options.object_streams)
    }

    /// The view a full save writes from. A catalog given directly in the
    /// trailer gets an object number; a page tree that needed repair is
    /// replaced by a flat one holding the pages found, with their inherited
    /// attributes written into each page.
    fn save_view(&self) -> Result<View<'_>> {
        let store = &self.store;
        let mut extra = HashMap::new();
        let mut next = store.next_number();
        let mut fresh = || {
            let id = ObjId::new(next, 0);
            next += 1;
            id
        };
        let (root, mut catalog) = match store.trailer().get(b"Root") {
            Some(Object::Ref(id)) => (*id, self.catalog()),
            Some(Object::Dict(d)) => (fresh(), d.clone()),
            _ => return Err(Error::NoRoot),
        };
        let mut catalog_changed = !matches!(store.trailer().get(b"Root"), Some(Object::Ref(_)));

        let pages = self.pages()?;
        let repaired = self.diagnostics().iter().any(|d| {
            matches!(
                d.kind,
                otf_pdf_syntax::DiagnosticKind::PagesRecovered
                    | otf_pdf_syntax::DiagnosticKind::BadPageTree
            )
        });
        if repaired {
            let tree = fresh();
            let mut kids = Vec::with_capacity(pages.len());
            for page in pages.iter() {
                let id = page.id.unwrap_or_else(&mut fresh);
                let mut dict = standalone_page_dict(page);
                dict.insert("Parent", tree);
                extra.insert(id, Object::Dict(dict));
                kids.push(Object::Ref(id));
            }
            let mut node = Dict::new();
            node.insert("Type", Name::from("Pages"));
            node.insert("Count", kids.len() as i64);
            node.insert("Kids", kids);
            extra.insert(tree, Object::Dict(node));
            catalog.insert("Pages", tree);
            catalog_changed = true;
        }
        if catalog_changed {
            extra.insert(root, Object::Dict(catalog));
        }
        Ok(View { store, extra, root })
    }

    fn save_incremental(&self, out: &mut Out<'_>, options: &SaveOptions) -> Result<()> {
        let store = &self.store;
        let file = store.file();
        if options.encryption.is_some() {
            return Err(Error::UnsupportedEncryption(
                "encryption cannot change in an incremental save; save in full".into(),
            ));
        }
        if file.was_rebuilt() {
            return Err(Error::syntax(
                0,
                "the file was repaired, so there is no revision to append to; save in full",
            ));
        }
        let prev = file
            .startxref()?
            .ok_or_else(|| Error::syntax(0, "no startxref to append to; save in full"))?;
        let security = file.security();
        if let Some(sec) = security
            && !sec.can_encrypt()
        {
            return Err(Error::UnsupportedEncryption(
                "the file uses RC4 or AES-128, which is never written; save in full".into(),
            ));
        }
        let encrypt_ref = store.trailer().get_ref(b"Encrypt");

        let base = file.read_all()?;
        out.sink.write_all(&base)?;
        out.pos = base.len() as u64;
        if !base.ends_with(b"\n") && !base.ends_with(b"\r") {
            out.write(b"\n")?;
        }
        // The default ID part is a hash of what this update appends.
        out.hash = Md5::new();

        let mut slots: BTreeMap<u32, Slot> = BTreeMap::new();
        for (id, change) in store.overlay() {
            match change {
                Change::Set(obj) => {
                    let mut obj = (**obj).clone();
                    if let Object::Stream(s) = &mut obj {
                        prepare_stream(s, options.compress)?;
                    }
                    if let Some(sec) = security
                        && Some(*id) != encrypt_ref
                    {
                        let entropy = options.entropy.as_deref().ok_or_else(needs_entropy)?;
                        encrypt_object(sec, &mut obj, entropy)?;
                    }
                    slots.insert(id.num, Slot::At(out.pos, id.generation));
                    write_indirect(out, *id, &obj)?;
                }
                Change::Deleted => {
                    slots.insert(id.num, Slot::Free(id.generation.saturating_add(1)));
                }
            }
        }

        let mut trailer = store.trailer().clone();
        for key in [&b"Prev"[..], b"XRefStm"] {
            trailer.remove(key);
        }
        trailer.insert("Prev", prev as i64);
        let new_part = options.document_id.unwrap_or_else(|| out.digest());
        let first = trailer
            .get_array(b"ID")
            .and_then(|a| a.first().cloned())
            .filter(|o| o.as_string().is_some())
            .unwrap_or_else(|| Object::String(PdfString::hex(new_part.to_vec())));
        trailer.insert(
            "ID",
            vec![first, Object::String(PdfString::hex(new_part.to_vec()))],
        );
        let size = trailer
            .get_int(b"Size")
            .and_then(|s| u32::try_from(s).ok())
            .unwrap_or(0)
            .max(store.next_number());
        let as_stream = file.newest_section_is_stream()?.unwrap_or(false);
        write_xref(out, &slots, trailer, size, as_stream)
    }
}

/// Object ids reachable from `roots`, in breadth-first order, and their new
/// numbers. Objects that cannot be read are left out; references to them
/// become `null`.
fn reachable(view: &View<'_>, roots: &[ObjId]) -> Result<(Vec<ObjId>, HashMap<ObjId, u32>)> {
    let limits = view.store.limits();
    let mut order = Vec::new();
    let mut numbers = HashMap::new();
    let mut seen = HashSet::new();
    let mut queue: VecDeque<ObjId> = roots.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        limits.charge(1)?;
        let Ok(obj) = view.get(id) else { continue };
        if order.len() >= limits.max_objects as usize {
            return Err(Error::Limit(LimitKind::Objects));
        }
        order.push(id);
        numbers.insert(id, order.len() as u32);
        let mut refs = Vec::new();
        collect_refs(&obj, &mut refs);
        queue.extend(refs.into_iter().filter(|r| !seen.contains(r)));
    }
    Ok((order, numbers))
}

fn needs_entropy() -> Error {
    Error::UnsupportedEncryption("encrypting needs an entropy source (SaveOptions::entropy)".into())
}

/// `/Extensions` declaring AES-256 in a 1.7 file (Adobe extension level 8).
fn aes256_extension() -> Dict {
    let mut adbe = Dict::new();
    adbe.insert("BaseVersion", Name::from("1.7"));
    adbe.insert("ExtensionLevel", 8i64);
    let mut ext = Dict::new();
    ext.insert("ADBE", adbe);
    ext
}

fn collect_refs(object: &Object, out: &mut Vec<ObjId>) {
    match object {
        Object::Ref(id) => out.push(*id),
        Object::Array(items) => items.iter().for_each(|i| collect_refs(i, out)),
        Object::Dict(d) => d.iter().for_each(|(_, v)| collect_refs(v, out)),
        Object::Stream(s) => s.dict.iter().for_each(|(_, v)| collect_refs(v, out)),
        _ => {}
    }
}

/// Rewrites references through `numbers`; unknown targets become `null`.
fn remap(object: &mut Object, numbers: &HashMap<ObjId, u32>) {
    match object {
        Object::Ref(id) => {
            *object = match numbers.get(id) {
                Some(&n) => Object::Ref(ObjId::new(n, 0)),
                None => Object::Null,
            }
        }
        Object::Array(items) => items.iter_mut().for_each(|i| remap(i, numbers)),
        Object::Dict(d) => remap_dict(d, numbers),
        Object::Stream(s) => remap_dict(&mut s.dict, numbers),
        _ => {}
    }
}

fn remap_dict(dict: &mut Dict, numbers: &HashMap<ObjId, u32>) {
    let keys: Vec<Name> = dict.iter().map(|(k, _)| k.clone()).collect();
    for key in keys {
        if let Some(mut v) = dict.remove(key.as_bytes()) {
            remap(&mut v, numbers);
            dict.insert(key, v);
        }
    }
}

/// Drops `/Length` (rewritten from the data) and a `/Crypt` filter (the
/// data is already decrypted), and compresses unfiltered data.
fn prepare_stream(stream: &mut Stream, compress_unfiltered: bool) -> Result<()> {
    stream.dict.remove(b"Length");
    let chain = filter_chain(&stream.dict);
    if chain.iter().any(|f| f.name.as_bytes() == b"Crypt") {
        let kept: Vec<_> = chain
            .into_iter()
            .filter(|f| f.name.as_bytes() != b"Crypt")
            .collect();
        stream.dict.remove(b"Filter");
        stream.dict.remove(b"DecodeParms");
        if !kept.is_empty() {
            stream.dict.insert(
                "Filter",
                kept.iter()
                    .map(|f| Object::Name(f.name.clone()))
                    .collect::<Vec<_>>(),
            );
            if kept.iter().any(|f| f.params.is_some()) {
                stream.dict.insert(
                    "DecodeParms",
                    kept.into_iter()
                        .map(|f| f.params.map_or(Object::Null, Object::Dict))
                        .collect::<Vec<_>>(),
                );
            }
        }
    }
    let unfiltered = !stream.dict.contains_key(b"Filter");
    // XMP stays readable without decoding, as PDF/A requires.
    if compress_unfiltered
        && unfiltered
        && stream.raw.len() >= MIN_COMPRESS
        && !stream.dict.is_type(b"Metadata")
    {
        compress(stream)?;
    }
    Ok(())
}

fn compress(stream: &mut Stream) -> Result<()> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(&stream.raw)
        .and_then(|()| e.finish())
        .map(|data| {
            stream.raw = data.into();
            stream.dict.insert("Filter", Name::from("FlateDecode"));
        })
        .map_err(|_| Error::Filter("compression failed"))
}

/// Encrypts an object's strings and, for a stream, its data.
fn encrypt_object(sec: &SecurityHandler, obj: &mut Object, entropy: &dyn Entropy) -> Result<()> {
    sec.encrypt_strings(obj, entropy)?;
    if let Object::Stream(s) = obj {
        let clear =
            s.dict.is_type(b"XRef") || (s.dict.is_type(b"Metadata") && !sec.encrypt_metadata());
        let method = if clear {
            CryptMethod::Identity
        } else if s.dict.is_type(b"EmbeddedFile") {
            sec.embedded_file_method()
        } else {
            sec.stream_method()
        };
        s.raw = sec.encrypt(method, &s.raw, entropy)?.into();
    }
    Ok(())
}

fn write_indirect(out: &mut Out<'_>, id: ObjId, obj: &Object) -> Result<()> {
    let mut bytes = format!("{} {} obj\n", id.num, id.generation).into_bytes();
    write_object(obj, &mut bytes);
    bytes.extend_from_slice(b"\nendobj\n");
    out.write(&bytes)
}

/// Writes the xref section (stream or table), trailer, `startxref` and
/// `%%EOF`. `size` is one past the highest object number.
fn write_xref(
    out: &mut Out<'_>,
    slots: &BTreeMap<u32, Slot>,
    mut trailer: Dict,
    size: u32,
    as_stream: bool,
) -> Result<()> {
    let xref_at = out.pos;
    if as_stream {
        let own = size;
        let size = size + 1;
        let mut slots = slots.clone();
        slots.insert(own, Slot::At(xref_at, 0));
        let max_offset = slots
            .values()
            .map(|s| match *s {
                Slot::At(o, _) => o,
                Slot::In(n, i) => u64::from(n.max(i)),
                Slot::Free(_) => 0,
            })
            .max()
            .unwrap_or(0);
        let w1 = bytes_needed(max_offset);
        let w2 = 2;
        let mut data = Vec::new();
        let mut index = Vec::new();
        for (first, run) in runs(&slots) {
            index.push(Object::Int(i64::from(first)));
            index.push(Object::Int(run.len() as i64));
            for slot in run {
                let (kind, a, b) = match slot {
                    Slot::Free(generation) => (0u64, 0u64, u64::from(generation)),
                    Slot::At(offset, generation) => (1, offset, u64::from(generation)),
                    Slot::In(stream, i) => (2, u64::from(stream), u64::from(i)),
                };
                data.push(kind as u8);
                data.extend_from_slice(a.to_be_bytes().get(8 - w1..).unwrap_or_default());
                data.extend_from_slice(b.to_be_bytes().get(8 - w2..).unwrap_or_default());
            }
        }
        trailer.insert("Type", Name::from("XRef"));
        trailer.insert("Size", i64::from(size));
        trailer.insert(
            "W",
            vec![
                Object::Int(1),
                Object::Int(w1 as i64),
                Object::Int(w2 as i64),
            ],
        );
        trailer.insert("Index", index);
        let mut stream = Stream {
            dict: trailer,
            raw: data.into(),
        };
        compress(&mut stream)?;
        write_indirect(out, ObjId::new(own, 0), &Object::Stream(stream))?;
    } else {
        let mut table = b"xref\n".to_vec();
        for (first, run) in runs(slots) {
            table.extend_from_slice(format!("{first} {}\n", run.len()).as_bytes());
            for slot in run {
                let line = match slot {
                    Slot::Free(generation) => format!("0000000000 {generation:05} f\r\n"),
                    Slot::At(offset, generation) => format!("{offset:010} {generation:05} n\r\n"),
                    Slot::In(..) => format!("0000000000 {:05} f\r\n", 0),
                };
                table.extend_from_slice(line.as_bytes());
            }
        }
        trailer.insert("Size", i64::from(size));
        table.extend_from_slice(b"trailer\n");
        write_object(&Object::Dict(trailer), &mut table);
        table.push(b'\n');
        out.write(&table)?;
    }
    out.write(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes())
}

/// Splits slots into runs of consecutive object numbers.
fn runs(slots: &BTreeMap<u32, Slot>) -> Vec<(u32, Vec<Slot>)> {
    let mut out: Vec<(u32, Vec<Slot>)> = Vec::new();
    for (&num, &slot) in slots {
        match out.last_mut() {
            Some((first, run)) if *first + run.len() as u32 == num => run.push(slot),
            _ => out.push((num, vec![slot])),
        }
    }
    out
}

fn bytes_needed(value: u64) -> usize {
    (8 - value.leading_zeros() as usize / 8).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_and_runs() {
        assert_eq!(bytes_needed(0), 1);
        assert_eq!(bytes_needed(255), 1);
        assert_eq!(bytes_needed(256), 2);
        assert_eq!(bytes_needed(u64::MAX), 8);
        let mut slots = BTreeMap::new();
        for n in [0, 1, 2, 5, 6, 9] {
            slots.insert(n, Slot::Free(0));
        }
        let r: Vec<_> = runs(&slots)
            .into_iter()
            .map(|(f, r)| (f, r.len()))
            .collect();
        assert_eq!(r, [(0, 3), (5, 2), (9, 1)]);
    }

    #[test]
    fn remapping_drops_dangling_references() {
        let mut numbers = HashMap::new();
        numbers.insert(ObjId::new(7, 2), 1);
        let mut obj = Object::Array(vec![
            Object::Ref(ObjId::new(7, 2)),
            Object::Ref(ObjId::new(8, 0)),
        ]);
        remap(&mut obj, &numbers);
        assert_eq!(
            obj,
            Object::Array(vec![Object::Ref(ObjId::new(1, 0)), Object::Null])
        );
    }

    #[test]
    fn crypt_filter_is_dropped_and_small_streams_stay_raw() {
        let mut dict = Dict::new();
        dict.insert(
            "Filter",
            vec![
                Object::Name(Name::from("Crypt")),
                Object::Name(Name::from("FlateDecode")),
            ],
        );
        dict.insert("Length", 99i64);
        let mut s = Stream {
            dict,
            raw: b"x".to_vec().into(),
        };
        prepare_stream(&mut s, true).unwrap();
        assert_eq!(
            s.dict.get(b"Filter"),
            Some(&Object::Array(vec![Object::Name(Name::from(
                "FlateDecode"
            ))]))
        );
        assert!(s.dict.get(b"Length").is_none());

        let mut small = Stream {
            dict: Dict::new(),
            raw: b"tiny".to_vec().into(),
        };
        prepare_stream(&mut small, true).unwrap();
        assert!(small.dict.get(b"Filter").is_none());
    }
}
