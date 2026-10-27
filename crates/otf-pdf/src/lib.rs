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

pub use otf_pdf_doc::{
    Attachment, AttachmentOptions, Document, Encryption, Info, OutlineItem, Page, Rect,
    SanitizeOptions, SanitizeReport, SaveMode, SaveOptions, Store, decode_text_string,
    encode_text_string,
};
pub use otf_pdf_syntax::crypt::{Authentication, CryptMethod, SecurityHandler};
pub use otf_pdf_syntax::serialize::{to_bytes, write_object};
pub use otf_pdf_syntax::xref::XrefEntry;
pub use otf_pdf_syntax::{
    Budget, Diagnostic, DiagnosticKind, Dict, Entropy, Error, File, HEADER_SEARCH_WINDOW, Header,
    HeaderError, LimitKind, Limits, Name, ObjId, Object, OpenOptions, PdfString, Permissions,
    Result, Sink, Source, SourceError, Stream, Strictness, Version, find_header,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Embedders move documents and errors across threads (a host's
    /// blocking pool, docs/EMBEDDING.md); this fails to compile if any
    /// public type stops being shareable.
    #[test]
    fn public_types_are_send_and_sync() {
        const fn shareable<T: Send + Sync>() {}
        shareable::<Document>();
        shareable::<File>();
        shareable::<OpenOptions>();
        shareable::<Limits>();
        shareable::<Error>();
        shareable::<Page>();
        shareable::<Object>();
        shareable::<Diagnostic>();
        shareable::<SecurityHandler>();
        shareable::<SaveOptions>();
        shareable::<Store>();
    }

    #[test]
    fn one_document_is_read_from_many_threads() {
        let bytes = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
            2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n\
            3 0 obj << /Type /Page /Parent 2 0 R >> endobj\n\
            4 0 obj << /Type /Page /Parent 2 0 R >> endobj\n"
            .to_vec();
        let doc = std::sync::Arc::new(Document::open(bytes, OpenOptions::default()).unwrap());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let doc = std::sync::Arc::clone(&doc);
                std::thread::spawn(move || doc.page_count().unwrap())
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), 2);
        }
    }
}
