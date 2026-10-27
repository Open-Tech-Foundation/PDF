//! A PDF file opened for reading: header, xref chain, trailer, decryption
//! and lazy, cached access to objects, with repair when the structure is
//! broken (docs/PLAN.md §4, "Base layer" and "Lenient parsing").

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use crate::crypt::{CryptMethod, SecurityHandler};
use crate::diag::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};
use crate::filters::{self, Decoded};
use crate::header::{Header, find_header};
use crate::lexer::{Lexer, Token, is_regular, is_whitespace};
use crate::limits::{LimitKind, Limits, Strictness};
use crate::object::{Dict, ObjId, Object, Stream};
use crate::parser::{ObjectEnd, Parser};
use crate::source::Source;
use crate::xref::{self, XrefEntry, XrefMap};

/// How to open a file.
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Password for encrypted files, tried as user then owner password. The
    /// empty password is always tried when this is empty.
    pub password: Vec<u8>,
    /// Whether repairs are allowed.
    pub strictness: Strictness,
    /// Resource limits.
    pub limits: Limits,
}

/// A source that can be shared across threads.
pub type SharedSource = Arc<dyn Source + Send + Sync>;

/// An object stream, decoded once and cached.
#[derive(Debug)]
struct ObjStm {
    data: Vec<u8>,
    first: usize,
    /// (object number, offset relative to `first`), in stream order.
    index: Vec<(u32, usize)>,
}

/// What a whole-file scan found.
#[derive(Debug, Default)]
struct Scan {
    entries: XrefMap,
    trailer: Option<Dict>,
}

/// Initial window for parsing one object; grown on demand.
const WINDOW: u64 = 4096;
/// Chunk size for whole-file scans.
const SCAN_CHUNK: u64 = 1 << 20;
/// Nesting of loads (a `/Length` reference, an object stream) allowed.
const MAX_LOAD_DEPTH: u32 = 8;

/// A PDF file opened for reading. Objects are parsed on first access and
/// cached; the file itself is never modified.
pub struct File {
    source: SharedSource,
    len: u64,
    header: Option<Header>,
    /// Offset of the header: xref offsets in files with leading junk are
    /// usually relative to it.
    base: u64,
    entries: XrefMap,
    trailer: Dict,
    /// Offsets of the xref sections read, newest first.
    sections: Vec<u64>,
    scan: OnceLock<Scan>,
    cache: Mutex<HashMap<ObjId, Arc<Object>>>,
    objstms: Mutex<HashMap<u32, Arc<ObjStm>>>,
    security: Option<SecurityHandler>,
    encrypt_ref: Option<ObjId>,
    limits: Limits,
    strictness: Strictness,
    diagnostics: Diagnostics,
}

impl std::fmt::Debug for File {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("File")
            .field("len", &self.len)
            .field("header", &self.header)
            .field("objects", &self.entries.len())
            .field("encrypted", &self.security.is_some())
            .finish_non_exhaustive()
    }
}

impl File {
    /// Opens `source`.
    pub fn open(source: SharedSource, options: OpenOptions) -> Result<Self> {
        let len = source.len();
        let mut file = Self {
            source,
            len,
            header: None,
            base: 0,
            entries: XrefMap::new(),
            trailer: Dict::new(),
            sections: Vec::new(),
            scan: OnceLock::new(),
            cache: Mutex::new(HashMap::new()),
            objstms: Mutex::new(HashMap::new()),
            security: None,
            encrypt_ref: None,
            limits: options.limits,
            strictness: options.strictness,
            diagnostics: Diagnostics::default(),
        };

        match find_header(&*file.source) {
            Ok(h) => {
                if h.has_leading_junk() {
                    file.repair(DiagnosticKind::JunkBeforeHeader, Some(h.offset))?;
                }
                file.base = h.offset;
                file.header = Some(h);
            }
            Err(crate::header::HeaderError::Source(e)) => return Err(Error::Source(e)),
            Err(_) => file.repair(DiagnosticKind::MissingHeader, Some(0))?,
        }

        match file.load_xref_chain() {
            Ok((entries, trailer, sections)) => {
                file.entries = entries;
                file.trailer = trailer;
                file.sections = sections;
            }
            Err(e @ (Error::Source(_) | Error::Limit(LimitKind::Budget))) => return Err(e),
            Err(e) => {
                if file.strictness == Strictness::Strict {
                    return Err(e);
                }
                file.adopt_scan()?;
            }
        }

        // Decryption first: the catalog may sit in an encrypted object
        // stream, and a wrong password must say so rather than "no catalog".
        file.init_security(&options.password)?;

        // A trailer without a usable catalog: the scan may find one.
        if !file.root_is_usable() {
            if file.strictness == Strictness::Strict {
                return Err(Error::NoRoot);
            }
            file.adopt_scan()?;
            if !file.root_is_usable() {
                return Err(Error::NoRoot);
            }
        }
        Ok(file)
    }

