//! Document layer: the page tree, metadata, outlines and attachments on top
//! of the syntax layer's lazily parsed objects, a copy-on-write overlay for
//! edits, and full and incremental save.
#![forbid(unsafe_code)]

mod document;
mod edit;
mod pages;
mod save;
mod store;
mod text;
mod xmp;

pub use document::{Attachment, Document, Info, OutlineItem, name_tree};
pub use edit::{AttachmentOptions, import};
pub use pages::{Page, Rect};
pub use save::{Encryption, SaveMode, SaveOptions};
pub use store::Store;
pub use text::{decode_text_string, encode_text_string, pdf_doc_char};
