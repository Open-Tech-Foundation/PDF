<div align="center">

# PDF

***Read, edit, encrypt, sanitize and extract text from PDFs: pure Rust, built for untrusted input***

[crates.io](https://crates.io/crates/otf-pdf) | [Docs](https://docs.rs/otf-pdf) | [API guide](https://github.com/Open-Tech-Foundation/PDF/blob/main/docs/API.md) | [Embedding guide](https://github.com/Open-Tech-Foundation/PDF/blob/main/docs/EMBEDDING.md)

</div>

<div align="right">

*An [Open Tech Foundation](https://opentechf.org/) project*

</div>

> A PDF is opened lazily: objects are parsed on first use, damaged files are
> repaired (and every repair reported), and edits go into a copy-on-write
> overlay, so a save can rewrite the file cleanly or append just the changes.

```rust
use otf_pdf::{Document, OpenOptions, SaveOptions, TextExtractor};

let doc = Document::open(std::fs::read("in.pdf")?, OpenOptions::default())?;
println!("{} pages, title {:?}", doc.page_count()?, doc.info().title);
println!("{}", TextExtractor::new(&doc).page_text(0)?);

let mut doc = doc;
doc.delete_pages(&[0])?;
std::fs::write("out.pdf", doc.save(&SaveOptions::default())?)?;
```

> [!NOTE]
> **Pre-1.0.** The API can change between minor versions; see the
> [changelog](https://github.com/Open-Tech-Foundation/PDF/blob/main/CHANGELOG.md).

## What it does

| Area | |
| --- | --- |
| 📖 **Read** | PDF 1.0–2.0; classic and stream xrefs, object streams, incremental updates; page tree with inheritance; Info and XMP metadata, outlines, attachments |
| 🔐 **Decrypt** | Standard security handler R2–R6 (RC4, AES-128, AES-256), user and owner passwords; empty passwords open automatically |
| 🩹 **Repair** | Rebuilds broken xrefs, recovers trailers, catalogs and page trees, fixes wrong stream lengths; every repair is a reported diagnostic |
| ✏️ **Edit** | Merge, split, extract, reorder, delete, rotate and crop pages; outlines, named destinations, metadata (Info and XMP kept in sync), attachments |
| 💾 **Save** | Full save (garbage-collected, compressed, object streams) or incremental save (original bytes kept as a prefix); deterministic output |
| 🔒 **Encrypt** | AES-256 (R6) with permissions; legacy RC4/AES-128 are read, never written |
| 🧹 **Sanitize** | Remove JavaScript, launch/submit actions, attachments, XFA, multimedia and, optionally, links and metadata |
| 🔤 **Extract text** | Lines and words with positions; Type1, TrueType, Type3 and CID fonts, ToUnicode, CMaps, Adobe CJK collections; form field text |

Not yet: creating PDFs from scratch, rendering, forms filling, signatures,
PDF/A. See the [plan](https://github.com/Open-Tech-Foundation/PDF/blob/main/docs/PLAN.md).

## Why

| Need | How it is met |
| --- | --- |
| 🦀 **Pure Rust** | No C libraries; built from scratch on a small, pinned dependency set (RustCrypto, flate2, Fontations) |
| 🛡️ **Safe on hostile input** | `unsafe` is forbidden; malformed files are errors or repairs, never panics; every resource has a limit and a CPU budget hook; seven fuzz targets |
| 🧪 **Checked against references** | 3,842 real-world test files; files we write are verified by qpdf and poppler; text extraction scored against poppler and known source text |
| 🔌 **Embeddable** | No I/O, clock, randomness or global state of its own; input through a `Source` trait, output through a `Sink`, entropy injected; types are `Send + Sync` |

## Quality, measured

| Check | Result |
| --- | --- |
| Open with every page enumerated | 3,832 of 3,842 corpus files; page counts match poppler |
| Open → save → open, page content identical | Every file that opens |
| Files written, checked by qpdf and poppler | 695 files, 0 problems |
| Decryption, known plaintext (qpdf-encrypted, R2–R6) | 11 of 11 |
| Text: character error rate vs poppler (Latin, CJK) | 0.35%, 0.0% |
| Text: character error rate vs source (Hindi, Bengali, Tamil, Telugu, Chinese, Japanese, Korean) | 0.0% |

## Install

```toml
[dependencies]
otf-pdf = "0.1"
```

Rust 1.88 or later. The `otf-pdf-cli` crate in the repository is a
development tool (`otf-pdf info|pages|text|save|merge|sanitize …`), not
published.

## License

Apache-2.0. See
[NOTICE](https://github.com/Open-Tech-Foundation/PDF/blob/main/NOTICE) for the
font data from pdf.js and Adobe.