    /// Replaces the xref and trailer with what a whole-file scan finds.
    fn adopt_scan(&mut self) -> Result<()> {
        let scan = self.scan();
        if scan.entries.is_empty() {
            return Err(Error::NoXref);
        }
        let entries = scan.entries.clone();
        let scanned_trailer = scan.trailer.clone();
        self.diagnostics
            .push(DiagnosticKind::XrefRebuilt, None, None);
        // Keep what the xref said about objects the scan did not see (e.g.
        // ones in object streams), but trust the scan for the rest.
        for (num, entry) in entries {
            self.entries.insert(num, entry);
        }
        if let Some(t) = scanned_trailer {
            // Keep keys like /ID and /Encrypt that the found trailer lacks.
            let mut merged = t;
            for (k, v) in self.trailer.iter() {
                if !merged.contains_key(k.as_bytes()) {
                    merged.insert(k.clone(), v.clone());
                }
            }
            self.trailer = merged;
            self.diagnostics
                .push(DiagnosticKind::TrailerRecovered, None, None);
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
        Ok(())
    }

    fn root_is_usable(&self) -> bool {
        match self.trailer.get(b"Root") {
            Some(Object::Ref(id)) => self
                .load(*id, 0)
                .is_ok_and(|o| o.as_dict().is_some_and(|d| !d.is_empty())),
            Some(Object::Dict(_)) => true,
            _ => false,
        }
    }

    fn init_security(&mut self, password: &[u8]) -> Result<()> {
        let encrypt = match self.trailer.get(b"Encrypt") {
            None => return Ok(()),
            Some(Object::Ref(id)) => {
                self.encrypt_ref = Some(*id);
                // Loaded before `security` is set, so it is not decrypted.
                let obj = self.load(*id, 0)?;
                obj.as_dict().cloned().ok_or(Error::WrongType {
                    expected: "dictionary",
                    found: obj.type_name(),
                })?
            }
            Some(Object::Dict(d)) => d.clone(),
            Some(other) => {
                return Err(Error::WrongType {
                    expected: "dictionary",
                    found: other.type_name(),
                });
            }
        };
        let id0 = self
            .trailer
            .get_array(b"ID")
            .and_then(|a| a.first())
            .and_then(Object::as_string)
            .unwrap_or_default()
            .to_vec();
        let handler = SecurityHandler::new(&encrypt, &id0, password)?;
        self.security = Some(handler);
        // Anything cached so far was read without decryption.
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
        if let Ok(mut objstms) = self.objstms.lock() {
            objstms.clear();
        }
        Ok(())
    }

    /// Records a repair, or fails in strict mode.
    fn repair(&self, kind: DiagnosticKind, offset: Option<u64>) -> Result<()> {
        if self.strictness == Strictness::Strict {
            return Err(Error::syntax(
                offset.unwrap_or(0),
                "repair needed in strict mode",
            ));
        }
        self.diagnostics.push(kind, offset, None);
        Ok(())
    }

    // ---- Accessors -------------------------------------------------------

    /// The header, if one was found.
    pub fn header(&self) -> Option<Header> {
        self.header
    }

    /// The (merged) trailer dictionary.
    pub fn trailer(&self) -> &Dict {
        &self.trailer
    }

    /// The xref entries, by object number.
    pub fn entries(&self) -> &XrefMap {
        &self.entries
    }

    /// Offsets of the xref sections read, newest first. One per revision
    /// for an incrementally updated file.
    pub fn sections(&self) -> &[u64] {
        &self.sections
    }

    /// The value written after the last `startxref`, as the file states it:
    /// what an incremental update's `/Prev` must point to.
    pub fn startxref(&self) -> Result<Option<u64>> {
        self.find_startxref()
    }

    /// Whether the newest xref section is an xref stream (rather than a
    /// classic table); `None` when no section was read.
    pub fn newest_section_is_stream(&self) -> Result<Option<bool>> {
        let Some(&at) = self.sections.first() else {
            return Ok(None);
        };
        let head = self.read(at, 16)?;
        let start = head.iter().position(|&b| !is_whitespace(b)).unwrap_or(0);
        Ok(Some(
            !head.get(start..).is_some_and(|h| h.starts_with(b"xref")),
        ))
    }

    /// Whether the xref had to be rebuilt by scanning. Such a file has no
    /// trustworthy revision chain to append to.
    pub fn was_rebuilt(&self) -> bool {
        self.diagnostics
            .snapshot()
            .iter()
            .any(|d| d.kind == DiagnosticKind::XrefRebuilt)
    }

    /// The whole source, as an incremental save copies it.
    pub fn read_all(&self) -> Result<Vec<u8>> {
        let bytes = self.read(0, self.len)?;
        if (bytes.len() as u64) < self.len {
            return Err(Error::Source(crate::source::SourceError::new(
                "source returned fewer bytes than its length",
            )));
        }
        Ok(bytes)
    }

    /// The security handler, if the file is encrypted.
    pub fn security(&self) -> Option<&SecurityHandler> {
        self.security.as_ref()
    }

    /// Repairs made so far.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics.snapshot()
    }

    /// The diagnostics sink, for higher layers to add to.
    pub fn diagnostics_sink(&self) -> &Diagnostics {
        &self.diagnostics
    }

    /// The limits in force.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Whether repairs are allowed.
    pub fn strictness(&self) -> Strictness {
        self.strictness
    }

    /// File length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    // ---- Objects ---------------------------------------------------------

