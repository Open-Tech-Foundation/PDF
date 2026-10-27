//! A from-scratch toolkit to parse, edit, sign, verify, generate, validate and
//! render PDFs.
//!
//! This facade is the stable public API (docs/PLAN.md §8); the `otf-pdf-*`
//! crates behind it are internal and may change freely.
//!
//! Input is read through the [`Source`] trait, so the caller decides where
//! bytes come from; this crate does no I/O of its own.
//!
//! ```
//! let header = otf_pdf::find_header(&b"%PDF-1.7\n"[..])?;
//! assert_eq!(header.version, otf_pdf::Version::V1_7);
//! # Ok::<(), otf_pdf::HeaderError>(())
//! ```
#![forbid(unsafe_code)]

pub use otf_pdf_syntax::{
    HEADER_SEARCH_WINDOW, Header, HeaderError, Source, SourceError, Version, find_header,
};
