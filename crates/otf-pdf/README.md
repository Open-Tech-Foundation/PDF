# otf-pdf

A from-scratch Rust toolkit to parse, edit, sign, verify, generate, validate
and render PDFs. This is the facade crate: the stable public API of the
`otf-pdf` workspace.

Status: Phase 1 (reading): open (including encrypted and damaged files),
enumerate pages, and read metadata, outlines and attachments. See
[docs/PLAN.md](https://github.com/Open-Tech-Foundation/PDF/blob/main/docs/PLAN.md)
for the roadmap.

```rust,no_run
use otf_pdf::{Document, OpenOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("file.pdf")?;
    let doc = Document::open(bytes, OpenOptions::default())?;
    println!("{} pages, title {:?}", doc.page_count()?, doc.info().title);
    Ok(())
}
```

Licensed under Apache-2.0.