    /// The object `id`, decrypted. A missing object is an error here; use
    /// [`File::resolve`] for the specification's "missing is null".
    pub fn get(&self, id: ObjId) -> Result<Arc<Object>> {
        if let Some(obj) = self.cache.lock().ok().and_then(|c| c.get(&id).cloned()) {
            return Ok(obj);
        }
        self.limits.charge(1)?;
        let obj = Arc::new(self.load_decrypted(id, 0)?);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(id, Arc::clone(&obj));
        }
        Ok(obj)
    }

    /// Follows references until a direct object. Missing objects and
    /// reference chains longer than the depth limit resolve to `null`.
    pub fn resolve(&self, object: &Object) -> Object {
        let mut current = object.clone();
        for _ in 0..self.limits.max_depth {
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

    /// Decodes a stream's data through its filters. Image codecs are left
    /// encoded and listed in [`Decoded::remaining`].
    pub fn decode_stream(&self, stream: &Stream) -> Result<Decoded> {
        let mut dict = stream.dict.clone();
        for key in [&b"Filter"[..], b"DecodeParms"] {
            if let Some(v) = dict.get(key).cloned() {
                let resolved = self.resolve_shallow(&v);
                dict.insert(key, resolved);
            }
        }
        filters::decode(
            &stream.raw,
            &filters::filter_chain(&dict),
            &self.limits,
            self.strictness,
            Some(&self.diagnostics),
        )
    }

    /// Resolves `object` and any references directly inside it (one level).
    fn resolve_shallow(&self, object: &Object) -> Object {
        match self.resolve(object) {
            Object::Array(items) => Object::Array(items.iter().map(|i| self.resolve(i)).collect()),
            other => other,
        }
    }

    fn load_decrypted(&self, id: ObjId, depth: u32) -> Result<Object> {
        let (mut obj, direct) = self.load_with_origin(id, depth)?;
        // Objects inside object streams were decrypted with their stream.
        if direct
            && Some(id) != self.encrypt_ref
            && let Some(sec) = &self.security
        {
            self.decrypt(sec, id, &mut obj);
        }
        Ok(obj)
    }

    fn decrypt(&self, sec: &SecurityHandler, id: ObjId, obj: &mut Object) {
        sec.decrypt_strings(id, obj);
        if let Object::Stream(s) = obj {
            let method = self.stream_method(sec, &s.dict);
            s.raw = sec.decrypt(id, method, &s.raw).into();
        }
    }

    fn stream_method(&self, sec: &SecurityHandler, dict: &Dict) -> CryptMethod {
        if dict.is_type(b"XRef") {
            return CryptMethod::Identity;
        }
        if dict.is_type(b"Metadata") && !sec.encrypt_metadata() {
            return CryptMethod::Identity;
        }
        // A /Crypt filter names its own crypt filter (§7.4.10); only the
        // Identity one is common, and it means "not encrypted".
        let crypt = filters::filter_chain(dict)
            .into_iter()
            .find(|f| f.name.as_bytes() == b"Crypt");
        if let Some(f) = crypt {
            let name = f.params.as_ref().and_then(|p| p.get_name(b"Name"));
            if name.is_none_or(|n| n == b"Identity") {
                return CryptMethod::Identity;
            }
        }
        if dict.is_type(b"EmbeddedFile") {
            return sec.embedded_file_method();
        }
        sec.stream_method()
    }

    /// Loads an object without decryption (used directly for the `/Encrypt`
    /// dictionary and while bootstrapping).
    fn load(&self, id: ObjId, depth: u32) -> Result<Object> {
        self.load_with_origin(id, depth).map(|(o, _)| o)
    }

    /// Loads an object; the flag says whether it was written directly (and
    /// so needs decrypting) rather than inside an object stream.
    fn load_with_origin(&self, id: ObjId, depth: u32) -> Result<(Object, bool)> {
        if depth > MAX_LOAD_DEPTH {
            return Err(Error::Limit(LimitKind::Depth));
        }
        let entry = self.entries.get(&id.num).copied();
        let result = match entry {
            Some(XrefEntry::InUse { offset, generation }) => {
                if generation != id.generation {
                    self.repair(DiagnosticKind::ObjectIdMismatch, Some(offset))?;
                }
                self.parse_at_xref_offset(id, offset, depth)
                    .map(|o| (o, true))
            }
            Some(XrefEntry::Compressed { stream, index }) => self
                .load_compressed(id, stream, index, depth)
                .map(|o| (o, false)),
            Some(XrefEntry::Free) | None => Err(Error::MissingObject(id)),
        };
        match result {
            Ok(found) => Ok(found),
            Err(e @ (Error::Limit(_) | Error::Source(_))) => Err(e),
            Err(e) if self.strictness == Strictness::Strict => Err(e),
            // Explicitly free (deleted in a later revision): the old bytes
            // still in the file must not resurrect it.
            Err(e) if entry == Some(XrefEntry::Free) => Err(e),
            Err(e) => {
                // The xref is wrong or incomplete: look where a scan found it.
                let scanned = self.scan().entries.get(&id.num).copied();
                match scanned {
                    Some(XrefEntry::InUse { offset, .. }) if !matches!(entry, Some(XrefEntry::InUse { offset: o, .. }) if o == offset) =>
                    {
                        let obj = self.parse_at(offset, Some(id), depth)?;
                        if entry.is_some() {
                            self.diagnostics.push(
                                DiagnosticKind::BadXrefEntry,
                                Some(offset),
                                Some(id),
                            );
                        }
                        Ok((obj, true))
                    }
                    Some(XrefEntry::Compressed { stream, index })
                        if !matches!(entry, Some(XrefEntry::Compressed { .. })) =>
                    {
                        Ok((self.load_compressed(id, stream, index, depth)?, false))
                    }
                    _ => Err(e),
                }
            }
        }
    }

    fn parse_at_xref_offset(&self, id: ObjId, offset: u64, depth: u32) -> Result<Object> {
        match self.parse_at(offset, Some(id), depth) {
            Err(e) if self.base > 0 => self
                .parse_at(offset + self.base, Some(id), depth)
                .map_err(|_| e),
            other => other,
        }
    }

    /// Reads `len` bytes at `offset` (fewer at the end of the file).
    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let len = len.min(self.len.saturating_sub(offset));
        let mut buf =
            vec![0u8; usize::try_from(len).map_err(|_| Error::Limit(LimitKind::ObjectSize))?];
        let mut filled = 0;
        while filled < buf.len() {
            let Some(rest) = buf.get_mut(filled..) else {
                break;
            };
            let n = self.source.read_at(offset + filled as u64, rest)?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        Ok(buf)
    }

    /// Runs `parse` over a window at `offset`, growing the window while the
    /// parse runs out of input before the end of the file.
    fn with_window<T>(&self, offset: u64, mut parse: impl FnMut(&[u8]) -> Result<T>) -> Result<T> {
        let mut size = WINDOW;
        loop {
            let buf = self.read(offset, size)?;
            match parse(&buf) {
                Err(Error::UnexpectedEof) if offset + size < self.len => {
                    if size >= self.limits.max_object_size {
                        return Err(Error::Limit(LimitKind::ObjectSize));
                    }
                    size = (size * 4).min(self.limits.max_object_size);
                }
                other => return other,
            }
        }
    }

    /// Parses the indirect object at `offset`, including stream data.
    fn parse_at(&self, offset: u64, expected: Option<ObjId>, depth: u32) -> Result<Object> {
        if offset >= self.len {
            return Err(Error::syntax(offset, "object offset past end of file"));
        }
        let (id, object, end) = self.with_window(offset, |buf| {
            Parser::new(buf, offset, &self.limits, self.strictness)
                .with_diagnostics(&self.diagnostics)
                .parse_indirect(expected)
        })?;
        let ObjectEnd::Stream { data_start } = end else {
            return Ok(object);
        };
        let Object::Dict(dict) = object else {
            return Err(Error::syntax(offset, "stream without dictionary"));
        };
        let data_offset = offset + data_start as u64;
        let declared = match dict.get(b"Length") {
            Some(Object::Int(n)) => u64::try_from(*n).ok(),
            Some(Object::Ref(r)) if *r != id => self
                .load_decrypted(*r, depth + 1)
                .ok()
                .and_then(|o| o.as_int())
                .and_then(|n| u64::try_from(n).ok()),
            _ => None,
        };
        let length = match declared {
            Some(n) if self.endstream_at(data_offset.saturating_add(n))? => n,
            _ => {
                self.repair(DiagnosticKind::BadStreamLength, Some(data_offset))?;
                self.find_stream_end(data_offset)?
            }
        };
        let raw = self.read(data_offset, length)?;
        Ok(Object::Stream(Stream {
            dict,
            raw: raw.into(),
        }))
    }

    /// Whether `endstream` follows `pos` (after optional whitespace).
    fn endstream_at(&self, pos: u64) -> Result<bool> {
        if pos > self.len {
            return Ok(false);
        }
        let buf = self.read(pos, 32)?;
        let start = buf
            .iter()
            .position(|&b| !is_whitespace(b))
            .unwrap_or(buf.len());
        Ok(buf
            .get(start..)
            .is_some_and(|r| r.starts_with(b"endstream")))
    }

    /// Finds the end of stream data starting at `start` by scanning for
    /// `endstream` (or, failing that, `endobj`), dropping the end-of-line
    /// marker before it.
    fn find_stream_end(&self, start: u64) -> Result<u64> {
        let found = self
            .find_forward(start, b"endstream")?
            .or(self.find_forward(start, b"endobj")?);
        let Some(end) = found else {
            return Ok(self.len.saturating_sub(start));
        };
        let tail = self.read(end.saturating_sub(2), 2.min(end))?;
        let eol = match tail.as_slice() {
            [b'\r', b'\n'] => 2,
            [.., b'\n' | b'\r'] => 1,
            _ => 0,
        };
        Ok(end.saturating_sub(eol).saturating_sub(start))
    }

    /// The offset of the first `needle` at or after `from`.
    fn find_forward(&self, from: u64, needle: &[u8]) -> Result<Option<u64>> {
        let overlap = needle.len() as u64;
        let mut pos = from;
        while pos < self.len {
            let chunk = self.read(pos, SCAN_CHUNK)?;
            if let Some(i) = chunk.windows(needle.len()).position(|w| w == needle) {
                return Ok(Some(pos + i as u64));
            }
            if chunk.len() as u64 <= overlap {
                break;
            }
            pos += chunk.len() as u64 - overlap;
        }
        Ok(None)
    }

    fn load_compressed(&self, id: ObjId, stream: u32, index: u32, depth: u32) -> Result<Object> {
        if stream == id.num {
            return Err(Error::syntax(0, "object stream contains itself"));
        }
        let objstm = self.objstm(stream, depth + 1)?;
        let by_index = objstm
            .index
            .get(index as usize)
            .filter(|(n, _)| *n == id.num);
        // Lenient: the index may be wrong while the number is right.
        let entry = match by_index {
            Some(e) => Some(e),
            None => objstm.index.iter().find(|(n, _)| *n == id.num),
        };
        let Some(&(_, offset)) = entry else {
            return Err(Error::MissingObject(id));
        };
        let start = objstm.first.saturating_add(offset);
        let buf = objstm.data.get(start..).unwrap_or_default();
        Parser::new(buf, 0, &self.limits, self.strictness)
            .with_diagnostics(&self.diagnostics)
            .parse_object()
    }

    fn objstm(&self, num: u32, depth: u32) -> Result<Arc<ObjStm>> {
        if let Some(s) = self.objstms.lock().ok().and_then(|m| m.get(&num).cloned()) {
            return Ok(s);
        }
        let id = ObjId::new(num, 0);
        let obj = self.load_decrypted(id, depth)?;
        let Object::Stream(stream) = obj else {
            return Err(Error::WrongType {
                expected: "object stream",
                found: obj.type_name(),
            });
        };
        let decoded = self.decode_stream(&stream)?;
        if !decoded.remaining.is_empty() {
            return Err(Error::Filter("object stream uses an unsupported filter"));
        }
        let n = stream.dict.get_int(b"N").unwrap_or(0).max(0) as usize;
        let first = stream.dict.get_int(b"First").unwrap_or(0).max(0) as usize;
        let header = decoded
            .data
            .get(..first.min(decoded.data.len()))
            .unwrap_or_default();
        let mut lexer = Lexer::new(header);
        let mut index = Vec::with_capacity(n.min(header.len() / 2 + 1));
        for _ in 0..n {
            match (lexer.next_token(), lexer.next_token()) {
                (Ok(Token::Int(obj)), Ok(Token::Int(off))) => {
                    if let (Ok(obj), Ok(off)) = (u32::try_from(obj), usize::try_from(off)) {
                        index.push((obj, off));
                    }
                }
                _ => break,
            }
        }
        let objstm = Arc::new(ObjStm {
            data: decoded.data,
            first,
            index,
        });
        if let Ok(mut m) = self.objstms.lock() {
            m.insert(num, Arc::clone(&objstm));
        }
        Ok(objstm)
    }

    // ---- Xref chain ------------------------------------------------------

    fn find_startxref(&self) -> Result<Option<u64>> {
        for tail in [1024u64, SCAN_CHUNK] {
            let start = self.len.saturating_sub(tail);
            let buf = self.read(start, tail)?;
            let Some(i) = buf.windows(9).rposition(|w| w == b"startxref") else {
                continue;
            };
            let mut lexer = Lexer::new(buf.get(i + 9..).unwrap_or_default());
            if let Ok(Token::Int(n)) = lexer.next_token() {
                return Ok(u64::try_from(n).ok());
            }
            return Ok(None);
        }
        Ok(None)
    }

    #[allow(clippy::type_complexity)]
    fn load_xref_chain(&self) -> Result<(XrefMap, Dict, Vec<u64>)> {
        let Some(start) = self.find_startxref()? else {
            self.repair(DiagnosticKind::BadStartxref, None)?;
            return Err(Error::NoXref);
        };
        let mut entries = XrefMap::new();
        let mut trailer = Dict::new();
        let mut sections = Vec::new();
        let mut seen = HashSet::new();
        let mut next = Some(start);
        while let Some(offset) = next.take() {
            if !seen.insert(offset) || sections.len() as u32 >= self.limits.max_xref_sections {
                self.repair(DiagnosticKind::XrefLoop, Some(offset))?;
                break;
            }
            let (section, section_trailer, at) = match self.load_section_near(offset) {
                Ok(s) => s,
                Err(e @ (Error::Source(_) | Error::Limit(LimitKind::Budget))) => return Err(e),
                Err(e) if sections.is_empty() => {
                    if self.strictness == Strictness::Lenient {
                        self.diagnostics
                            .push(DiagnosticKind::BadStartxref, Some(offset), None);
                    }
                    return Err(e);
                }
                Err(_) => {
                    // An older section is broken: keep the newer ones; the
                    // scan covers objects only the broken one knew.
                    self.repair(DiagnosticKind::BadXrefSection, Some(offset))?;
                    break;
                }
            };
            sections.push(at);
            for (num, entry) in section {
                entries.entry(num).or_insert(entry);
            }
            for (k, v) in section_trailer.iter() {
                if !trailer.contains_key(k.as_bytes()) {
                    trailer.insert(k.clone(), v.clone());
                }
            }
            next = section_trailer
                .get_int(b"Prev")
                .and_then(|p| u64::try_from(p).ok());
        }
        trailer.remove(b"Prev");
        trailer.remove(b"XRefStm");
        Ok((entries, trailer, sections))
    }

    /// Loads the section at `offset`, or at `offset` relative to the header
    /// for files with leading junk.
    fn load_section_near(&self, offset: u64) -> Result<(XrefMap, Dict, u64)> {
        match self.load_section(offset) {
            Err(e) if self.base > 0 => self
                .load_section(offset + self.base)
                .map(|(m, t)| (m, t, offset + self.base))
                .map_err(|_| e),
            other => other.map(|(m, t)| (m, t, offset)),
        }
    }

    fn load_section(&self, offset: u64) -> Result<(XrefMap, Dict)> {
        if offset >= self.len {
            return Err(Error::syntax(offset, "xref offset past end of file"));
        }
        let head = self.read(offset, 16)?;
        let skip = head.iter().position(|&b| !is_whitespace(b)).unwrap_or(0) as u64;
        if head
            .get(skip as usize..)
            .is_some_and(|h| h.starts_with(b"xref"))
        {
            let at = offset + skip;
            let (mut entries, trailer) = self.with_window(at, |buf| {
                xref::parse_table(buf, at, &self.limits, self.strictness, &self.diagnostics)
            })?;
            // Hybrid file: the xref stream adds objects the table hides.
            if let Some(stm) = trailer
                .get_int(b"XRefStm")
                .and_then(|o| u64::try_from(o).ok())
            {
                match self.load_xref_stream(stm) {
                    Ok((stream_entries, _)) => {
                        for (num, entry) in stream_entries {
                            let replace = matches!(entries.get(&num), None | Some(XrefEntry::Free));
                            if replace {
                                entries.insert(num, entry);
                            }
                        }
                    }
                    Err(e @ Error::Source(_)) => return Err(e),
                    Err(_) => self.repair(DiagnosticKind::BadXrefSection, Some(stm))?,
                }
            }
            return Ok((entries, trailer));
        }
        self.load_xref_stream(offset)
    }

    fn load_xref_stream(&self, offset: u64) -> Result<(XrefMap, Dict)> {
        let obj = self.parse_at(offset, None, MAX_LOAD_DEPTH)?;
        if !xref::is_xref_stream(&obj) {
            return Err(Error::syntax(offset, "expected an xref stream"));
        }
        let Object::Stream(stream) = obj else {
            return Err(Error::syntax(offset, "expected an xref stream"));
        };
        let decoded = filters::decode(
            &stream.raw,
            &filters::filter_chain(&stream.dict),
            &self.limits,
            self.strictness,
            Some(&self.diagnostics),
        )?;
        let entries = xref::parse_stream(&stream.dict, &decoded.data, &self.limits)?;
        let mut trailer = stream.dict;
        for key in [
            &b"Type"[..],
            b"W",
            b"Index",
            b"Filter",
            b"DecodeParms",
            b"Length",
        ] {
            trailer.remove(key);
        }
        Ok((entries, trailer))
    }

    // ---- Repair ----------------------------------------------------------

    /// Scans the whole file once for `num gen obj` and `trailer`.
    fn scan(&self) -> &Scan {
        self.scan
            .get_or_init(|| self.scan_file().unwrap_or_default())
    }

    fn scan_file(&self) -> Result<Scan> {
        let mut found: BTreeMap<u32, (u64, u16)> = BTreeMap::new();
        let mut trailers = Vec::new();
        let mut candidates: Vec<(u64, Flags)> = Vec::new();
        let overlap = 64u64;
        let mut pos = 0u64;
        while pos < self.len {
            self.limits.charge(SCAN_CHUNK)?;
            let chunk = self.read(pos, SCAN_CHUNK)?;
            let last = pos + chunk.len() as u64 >= self.len;
            // Only report hits that start before the overlap, so none is
            // seen twice; the next chunk sees the rest.
            let limit = if last {
                chunk.len()
            } else {
                chunk.len().saturating_sub(overlap as usize)
            };
            for i in 0..limit {
                let Some(window) = chunk.get(i..) else { break };
                if window.starts_with(b"obj") && !is_end_keyword(&chunk, i) {
                    let after_ok = window.get(3).is_none_or(|&b| !is_regular(b));
                    if after_ok
                        && let Some((start, num, generation)) = object_header_before(&chunk, i)
                    {
                        let offset = pos + start as u64;
                        if num <= self.limits.max_objects {
                            // Later definitions (incremental updates) win.
                            found.insert(num, (offset, generation));
                            let body = chunk.get(i..(i + 600).min(chunk.len())).unwrap_or_default();
                            candidates.push((offset, Flags::of(body)));
                        }
                    }
                } else if window.starts_with(b"trailer") {
                    trailers.push(pos + i as u64 + 7);
                }
            }
            if last || chunk.len() as u64 <= overlap {
                break;
            }
            pos += chunk.len() as u64 - overlap;
        }

        let mut scan = Scan::default();
        for (&num, &(offset, generation)) in &found {
            scan.entries
                .insert(num, XrefEntry::InUse { offset, generation });
        }

        // Trailer: the last `trailer` dictionary with a /Root, else an xref
        // stream's dictionary, else a synthesized one around a catalog.
        let mut trailer: Option<Dict> = None;
        for &at in trailers.iter().rev() {
            let parsed = self.with_window(at, |buf| {
                Parser::new(buf, at, &self.limits, Strictness::Lenient).parse_dict()
            });
            if let Ok(d) = parsed {
                match &mut trailer {
                    None => trailer = Some(d),
                    Some(t) => {
                        for (k, v) in d.iter() {
                            if !t.contains_key(k.as_bytes()) {
                                t.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
            }
        }
        // Object streams found by the scan add their objects.
        let live: HashSet<u64> = found.values().map(|&(o, _)| o).collect();
        for &(offset, flags) in candidates.iter().filter(|(o, _)| live.contains(o)) {
            if flags.objstm
                && let Ok(Object::Stream(s)) = self.parse_at(offset, None, MAX_LOAD_DEPTH)
                && let Ok(d) = filters::decode(
                    &s.raw,
                    &filters::filter_chain(&s.dict),
                    &self.limits,
                    Strictness::Lenient,
                    None,
                )
            {
                let n = s.dict.get_int(b"N").unwrap_or(0).max(0) as usize;
                let first = s.dict.get_int(b"First").unwrap_or(0).max(0) as usize;
                let mut lexer =
                    Lexer::new(d.data.get(..first.min(d.data.len())).unwrap_or_default());
                let stream_num = found
                    .iter()
                    .find(|(_, (o, _))| *o == offset)
                    .map(|(&n, _)| n);
                for index in 0..n {
                    let (Ok(Token::Int(obj)), Ok(Token::Int(_))) =
                        (lexer.next_token(), lexer.next_token())
                    else {
                        break;
                    };
                    if let (Ok(obj), Some(stream)) = (u32::try_from(obj), stream_num) {
                        scan.entries.entry(obj).or_insert(XrefEntry::Compressed {
                            stream,
                            index: index as u32,
                        });
                    }
                }
            }
            if flags.xref
                && trailer.as_ref().is_none_or(|t| !t.contains_key(b"Root"))
                && let Ok(Object::Stream(s)) = self.parse_at(offset, None, MAX_LOAD_DEPTH)
                && s.dict.contains_key(b"Root")
            {
                let mut d = s.dict;
                for key in [
                    &b"Type"[..],
                    b"W",
                    b"Index",
                    b"Filter",
                    b"DecodeParms",
                    b"Length",
                    b"Prev",
                ] {
                    d.remove(key);
                }
                trailer = Some(d);
            }
        }
        if trailer.as_ref().is_none_or(|t| !t.contains_key(b"Root")) {
            // Prefer the last catalog in the file (the newest revision).
            for &(offset, flags) in candidates.iter().rev().filter(|(o, _)| live.contains(o)) {
                if !flags.catalog {
                    continue;
                }
                let Ok(obj) = self.with_window(offset, |buf| {
                    Parser::new(buf, offset, &self.limits, Strictness::Lenient).parse_indirect(None)
                }) else {
                    continue;
                };
                if obj.1.as_dict().is_some_and(|d| d.is_type(b"Catalog")) {
                    let t = trailer.get_or_insert_with(Dict::new);
                    t.insert("Root", obj.0);
                    break;
                }
            }
        }
        scan.trailer = trailer;
        Ok(scan)
    }
}

/// What the start of a scanned object's body mentions.
#[derive(Debug, Clone, Copy, Default)]
struct Flags {
    objstm: bool,
    xref: bool,
    catalog: bool,
}

impl Flags {
    fn of(body: &[u8]) -> Self {
        let has = |n: &[u8]| body.windows(n.len()).any(|w| w == n);
        Self {
            objstm: has(b"/ObjStm"),
            xref: has(b"/XRef"),
            catalog: has(b"/Catalog"),
        }
    }
}

/// Whether the `obj` at `i` is the tail of `endobj`.
fn is_end_keyword(buf: &[u8], i: usize) -> bool {
    i >= 3 && buf.get(i - 3..i) == Some(b"end")
}

/// Given `obj` at `i`, finds `num gen` before it: returns where `num`
/// starts and the two numbers.
fn object_header_before(buf: &[u8], i: usize) -> Option<(usize, u32, u16)> {
    let mut j = i;
    while j > 0 && buf.get(j - 1).copied().is_some_and(is_whitespace) {
        j -= 1;
    }
    let gen_end = j;
    while j > 0 && buf.get(j - 1).is_some_and(u8::is_ascii_digit) {
        j -= 1;
    }
    if j == gen_end || gen_end - j > 5 {
        return None;
    }
    let gen_start = j;
    while j > 0 && buf.get(j - 1).copied().is_some_and(is_whitespace) {
        j -= 1;
    }
    if j == gen_start {
        return None;
    }
    let num_end = j;
    while j > 0 && buf.get(j - 1).is_some_and(u8::is_ascii_digit) {
        j -= 1;
    }
    if j == num_end || num_end - j > 10 {
        return None;
    }
    if j > 0 && buf.get(j - 1).copied().is_some_and(is_regular) {
        return None;
    }
    let num: u32 = std::str::from_utf8(buf.get(j..num_end)?)
        .ok()?
        .parse()
        .ok()?;
    let generation: u16 = std::str::from_utf8(buf.get(gen_start..gen_end)?)
        .ok()?
        .parse()
        .ok()?;
    Some((j, num, generation))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(bytes: &[u8]) -> Result<File> {
        File::open(Arc::new(bytes.to_vec()), OpenOptions::default())
    }

    fn open_strict(bytes: &[u8]) -> Result<File> {
        File::open(
            Arc::new(bytes.to_vec()),
            OpenOptions {
                strictness: Strictness::Strict,
                ..OpenOptions::default()
            },
        )
    }

    /// A small valid file with correct offsets.
    fn build(objects: &[(u32, &str)], trailer_extra: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (num, body) in objects {
            offsets.push((*num, out.len()));
            out.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let size = objects.iter().map(|(n, _)| n).max().copied().unwrap_or(0) + 1;
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for n in 1..size {
            match offsets.iter().find(|(m, _)| *m == n) {
                Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root 1 0 R {trailer_extra}>>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    const CATALOG: (u32, &str) = (1, "<< /Type /Catalog /Pages 2 0 R >>");
    const PAGES: (u32, &str) = (2, "<< /Type /Pages /Kids [] /Count 0 >>");

    #[test]
    fn opens_a_valid_file() {
        let f = open(&build(&[CATALOG, PAGES, (3, "(hello)")], "")).unwrap();
        assert_eq!(f.header().map(|h| h.version.minor), Some(7));
        assert!(f.diagnostics().is_empty(), "{:?}", f.diagnostics());
        assert_eq!(f.sections().len(), 1);
        let root = f.trailer().get_ref(b"Root").unwrap();
        assert!(f.get(root).unwrap().as_dict().unwrap().is_type(b"Catalog"));
        assert_eq!(
            f.get(ObjId::new(3, 0)).unwrap().as_string(),
            Some(&b"hello"[..])
        );
        assert_eq!(
            f.resolve(&Object::Ref(ObjId::new(3, 0))).as_string(),
            Some(&b"hello"[..])
        );
        assert_eq!(f.resolve(&Object::Ref(ObjId::new(99, 0))), Object::Null);
        assert!(matches!(
            f.get(ObjId::new(99, 0)),
            Err(Error::MissingObject(_))
        ));
    }

    #[test]
    fn stream_with_indirect_length() {
        let f = open(&build(
            &[
                CATALOG,
                PAGES,
                (3, "<< /Length 4 0 R >>\nstream\nabcdef\nendstream"),
                (4, "6"),
            ],
            "",
        ))
        .unwrap();
        let s = f.get(ObjId::new(3, 0)).unwrap();
        assert_eq!(&*s.as_stream().unwrap().raw, b"abcdef");
        assert!(f.diagnostics().is_empty());
    }

    #[test]
    fn wrong_stream_length_is_repaired() {
        let bytes = build(
            &[
                CATALOG,
                PAGES,
                (3, "<< /Length 2 >>\nstream\nabcdef\r\nendstream"),
            ],
            "",
        );
        let f = open(&bytes).unwrap();
        let s = f.get(ObjId::new(3, 0)).unwrap();
        assert_eq!(&*s.as_stream().unwrap().raw, b"abcdef");
        assert_eq!(f.diagnostics()[0].kind, DiagnosticKind::BadStreamLength);
        let strict = open_strict(&bytes).unwrap();
        assert!(strict.get(ObjId::new(3, 0)).is_err());
    }

    #[test]
    fn rebuilds_a_missing_xref() {
        let mut bytes = build(&[CATALOG, PAGES], "");
        let cut = bytes.windows(4).position(|w| w == b"xref").unwrap();
        bytes.truncate(cut);
        bytes.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
        let f = open(&bytes).unwrap();
        assert!(
            f.diagnostics()
                .iter()
                .any(|d| d.kind == DiagnosticKind::XrefRebuilt)
        );
        assert!(
            f.get(ObjId::new(2, 0))
                .unwrap()
                .as_dict()
                .unwrap()
                .is_type(b"Pages")
        );
        assert!(open_strict(&bytes).is_err());
    }

    #[test]
    fn finds_a_catalog_without_any_trailer() {
        let bytes = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n";
        let f = open(bytes).unwrap();
        assert_eq!(f.trailer().get_ref(b"Root"), Some(ObjId::new(1, 0)));
    }

    #[test]
    fn nothing_to_find() {
        assert!(open(b"%PDF-1.4\nnothing here\n").is_err());
        assert!(open(b"").is_err());
    }

    #[test]
    fn wrong_xref_offsets_fall_back_to_scan() {
        let bytes = build(&[CATALOG, PAGES, (3, "42")], "");
        let text = String::from_utf8(bytes).unwrap();
        // Shift every in-use offset by 3.
        let fixed: String = text
            .lines()
            .map(|l| {
                if l.len() == 19 && l.ends_with(" 00000 n ") {
                    let n: u64 = l[..10].parse().unwrap();
                    format!("{:010} 00000 n \n", n - 3)
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        let f = open(fixed.as_bytes()).unwrap();
        assert_eq!(f.get(ObjId::new(3, 0)).unwrap().as_int(), Some(42));
        assert!(
            f.diagnostics()
                .iter()
                .any(|d| d.kind == DiagnosticKind::BadXrefEntry)
        );
    }

    #[test]
    fn incremental_update_newest_wins() {
        let mut bytes = build(&[CATALOG, PAGES, (3, "(old)")], "");
        let first_xref = {
            let s = String::from_utf8_lossy(&bytes);
            let i = s.rfind("startxref\n").unwrap() + 10;
            s[i..].lines().next().unwrap().parse::<usize>().unwrap()
        };
        let obj_at = bytes.len();
        bytes.extend_from_slice(b"3 0 obj\n(new)\nendobj\n");
        let xref_at = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n3 1\n{obj_at:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Prev {first_xref} >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
        );
        let f = open(&bytes).unwrap();
        assert_eq!(f.sections().len(), 2);
        assert_eq!(
            f.get(ObjId::new(3, 0)).unwrap().as_string(),
            Some(&b"new"[..])
        );
        assert!(f.trailer().get(b"Prev").is_none());
    }

    #[test]
    fn prev_loop_is_cut() {
        let bytes = build(&[CATALOG, PAGES], "/Prev 0 ");
        let xref = String::from_utf8_lossy(&bytes);
        let at = xref.rfind("startxref\n").unwrap() + 10;
        let offset: usize = xref[at..].lines().next().unwrap().parse().unwrap();
        let looped = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("/Prev 0", &format!("/Prev {offset}"));
        let f = open(looped.as_bytes()).unwrap();
        assert!(
            f.diagnostics()
                .iter()
                .any(|d| d.kind == DiagnosticKind::XrefLoop)
        );
    }

    #[test]
    fn revision_accessors() {
        let bytes = build(&[CATALOG, PAGES], "");
        let f = open(&bytes).unwrap();
        assert_eq!(f.newest_section_is_stream().unwrap(), Some(false));
        assert_eq!(f.startxref().unwrap(), Some(f.sections()[0]));
        assert!(!f.was_rebuilt());
        assert_eq!(f.read_all().unwrap(), bytes);
        let scanned =
            open(b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n").unwrap();
        assert!(scanned.was_rebuilt());
        assert_eq!(scanned.newest_section_is_stream().unwrap(), None);
    }

    #[test]
    fn object_header_scanner() {
        assert_eq!(object_header_before(b"\n12 0 obj", 6), Some((1, 12, 0)));
        assert_eq!(object_header_before(b"12 3obj", 4), Some((0, 12, 3)));
        assert_eq!(object_header_before(b"x12 0 obj", 6), None);
        assert_eq!(object_header_before(b"0 obj", 2), None);
        assert_eq!(object_header_before(b"1 999999 obj", 9), None);
        assert!(is_end_keyword(b"endobj", 3));
    }
}
