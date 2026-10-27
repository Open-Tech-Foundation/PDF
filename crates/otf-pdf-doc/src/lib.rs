//! Document layer: the page tree, metadata, outlines and attachments on top
//! of the syntax layer's lazily parsed objects. Editing and saving (the
//! copy-on-write overlay) arrive in Phase 2.
#![forbid(unsafe_code)]

mod document;
mod pages;
mod text;

pub use document::{Attachment, Document, Info, OutlineItem, name_tree};
pub use pages::{Page, Rect};
pub use text::{decode_text_string, pdf_doc_char};
