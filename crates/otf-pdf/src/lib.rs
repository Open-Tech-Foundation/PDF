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
//! # let bytes = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
//! #   2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
//! #   3 0 obj << /Type /Page /Parent 2 0 R >> endobj\n".to_vec();
//! use otf_pdf::{Document, OpenOptions};
//!
//! let doc = Document::open(bytes, OpenOptions::default())?;
//! assert_eq!(doc.page_count()?, 1);
//! // That file has no xref; the repair is recorded, not hidden.
//! assert!(!doc.diagnostics().is_empty());
//! # Ok::<(), otf_pdf::Error>(())
//! ```
#![forbid(unsafe_code)]

pub use otf_pdf_doc::{Attachment, Document, Info, OutlineItem, Page, Rect, decode_text_string};
pub use otf_pdf_syntax::crypt::{Authentication, CryptMethod, SecurityHandler};
pub use otf_pdf_syntax::serialize::{to_bytes, write_object};
pub use otf_pdf_syntax::xref::XrefEntry;
pub use otf_pdf_syntax::{
    Budget, Diagnostic, DiagnosticKind, Dict, Error, File, HEADER_SEARCH_WINDOW, Header,
    HeaderError, LimitKind, Limits, Name, ObjId, Object, OpenOptions, PdfString, Result, Source,
    SourceError, Stream, Strictness, Version, find_header,
};
