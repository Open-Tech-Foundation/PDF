//! PDF syntax layer: byte sources, the file header, the lexer and object
//! parser, the cross-reference table, stream filters, decryption and repair.
//!
//! Every input is untrusted (docs/PLAN.md §2): nothing here panics on any
//! bytes, every resource is bounded by [`Limits`], and all reads go through
//! the injected [`Source`] trait rather than touching the filesystem.
#![forbid(unsafe_code)]

pub mod crypt;
mod diag;
mod error;
mod file;
pub mod filters;
mod header;
pub mod lexer;
mod limits;
mod object;
pub mod parser;
pub mod serialize;
mod source;
pub mod xref;

pub use diag::{Diagnostic, DiagnosticKind, Diagnostics};
pub use error::{Error, Result};
pub use file::{File, OpenOptions, SharedSource};
pub use header::{HEADER_SEARCH_WINDOW, Header, HeaderError, Version, find_header};
pub use limits::{Budget, LimitKind, Limits, Strictness};
pub use object::{Dict, Name, ObjId, Object, PdfString, Stream};
pub use source::{Source, SourceError};
