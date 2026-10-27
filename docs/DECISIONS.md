# otf-pdf — Decisions

ADR-style log. New decisions are appended; superseded ones are marked, not deleted.

## D1: Standalone project, Apache-2.0
`otf-pdf` is its own repository under Open-Tech-Foundation, licensed Apache-2.0. es-runtime consumes it as `runtime:pdf`. The JSX→PDF layout project consumes it as its PDF backend.

## D2: From scratch, with a minimal dependency allowlist
No existing PDF crates. External crates are allowed only where reimplementing would be a project in its own right or a security risk:
- Fontations (`skrifa`, `read-fonts`)
- `harfrust`
- RustCrypto
- es-runtime's Flate crate
- `otf-pixels`

## D3: No shared crypto crate
`otf-pdf` and es-runtime both depend directly on RustCrypto, pinned through workspace dependencies to the same versions. Cargo unifies compatible versions, so a wrapper crate adds nothing.

## D4: Read everything, emit the modern subset
- Read: PDF 1.0–2.0 and encryption R2–R6 (including RC4), because real-world files use them, including files with an empty password that are encrypted only to set permissions.
- Emit: AES-256 (R6) only; xref and object streams.

## D5: Output version
PDF 1.7 is the default; it is the base for PDF/A-2/3 (e-invoicing) and PDF/UA-1, and every viewer supports it. PDF 2.0 is opt-in and is required for PDF/A-4 and UA-2. AES-256 in 1.7 output uses an extension declaration (ExtensionLevel 8).

## D6: One object model, built in layers
A lazily parsed, immutable base plus a copy-on-write overlay, with incremental and full save. Chosen over a reader/writer split (the hayro and krilla model) because editing, incremental save and signing need a single editable model, and laziness keeps large files cheap.

## D7: Interpreter → Device abstraction
One content interpreter feeds text extraction, conformance checks, raster and SVG output through a `Device` trait.

## D8: Signatures, full modern scope, added in phases
- Verification covers legacy formats through PAdES B-LTA, DocMDP/FieldMDP, analysis of incremental updates, and online or offline revocation.
- Signing progresses B-B → B-T → B-LT → B-LTA.
- Signer, timestamp-authority and network access are injected traits.

## D9: Image codecs
Raster work and common codecs come from `otf-pixels`. The PDF-specific decoders (JBIG2, JPX, CCITT) are placed in Phase 8; whether they live in `otf-pixels` or `otf-pdf-render` is still open.

## D10: Runs on the CPU offload tier
Heavy `runtime:pdf` operations never run on the isolate thread, unlike WebCrypto in es-runtime.

## D11: Untrusted input, no ambient I/O, deterministic output
- Resource limits are enforced everywhere; no panics on any input.
- All I/O and randomness go through injected traits.
- Output is byte-for-byte deterministic for the same input and options.

## D12: XFA is detected and reported only
No XFA form engine.
