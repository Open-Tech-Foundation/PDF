//! The PDF object model (ISO 32000-2 §7.3).
//!
//! Names and strings are bytes, never `String`: PDF does not promise any
//! encoding for either, and a lossy conversion would corrupt round-trips.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// An indirect object's identity: object number and generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjId {
    /// Object number. Zero is never a real object.
    pub num: u32,
    /// Generation number.
    pub generation: u16,
}

impl ObjId {
    /// An id from its two parts.
    pub const fn new(num: u32, generation: u16) -> Self {
        Self { num, generation }
    }
}

impl fmt::Display for ObjId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.num, self.generation)
    }
}

/// A name object, without its leading `/` and with `#xx` escapes resolved.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Name(pub Vec<u8>);

impl Name {
    /// A name from its bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// The name's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Borrow<[u8]> for Name {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl From<&[u8]> for Name {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl<const N: usize> From<&[u8; N]> for Name {
    fn from(bytes: &[u8; N]) -> Self {
        Self(bytes.to_vec())
    }
}

impl From<&str> for Name {
    fn from(s: &str) -> Self {
        Self(s.as_bytes().to_vec())
    }
}

impl PartialEq<[u8]> for Name {
    fn eq(&self, other: &[u8]) -> bool {
        self.0 == other
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for Name {
    fn eq(&self, other: &&[u8; N]) -> bool {
        self.0 == other.as_slice()
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", String::from_utf8_lossy(&self.0))
    }
}

/// A string object's bytes, and whether it was written in hex.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct PdfString {
    /// The string's bytes, with escapes resolved.
    pub bytes: Vec<u8>,
    /// Whether it was written as `<hex>` rather than `(literal)`.
    pub hex: bool,
}

impl PdfString {
    /// A literal string.
    pub fn literal(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
            hex: false,
        }
    }

    /// A hex string.
    pub fn hex(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
            hex: true,
        }
    }
}

impl fmt::Debug for PdfString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.hex {
            write!(f, "<")?;
            for b in &self.bytes {
                write!(f, "{b:02x}")?;
            }
            write!(f, ">")
        } else {
            write!(f, "({})", String::from_utf8_lossy(&self.bytes))
        }
    }
}

/// A dictionary. Keys are kept sorted, so iteration (and anything written
/// from it) is deterministic. A repeated key keeps its last value.
#[derive(Clone, PartialEq, Default)]
pub struct Dict(BTreeMap<Name, Object>);

impl Dict {
    /// An empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value for `key`, if present. A `null` value counts as absent
    /// (ISO 32000-2 §7.3.7).
    pub fn get(&self, key: &[u8]) -> Option<&Object> {
        self.0.get(key).filter(|v| !matches!(v, Object::Null))
    }

    /// Sets `key` to `value`, returning the previous value.
    pub fn insert(&mut self, key: impl Into<Name>, value: impl Into<Object>) -> Option<Object> {
        self.0.insert(key.into(), value.into())
    }

    /// Removes `key`, returning its value.
    pub fn remove(&mut self, key: &[u8]) -> Option<Object> {
        self.0.remove(key)
    }

    /// Whether `key` is present with a non-null value.
    pub fn contains_key(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&Name, &Object)> {
        self.0.iter()
    }

    /// The value for `key` if it is a name.
    pub fn get_name(&self, key: &[u8]) -> Option<&[u8]> {
        self.get(key).and_then(Object::as_name)
    }

    /// The value for `key` if it is an integer.
    pub fn get_int(&self, key: &[u8]) -> Option<i64> {
        self.get(key).and_then(Object::as_int)
    }

    /// The value for `key` if it is a dictionary (or a stream's dictionary).
    pub fn get_dict(&self, key: &[u8]) -> Option<&Dict> {
        self.get(key).and_then(Object::as_dict)
    }

    /// The value for `key` if it is an array.
    pub fn get_array(&self, key: &[u8]) -> Option<&[Object]> {
        self.get(key).and_then(Object::as_array)
    }

    /// The value for `key` if it is an indirect reference.
    pub fn get_ref(&self, key: &[u8]) -> Option<ObjId> {
        self.get(key).and_then(Object::as_ref)
    }

    /// Whether `/Type` is `ty`.
    pub fn is_type(&self, ty: &[u8]) -> bool {
        self.get_name(b"Type") == Some(ty)
    }
}

impl fmt::Debug for Dict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

impl FromIterator<(Name, Object)> for Dict {
    fn from_iter<I: IntoIterator<Item = (Name, Object)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// A stream: its dictionary and its bytes exactly as stored in the file,
/// still encoded and (if the file is encrypted) still encrypted.
#[derive(Clone, PartialEq)]
pub struct Stream {
    /// The stream dictionary.
    pub dict: Dict,
    /// The raw bytes between `stream` and `endstream`.
    pub raw: Arc<[u8]>,
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} stream[{} bytes]", self.dict, self.raw.len())
    }
}

