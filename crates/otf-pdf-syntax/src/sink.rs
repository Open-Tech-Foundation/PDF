//! Sequential byte output.

use crate::source::SourceError;

/// Where a saved document is written. Like [`crate::Source`], it keeps the
/// core crates free of ambient I/O: the host decides whether bytes go to
/// memory, a file or a socket.
pub trait Sink {
    /// Appends `bytes`.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), SourceError>;
}

impl Sink for Vec<u8> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), SourceError> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_sink_appends() {
        let mut v = b"a".to_vec();
        v.write_all(b"bc").unwrap();
        assert_eq!(v, b"abc");
    }
}
