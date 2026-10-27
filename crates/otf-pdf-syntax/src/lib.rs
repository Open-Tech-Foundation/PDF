//! PDF syntax layer: byte sources and the file header.
//!
//! Every input is untrusted (docs/PLAN.md §2): nothing here panics on any
//! bytes, and all reads go through the injected [`Source`] trait rather than
//! touching the filesystem.
#![forbid(unsafe_code)]

mod header;
mod source;

pub use header::{HEADER_SEARCH_WINDOW, Header, HeaderError, Version, find_header};
pub use source::{Source, SourceError};
