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

## D13: Pins for allowlisted crates es-runtime does not use
es-runtime has no `rc4`, `x509-cert` or `cms`. They are pinned to the generation es-runtime's other RustCrypto crates sit on, so no second `cipher` or `der` enters a build that links both:
- `rc4` 0.1 (`cipher` 0.4, like `aes` 0.8 / `cbc` 0.1)
- `x509-cert` 0.2 and `cms` 0.2 (`der` 0.7, like `spki` 0.7 / `pkcs8` 0.10)

Every other allowlisted crate is pinned to exactly es-runtime's version (D3). Flate is `flate2` 1.1.9 with its default pure-Rust `miniz_oxide` backend, which is what `runtime:archive` builds. Moving to the newer `der` 0.8 / `cipher` 0.5 generation happens together with es-runtime, not ahead of it.

## D14: otf-pixels from crates.io
`otf-pixels` is consumed as the published crate (0.5.0), not as a path or git dependency, so CI needs no second checkout and releases stay reproducible.

## D15: Toolchain, MSRV and CI
- Edition 2024; `rust-version` 1.88, the highest MSRV in the allowlisted set (`sha2` 0.11.1). It must stay at or below es-runtime's MSRV.
- Every CI command is a `tsr` task in `tasks.toml`, as in Pixels and es-runtime; GitHub Actions only installs tools and calls tasks.
- Workspace lints: `unsafe_code = "forbid"`, `missing_docs = "deny"`, and clippy `unwrap_used`, `expect_used`, `panic` denied (`indexing_slicing` warned, which `-D warnings` makes binding). Tests are exempt through `clippy.toml`.

## D16: Crates join the workspace in the phase that needs them
The PLAN §3 layout is the target, not a Phase 0 deliverable. Phase 0 adds only `otf-pdf-syntax` (the `Source` trait and header detection), the `otf-pdf` facade and `otf-pdf-cli`; each other crate arrives with the phase that fills it, so merged code carries no empty stubs. The CLI parses arguments by hand because no argument-parser crate is on the allowlist.

## D17: Dev-only tooling dependencies
The allowlist governs what otf-pdf crates link. Tools that never ship are recorded here instead:
- `libfuzzer-sys` 0.4, used only by the `fuzz/` crate, which is its own workspace and is never published. It is required by cargo-fuzz (PLAN §5, Phase 0).
- Python 3 (standard library only), for `scripts/generate-malformed-corpus.py`.
- git, for `scripts/fetch-corpus.sh`.
- pikepdf (qpdf), for `scripts/generate-encrypted-corpus.py`. Its output is committed; qpdf is used because it is an independent implementation of the encryption algorithms.

## D18: Test corpus
- External sources are listed in `corpus/sources.tsv`, each pinned to a commit and fetched by `scripts/fetch-corpus.sh` with a sparse, shallow checkout into the git-ignored `corpus/external/`. CI caches it on the manifest's hash.
- pdf.js: the PDFs in `test/pdfs`. Its `.link` files point to third-party URLs, many of them dead, and are not followed.
- PDFBox: the PDFs in the repository's test resources. The larger regression set its Maven build downloads from JIRA attachments is not fetched.
- veraPDF: the whole `veraPDF-corpus` repository, which also carries the PDF Association TWG test files.
- Isartor has no download a script can reach (pdfa.org refuses non-browser requests), so it is optional: `ISARTOR_ZIP` points the script at a local copy.
- Hand-made malformed files live in `corpus/malformed`, committed. They are generated by `scripts/generate-malformed-corpus.py`, and CI fails if the committed bytes differ from its output.
- Corpus checks compare failures against a known-failure list per check in `corpus/baseline/`. A new failure fails the test, and so does a listed file that now passes, which keeps the list shrinking.

## D19: Object model details
- `Dict` is a `BTreeMap<Name, Object>`: lookups stay O(log n) on hostile dictionaries with very many keys, and iteration (so any output) is sorted and deterministic. Source key order is not preserved; a repeated key keeps its last value. A `null` value reads as absent.
- `Stream::raw` holds the bytes between `stream` and `endstream` after decryption and before filters. Decryption happens when an object is first loaded, which is when it is first accessed, and the result is cached.
- Objects are parsed from a window of the `Source` that grows on demand (4 KiB, ×4, up to `Limits::max_object_size`); stream data is read separately by length, so large streams never pass through the parser.
- A wrong xref entry or a missing object triggers one lazy whole-file scan, and its result is reused for every later lookup.

## D20: Password handling
Passwords are taken as bytes. Each one is also tried in another encoding: Latin-1 for R2–R4 (whose passwords are PDFDocEncoding), and for R5/R6 a partial SASLprep that drops characters mapped to nothing and maps non-ASCII spaces. Full SASLprep needs NFKC normalization tables, which would mean a new dependency or a large generated table; it is left out until a real file needs it. When no password matches but strings and streams use the Identity crypt filter (`/AuthEvent /EFOpen`), the file opens unauthenticated and only embedded files stay encrypted. Public-key security handlers (`Adobe.PubSec`) are not supported; they need the recipient's private key, which belongs with Phase 5's key handling.
