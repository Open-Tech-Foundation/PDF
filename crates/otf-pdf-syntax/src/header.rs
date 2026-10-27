//! The `%PDF-M.m` file header.

use std::fmt;

use crate::source::{Source, SourceError};

/// How far into a file the header is looked for.
///
/// ISO 32000 puts the header at byte 0, but real files carry junk before it
/// (mail headers, BOMs, printer job language). Acrobat accepts a header that
/// starts anywhere in the first 1024 bytes; so do we.
pub const HEADER_SEARCH_WINDOW: u64 = 1024;

const MARKER: &[u8] = b"%PDF-";
/// Digits per version component; more than this is not a version.
const MAX_DIGITS: usize = 3;
/// Bytes read past the window so a header starting near its end is whole.
const TAIL: usize = MARKER.len() + 2 * MAX_DIGITS + 1;

/// A PDF version as written in the header, e.g. `1.7`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// The part before the dot.
    pub major: u8,
    /// The part after the dot.
    pub minor: u8,
}

impl Version {
    /// PDF 1.7 (ISO 32000-1), the default output version.
    pub const V1_7: Version = Version::new(1, 7);
    /// PDF 2.0 (ISO 32000-2).
    pub const V2_0: Version = Version::new(2, 0);

    /// A version from its two components.
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    /// Whether this is a published PDF version: 1.0 to 1.7, or 2.0.
    pub fn is_known(self) -> bool {
        matches!((self.major, self.minor), (1, 0..=7) | (2, 0))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A located file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Byte offset of the `%` that starts the header. Non-zero means there is
    /// junk before it; byte offsets inside the file (the xref, `startxref`)
    /// are usually relative to this point in such files.
    pub offset: u64,
    /// The version the header declares. It may be unknown (see
    /// [`Version::is_known`]); the catalog's `/Version` can also override it.
    pub version: Version,
}

impl Header {
    /// Whether bytes precede the header.
    pub fn has_leading_junk(&self) -> bool {
        self.offset != 0
    }
}

/// Why no header could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    /// No `%PDF-` within the first [`HEADER_SEARCH_WINDOW`] bytes.
    NotFound,
    /// `%PDF-` was found at `offset` but no `major.minor` version follows it.
    MalformedVersion {
        /// Byte offset of the `%PDF-` marker.
        offset: u64,
    },
    /// The source failed to serve the read.
    Source(SourceError),
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(
                f,
                "no %PDF- header in the first {HEADER_SEARCH_WINDOW} bytes"
            ),
            Self::MalformedVersion { offset } => {
                write!(f, "malformed version after %PDF- at byte {offset}")
            }
            Self::Source(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for HeaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(err) => Some(err),
            _ => None,
        }
    }
}

impl From<SourceError> for HeaderError {
    fn from(err: SourceError) -> Self {
        Self::Source(err)
    }
}

