# otf-pdf

A from-scratch Rust toolkit to parse, edit, sign, verify, generate, validate
and render PDFs. This is the facade crate: the stable public API of the
`otf-pdf` workspace.

Status: Phase 0 (foundations). Only file-header detection is available; see
[docs/PLAN.md](https://github.com/Open-Tech-Foundation/PDF/blob/main/docs/PLAN.md)
for the roadmap.

```rust
let header = otf_pdf::find_header(&b"%PDF-1.7\n"[..])?;
assert_eq!(header.version, otf_pdf::Version::V1_7);
# Ok::<(), otf_pdf::HeaderError>(())
```

Licensed under Apache-2.0.
