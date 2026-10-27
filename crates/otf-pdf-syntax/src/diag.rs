//! Diagnostics: a record of every repair made while reading.

use std::fmt;
use std::sync::Mutex;

use crate::object::ObjId;

/// What was repaired or tolerated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// Bytes precede the `%PDF-` header.
    JunkBeforeHeader,
    /// No usable header; the file was read anyway.
    MissingHeader,
    /// `startxref` was missing or pointed at no xref.
    BadStartxref,
    /// An xref section could not be parsed and was skipped.
    BadXrefSection,
    /// An xref entry pointed at the wrong place; the object was found by scan.
    BadXrefEntry,
    /// The xref was rebuilt by scanning the file for objects.
    XrefRebuilt,
    /// The trailer was missing and was recovered from elsewhere.
    TrailerRecovered,
    /// A `/Prev` chain revisited a section or was cut at the limit.
    XrefLoop,
    /// A stream's `/Length` was wrong; its end was found by `endstream`.
    BadStreamLength,
    /// An object had no `endobj`.
    MissingEndobj,
    /// The object at an xref offset had a different number or generation.
    ObjectIdMismatch,
    /// A filter failed part-way; the bytes decoded before it are kept.
    TruncatedStream,
    /// An unknown or unsupported filter; the stream is left encoded.
    UnknownFilter(Vec<u8>),
    /// A syntax error inside an object was skipped over.
    SyntaxError,
    /// A broken or cyclic page tree node was skipped.
    BadPageTree,
    /// The page tree was unusable and pages were found by scanning objects.
    PagesRecovered,
    /// Something else, described in the message.
    Other(String),
}

impl fmt::Display for DiagnosticKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::JunkBeforeHeader => f.write_str("junk before header"),
            Self::MissingHeader => f.write_str("missing header"),
            Self::BadStartxref => f.write_str("bad startxref"),
            Self::BadXrefSection => f.write_str("bad xref section"),
            Self::BadXrefEntry => f.write_str("bad xref entry"),
            Self::XrefRebuilt => f.write_str("xref rebuilt by scanning"),
            Self::TrailerRecovered => f.write_str("trailer recovered"),
            Self::XrefLoop => f.write_str("xref /Prev loop"),
            Self::BadStreamLength => f.write_str("wrong stream /Length"),
            Self::MissingEndobj => f.write_str("missing endobj"),
            Self::ObjectIdMismatch => f.write_str("object id mismatch"),
            Self::TruncatedStream => f.write_str("truncated stream"),
            Self::UnknownFilter(name) => {
                write!(f, "unsupported filter /{}", String::from_utf8_lossy(name))
            }
            Self::SyntaxError => f.write_str("syntax error"),
            Self::BadPageTree => f.write_str("bad page tree node"),
            Self::PagesRecovered => f.write_str("pages recovered by scanning"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

/// One repair, with where it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// What was repaired.
    pub kind: DiagnosticKind,
    /// Byte offset in the file, when known.
    pub offset: Option<u64>,
    /// The object concerned, when known.
    pub object: Option<ObjId>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(id) = self.object {
            write!(f, " (object {} {})", id.num, id.generation)?;
        }
        if let Some(offset) = self.offset {
            write!(f, " at byte {offset}")?;
        }
        Ok(())
    }
}

/// A shared, append-only list of diagnostics, capped so a hostile file cannot
/// grow it without bound.
#[derive(Debug, Default)]
pub struct Diagnostics {
    list: Mutex<Vec<Diagnostic>>,
}

/// More diagnostics than this are counted as dropped, not stored.
const MAX_DIAGNOSTICS: usize = 10_000;

impl Diagnostics {
    /// Records a diagnostic.
    pub fn push(&self, kind: DiagnosticKind, offset: Option<u64>, object: Option<ObjId>) {
        if let Ok(mut list) = self.list.lock()
            && list.len() < MAX_DIAGNOSTICS
        {
            list.push(Diagnostic {
                kind,
                offset,
                object,
            });
        }
    }

    /// A copy of everything recorded so far.
    pub fn snapshot(&self) -> Vec<Diagnostic> {
        self.list.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// How many are recorded.
    pub fn len(&self) -> usize {
        self.list.lock().map(|l| l.len()).unwrap_or(0)
    }

    /// Whether none are recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_displays() {
        let d = Diagnostics::default();
        assert!(d.is_empty());
        d.push(
            DiagnosticKind::BadStreamLength,
            Some(10),
            Some(ObjId::new(4, 0)),
        );
        d.push(DiagnosticKind::UnknownFilter(b"Foo".to_vec()), None, None);
        let all = d.snapshot();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all[0].to_string(),
            "wrong stream /Length (object 4 0) at byte 10"
        );
        assert_eq!(all[1].to_string(), "unsupported filter /Foo");
    }

    #[test]
    fn capped() {
        let d = Diagnostics::default();
        for _ in 0..MAX_DIAGNOSTICS + 5 {
            d.push(DiagnosticKind::SyntaxError, None, None);
        }
        assert_eq!(d.len(), MAX_DIAGNOSTICS);
    }
}
