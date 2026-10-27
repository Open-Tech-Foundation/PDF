//! Random-access byte input.

use std::fmt;

/// Random-access bytes a document is read from.
///
/// Core crates never open files or sockets themselves (docs/PLAN.md §2, "No
/// ambient I/O"); the caller hands them a `Source` backed by memory, a file,
/// or anything else that can serve ranges.
pub trait Source {
    /// Total length in bytes.
    fn len(&self) -> u64;

    /// Whether the source holds no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copies bytes starting at `offset` into `buf` and returns how many were
    /// copied. Fewer than `buf.len()` means the end of the source was reached;
    /// an `offset` at or past the end copies nothing.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SourceError>;
}

impl Source for [u8] {
    fn len(&self) -> u64 {
        // `usize` is at most 64 bits on every supported target.
        self.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SourceError> {
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        let Some(available) = self.get(start..) else {
            return Ok(0);
        };
        let n = available.len().min(buf.len());
        if let (Some(dst), Some(src)) = (buf.get_mut(..n), available.get(..n)) {
            dst.copy_from_slice(src);
        }
        Ok(n)
    }
}

impl Source for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SourceError> {
        self.as_slice().read_at(offset, buf)
    }
}

impl<S: Source + ?Sized> Source for &S {
    fn len(&self) -> u64 {
        (**self).len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SourceError> {
        (**self).read_at(offset, buf)
    }
}

/// A [`Source`] failed to serve a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    message: String,
}

impl SourceError {
    /// An error carrying the source's own description of what went wrong.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The source's description of what went wrong.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "source read failed: {}", self.message)
    }
}

impl std::error::Error for SourceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_range() {
        let bytes: &[u8] = b"hello world";
        let mut buf = [0u8; 5];
        assert_eq!(bytes.read_at(6, &mut buf), Ok(5));
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn short_read_at_end() {
        let bytes: &[u8] = b"abc";
        let mut buf = [0u8; 8];
        assert_eq!(bytes.read_at(1, &mut buf), Ok(2));
        assert_eq!(&buf[..2], b"bc");
    }

    #[test]
    fn offset_past_end_reads_nothing() {
        let bytes: &[u8] = b"abc";
        let mut buf = [0u8; 4];
        assert_eq!(bytes.read_at(3, &mut buf), Ok(0));
        assert_eq!(bytes.read_at(u64::MAX, &mut buf), Ok(0));
    }

    #[test]
    fn empty_buffer_reads_nothing() {
        let bytes: &[u8] = b"abc";
        assert_eq!(bytes.read_at(0, &mut []), Ok(0));
    }

    #[test]
    fn vec_and_reference_delegate() {
        let bytes = b"xyz".to_vec();
        let by_ref = &bytes;
        let mut buf = [0u8; 3];
        assert_eq!(Source::len(&by_ref), 3);
        assert!(!Source::is_empty(&by_ref));
        assert_eq!(by_ref.read_at(0, &mut buf), Ok(3));
        assert_eq!(&buf, b"xyz");
        assert!(Source::is_empty(&Vec::new()));
    }

    #[test]
    fn error_displays_message() {
        let err = SourceError::new("disk on fire");
        assert_eq!(err.message(), "disk on fire");
        assert_eq!(err.to_string(), "source read failed: disk on fire");
    }
}