/// Finds the `%PDF-M.m` header in the first [`HEADER_SEARCH_WINDOW`] bytes.
///
/// The first `%PDF-` marker wins; if no version follows it, that is an error
/// rather than a reason to keep looking, since a second header in the junk is
/// no more trustworthy than the first.
pub fn find_header<S: Source + ?Sized>(source: &S) -> Result<Header, HeaderError> {
    // The window and its tail are a little over 1 KiB, so this always fits.
    let mut buf = [0u8; HEADER_SEARCH_WINDOW as usize + TAIL];
    let mut filled = 0;
    while filled < buf.len() {
        let Some(rest) = buf.get_mut(filled..) else {
            break;
        };
        let n = source.read_at(filled as u64, rest)?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    let bytes = buf.get(..filled).unwrap_or_default();

    let start = bytes
        .windows(MARKER.len())
        .take(HEADER_SEARCH_WINDOW as usize)
        .position(|w| w == MARKER)
        .ok_or(HeaderError::NotFound)?;
    let offset = start as u64;
    let after = bytes
        .get(start + MARKER.len()..)
        .ok_or(HeaderError::MalformedVersion { offset })?;
    let version = parse_version(after).ok_or(HeaderError::MalformedVersion { offset })?;
    Ok(Header { offset, version })
}

/// Parses `major.minor` from the start of `bytes`.
fn parse_version(bytes: &[u8]) -> Option<Version> {
    let (major, rest) = parse_component(bytes)?;
    let rest = rest.strip_prefix(b".")?;
    let (minor, _) = parse_component(rest)?;
    Some(Version::new(major, minor))
}

/// Parses one to [`MAX_DIGITS`] ASCII digits that fit in a `u8`.
fn parse_component(bytes: &[u8]) -> Option<(u8, &[u8])> {
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > MAX_DIGITS {
        return None;
    }
    let (number, rest) = bytes.split_at_checked(digits)?;
    let value = number
        .iter()
        .try_fold(0u16, |acc, &d| Some(acc * 10 + u16::from(d - b'0')))?;
    Some((u8::try_from(value).ok()?, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(bytes: &[u8]) -> Result<Header, HeaderError> {
        find_header(bytes)
    }

    #[test]
    fn header_at_start() {
        assert_eq!(
            header(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"),
            Ok(Header {
                offset: 0,
                version: Version::V1_7
            })
        );
    }

    #[test]
    fn pdf_2_0() {
        let h = header(b"%PDF-2.0\r\n").unwrap();
        assert_eq!(h.version, Version::V2_0);
        assert!(h.version.is_known());
        assert!(!h.has_leading_junk());
    }

    #[test]
    fn junk_before_header() {
        let mut bytes = vec![b'x'; 500];
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let h = header(&bytes).unwrap();
        assert_eq!(h.offset, 500);
        assert!(h.has_leading_junk());
        assert_eq!(h.version, Version::new(1, 4));
    }

    #[test]
    fn header_starting_at_last_window_byte() {
        let mut bytes = vec![0u8; HEADER_SEARCH_WINDOW as usize - 1];
        bytes.extend_from_slice(b"%PDF-1.3");
        assert_eq!(header(&bytes).unwrap().offset, HEADER_SEARCH_WINDOW - 1);
    }

    #[test]
    fn header_past_window_is_not_found() {
        let mut bytes = vec![0u8; HEADER_SEARCH_WINDOW as usize];
        bytes.extend_from_slice(b"%PDF-1.3");
        assert_eq!(header(&bytes), Err(HeaderError::NotFound));
    }

    #[test]
    fn empty_and_unrelated_input() {
        assert_eq!(header(b""), Err(HeaderError::NotFound));
        assert_eq!(header(b"%!PS-Adobe-3.0"), Err(HeaderError::NotFound));
        assert_eq!(header(b"%PDF"), Err(HeaderError::NotFound));
    }

    #[test]
    fn malformed_versions() {
        for bad in [
            &b"%PDF-"[..],
            b"%PDF-x.y",
            b"%PDF-1",
            b"%PDF-1.",
            b"%PDF-.7",
            b"%PDF-1234.0",
            b"%PDF-256.0",
            b"%PDF-1.9999",
        ] {
            assert_eq!(
                header(bad),
                Err(HeaderError::MalformedVersion { offset: 0 }),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn first_marker_wins_even_if_malformed() {
        assert_eq!(
            header(b"junk%PDF-?\n%PDF-1.7"),
            Err(HeaderError::MalformedVersion { offset: 4 })
        );
    }

    #[test]
    fn unknown_but_well_formed_version() {
        let h = header(b"%PDF-1.8").unwrap();
        assert_eq!(h.version, Version::new(1, 8));
        assert!(!h.version.is_known());
        assert!(!Version::new(2, 1).is_known());
        assert!(!Version::new(0, 9).is_known());
        assert!(Version::new(1, 0).is_known());
    }

    #[test]
    fn three_digit_components() {
        assert_eq!(
            header(b"%PDF-001.255").unwrap().version,
            Version::new(1, 255)
        );
    }

    #[test]
    fn version_display() {
        assert_eq!(Version::V1_7.to_string(), "1.7");
        assert_eq!(Version::new(10, 12).to_string(), "10.12");
    }

    #[test]
    fn error_display() {
        assert_eq!(
            HeaderError::NotFound.to_string(),
            "no %PDF- header in the first 1024 bytes"
        );
        assert_eq!(
            HeaderError::MalformedVersion { offset: 3 }.to_string(),
            "malformed version after %PDF- at byte 3"
        );
    }

    /// A source that serves one byte per read, to exercise short reads.
    struct Trickle<'a>(&'a [u8]);

    impl Source for Trickle<'_> {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SourceError> {
            let end = buf.len().min(1);
            self.0.read_at(offset, &mut buf[..end])
        }
    }

    #[test]
    fn short_reads_are_retried() {
        let mut bytes = vec![b' '; 10];
        bytes.extend_from_slice(b"%PDF-1.5");
        assert_eq!(find_header(&Trickle(&bytes)).unwrap().offset, 10);
    }

    struct Failing;

    impl Source for Failing {
        fn len(&self) -> u64 {
            100
        }

        fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize, SourceError> {
            Err(SourceError::new("boom"))
        }
    }

    #[test]
    fn source_errors_propagate() {
        let err = find_header(&Failing).unwrap_err();
        assert_eq!(err, HeaderError::Source(SourceError::new("boom")));
        assert!(std::error::Error::source(&err).is_some());
        assert_eq!(err.to_string(), "source read failed: boom");
    }
}