/// Any PDF object.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Object {
    /// `null`.
    #[default]
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A real number.
    Real(f64),
    /// A string.
    String(PdfString),
    /// A name.
    Name(Name),
    /// An array.
    Array(Vec<Object>),
    /// A dictionary.
    Dict(Dict),
    /// A stream.
    Stream(Stream),
    /// An indirect reference.
    Ref(ObjId),
}

impl Object {
    /// The kind of object, for messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Int(_) => "integer",
            Self::Real(_) => "real",
            Self::String(_) => "string",
            Self::Name(_) => "name",
            Self::Array(_) => "array",
            Self::Dict(_) => "dictionary",
            Self::Stream(_) => "stream",
            Self::Ref(_) => "reference",
        }
    }

    /// The value if this is a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The value if this is an integer, or a real with no fractional part.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Real(r) if r.fract() == 0.0 && r.abs() < 9.0e15 => Some(*r as i64),
            _ => None,
        }
    }

    /// The value as a number, if it is an integer or a real.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(*i as f64),
            Self::Real(r) => Some(*r),
            _ => None,
        }
    }

    /// The bytes if this is a name.
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Self::Name(n) => Some(n.as_bytes()),
            _ => None,
        }
    }

    /// The bytes if this is a string.
    pub fn as_string(&self) -> Option<&[u8]> {
        match self {
            Self::String(s) => Some(&s.bytes),
            _ => None,
        }
    }

    /// The elements if this is an array.
    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The dictionary if this is a dictionary or a stream.
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Self::Dict(d) => Some(d),
            Self::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    /// The stream if this is one.
    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Self::Stream(s) => Some(s),
            _ => None,
        }
    }

    /// The target if this is an indirect reference.
    pub fn as_ref(&self) -> Option<ObjId> {
        match self {
            Self::Ref(id) => Some(*id),
            _ => None,
        }
    }
}

impl From<bool> for Object {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i64> for Object {
    fn from(i: i64) -> Self {
        Self::Int(i)
    }
}

impl From<f64> for Object {
    fn from(r: f64) -> Self {
        Self::Real(r)
    }
}

impl From<Name> for Object {
    fn from(n: Name) -> Self {
        Self::Name(n)
    }
}

impl From<PdfString> for Object {
    fn from(s: PdfString) -> Self {
        Self::String(s)
    }
}

impl From<Dict> for Object {
    fn from(d: Dict) -> Self {
        Self::Dict(d)
    }
}

impl From<Vec<Object>> for Object {
    fn from(a: Vec<Object>) -> Self {
        Self::Array(a)
    }
}

impl From<ObjId> for Object {
    fn from(id: ObjId) -> Self {
        Self::Ref(id)
    }
}

impl From<Stream> for Object {
    fn from(s: Stream) -> Self {
        Self::Stream(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_null_is_absent() {
        let mut d = Dict::new();
        d.insert("A", Object::Null);
        d.insert("B", 3i64);
        assert!(d.get(b"A").is_none());
        assert!(!d.contains_key(b"A"));
        assert_eq!(d.get_int(b"B"), Some(3));
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn dict_last_key_wins_and_is_sorted() {
        let d: Dict = [
            (Name::from("Z"), Object::Int(1)),
            (Name::from("A"), Object::Int(2)),
            (Name::from("Z"), Object::Int(3)),
        ]
        .into_iter()
        .collect();
        let keys: Vec<_> = d.iter().map(|(k, _)| k.as_bytes().to_vec()).collect();
        assert_eq!(keys, vec![b"A".to_vec(), b"Z".to_vec()]);
        assert_eq!(d.get_int(b"Z"), Some(3));
    }

    #[test]
    fn typed_accessors() {
        let mut d = Dict::new();
        d.insert("Type", Name::from("Page"));
        d.insert("Kids", vec![Object::Ref(ObjId::new(4, 0))]);
        d.insert("Parent", ObjId::new(2, 0));
        d.insert("Res", Dict::new());
        assert!(d.is_type(b"Page"));
        assert_eq!(d.get_array(b"Kids").map(<[_]>::len), Some(1));
        assert_eq!(d.get_ref(b"Parent"), Some(ObjId::new(2, 0)));
        assert!(d.get_dict(b"Res").is_some());
        assert_eq!(d.remove(b"Res"), Some(Object::Dict(Dict::new())));
    }

    #[test]
    fn numbers() {
        assert_eq!(Object::Real(3.0).as_int(), Some(3));
        assert_eq!(Object::Real(3.5).as_int(), None);
        assert_eq!(Object::Real(1e300).as_int(), None);
        assert_eq!(Object::Int(2).as_f64(), Some(2.0));
        assert_eq!(Object::Null.as_f64(), None);
    }

    #[test]
    fn debug_forms() {
        assert_eq!(format!("{:?}", Name::from("A B")), "/A B");
        assert_eq!(format!("{:?}", PdfString::hex(vec![0xab, 1])), "<ab01>");
        assert_eq!(format!("{:?}", PdfString::literal("hi")), "(hi)");
        assert_eq!(ObjId::new(3, 1).to_string(), "3 1 R");
        assert_eq!(Object::Bool(true).type_name(), "boolean");
    }
}
