//! The syntax layer's error type.

use std::fmt;

use crate::limits::LimitKind;
use crate::object::ObjId;
use crate::source::SourceError;

/// Everything that can go wrong in the syntax layer.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The source failed to serve a read.
    Source(SourceError),
    /// Malformed syntax that could not be (or, in strict mode, was not
    /// allowed to be) repaired.
    Syntax {
        /// Byte offset in the file or in the buffer being parsed.
        offset: u64,
        /// What was expected or found.
        message: &'static str,
    },
    /// The input ended inside an object.
    UnexpectedEof,
    /// A resource limit was exceeded.
    Limit(LimitKind),
    /// No xref and no objects could be found.
    NoXref,
    /// The trailer has no usable `/Root`.
    NoRoot,
    /// An object is not in the xref, or cannot be found where it points.
    MissingObject(ObjId),
    /// An object had a different type than required.
    WrongType {
        /// What was required.
        expected: &'static str,
        /// What was found.
        found: &'static str,
    },
    /// A stream's filters failed.
    Filter(&'static str),
    /// The file uses an encryption scheme this library cannot read.
    UnsupportedEncryption(String),
    /// Neither the user nor the owner password matched.
    InvalidPassword,
}

impl Error {
    pub(crate) fn syntax(offset: u64, message: &'static str) -> Self {
        Self::Syntax { offset, message }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(e) => e.fmt(f),
            Self::Syntax { offset, message } => {
                write!(f, "syntax error at byte {offset}: {message}")
            }
            Self::UnexpectedEof => f.write_str("unexpected end of input"),
            Self::Limit(kind) => write!(f, "{kind} limit exceeded"),
            Self::NoXref => f.write_str("no cross-reference data or objects found"),
            Self::NoRoot => f.write_str("trailer has no document catalog (/Root)"),
            Self::MissingObject(id) => write!(f, "object {} {} not found", id.num, id.generation),
            Self::WrongType { expected, found } => {
                write!(f, "expected {expected}, found {found}")
            }
            Self::Filter(msg) => write!(f, "stream decoding failed: {msg}"),
            Self::UnsupportedEncryption(msg) => write!(f, "unsupported encryption: {msg}"),
            Self::InvalidPassword => f.write_str("incorrect password"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(e) => Some(e),
            _ => None,
        }
    }
}

impl From<SourceError> for Error {
    fn from(e: SourceError) -> Self {
        Self::Source(e)
    }
}

/// The syntax layer's result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(
            Error::syntax(5, "expected dictionary").to_string(),
            "syntax error at byte 5: expected dictionary"
        );
        assert_eq!(
            Error::Limit(LimitKind::Depth).to_string(),
            "nesting depth limit exceeded"
        );
        assert_eq!(
            Error::MissingObject(ObjId::new(3, 0)).to_string(),
            "object 3 0 not found"
        );
        assert!(std::error::Error::source(&Error::Source(SourceError::new("x"))).is_some());
        assert!(std::error::Error::source(&Error::NoRoot).is_none());
    }
}
