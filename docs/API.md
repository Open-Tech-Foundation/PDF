# otf-pdf API guide

Everything here is in the `otf-pdf` crate (`use otf_pdf::…`). Full item
documentation is on [docs.rs](https://docs.rs/otf-pdf); this guide is the
tour by task. Threading, limits and errors for hosts are in
[EMBEDDING.md](https://github.com/Open-Tech-Foundation/PDF/blob/main/docs/EMBEDDING.md).

## Open

```rust
use otf_pdf::{Document, OpenOptions, Strictness, Limits};

let doc = Document::open(bytes, OpenOptions {
    password: b"secret".to_vec(),      // tried as user, then owner password
    strictness: Strictness::Lenient,   // repair damaged files (default)
    limits: Limits::default(),         // untrusted-input bounds
})?;
```

| Call | Returns |
| --- | --- |
| `Document::open(source, options)` | Any `Source` (`Vec<u8>`, `&[u8]`, or your own random-access reader) |
| `Document::new()` | An empty document to build up (pages are appended from others) |
| `doc.version()` | `Option<Version>`: header or catalog `/Version`, whichever is later |
| `doc.diagnostics()` | `Vec<Diagnostic>`: every repair made, with kind, offset and object |
| `doc.is_encrypted()`, `doc.file().security()` | Encryption: revision, `permissions()`, `authentication()` |

`Error::InvalidPassword` means ask for a password;
`Error::UnsupportedEncryption` a certificate-encrypted file.

## Pages, metadata, outlines, attachments

| Call | Returns |
| --- | --- |
| `doc.pages()` / `doc.page_count()` | `Page { id, dict, media_box, crop_box, rotate, resources }`, inheritance resolved |
| `doc.info()` | `Info { title, author, subject, keywords, creator, producer, creation_date, mod_date, trapped }` |
| `doc.xmp_metadata()` | The XMP packet bytes |
| `doc.outlines()` | `Vec<OutlineItem { title, dest, action, open, children, extra }>` |
| `doc.named_destinations()` | `(name, destination)` pairs |
| `doc.attachments()` / `doc.attachment_data(&a)` | `Attachment { name, filename, description, mime_type, size, stream }`, and its bytes |

## Text

```rust
use otf_pdf::TextExtractor;

let ex = TextExtractor::new(&doc);          // fonts are cached across pages
let all: String = ex.text()?;               // pages separated by '\x0c'
let page = ex.page(0)?;                     // PageText
for line in &page.lines {                   // TextLine { text, bbox, chars }
    for word in line.words() { /* Word { text, bbox } */ }
}
```

Coordinates are in the displayed page (crop box origin, `/Rotate` applied,
y up, points). Reading order follows the content stream, like
`pdftotext -raw`. Annotation appearances (filled form fields) are included;
clipped and hidden text is not.

## Edit

All edits go into a copy-on-write overlay; the original bytes are untouched.

| Call | Does |
| --- | --- |
| `select_pages(&[usize])` | Keep these pages in this order (duplicates allowed) |
| `reorder_pages(&perm)`, `delete_pages(&[usize])` | Reorder; delete with outline and named-destination cleanup |
| `rotate_page(i, degrees)`, `set_crop_box(i, rect)`, `set_media_box(i, rect)` | Page geometry |
| `append_pages_from(&other, Some(&[..]))`, `extract_pages(&[..])` | Merge from / split into documents (deep copy, shared objects once) |
| `set_outlines(&[OutlineItem])` | Replace the bookmark tree |
| `set_info(&Info)` | Write Info and a matching XMP packet |
| `add_attachment(name, data, &AttachmentOptions)`, `remove_attachment(name)` | Embedded files |
| `sanitize(&SanitizeOptions)` → `SanitizeReport` | Remove scripts, risky actions, attachments, XFA, multimedia (and optionally links, metadata); save in full afterwards |
| `store_mut()` → `Store` | Low level: `get`, `set`, `add`, `delete`, `set_trailer_entry` |

## Save

```rust
use otf_pdf::{SaveOptions, SaveMode, Encryption, Permissions};

let bytes = doc.save(&SaveOptions::default())?;              // full, compact
let bytes = doc.save(&SaveOptions { mode: SaveMode::Incremental, ..Default::default() })?;
let bytes = doc.save(&SaveOptions {
    encryption: Some(Encryption {
        user_password: b"u".to_vec(), owner_password: b"o".to_vec(),
        permissions: Permissions { copy: false, ..Permissions::ALL },
        encrypt_metadata: true,
    }),
    entropy: Some(my_csprng),                                 // impl Entropy
    ..Default::default()
})?;
doc.save_to(&mut my_sink, &options)?;                        // impl Sink
```

| Option | Default | |
| --- | --- | --- |
| `mode` | `Full` | `Incremental` appends; refused on damaged xrefs and legacy encryption |
| `version` | 1.7 | or 2.0 |
| `object_streams` | `true` | `false` writes a classic xref table |
| `compress` | `true` | Flate for unfiltered streams |
| `encryption` | none | AES-256 only |
| `document_id` | content hash | for reproducible IDs of your own |
| `entropy` | none | required to encrypt |

## Lower levels

| Module | For |
| --- | --- |
| `otf_pdf::content` | `run_page(store, page, &FontCache, &mut device)` with your own `Device` (glyphs, paths, images, marked content); `parse_content` for operators |
| `File`, `Object`, `Dict`, `ObjId`, `XrefEntry`, `to_bytes` | Objects and the cross-reference table directly |
