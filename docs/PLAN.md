# otf-pdf — Implementation Plan

Standalone, from-scratch PDF toolset in Rust (Apache-2.0), exposed in es-runtime as `runtime:pdf`.
Read `DECISIONS.md` before starting; it records why each constraint below exists.

## 1. Goal

One toolset for PDFs: **parse → manipulate → sign/verify → generate → validate → render**, delivered in phases.
Consumers:
- es-runtime (`runtime:pdf`)
- the JSX→PDF layout project, which uses `otf-pdf` as its PDF backend, as Typst uses krilla

## 2. Hard constraints

- **From scratch.** No PDF crates (lopdf, pdf-rs, hayro, pdf-writer, krilla, etc.). Study them; don't depend on them.
- **External dependency allowlist.** Anything else needs a new entry in `DECISIONS.md` first.
  - Fonts: `skrifa`, `read-fonts` (Fontations)
  - Shaping: `harfrust`
  - Crypto: RustCrypto crates (`aes`, `cbc`, `rc4`, `md-5`, `sha1`, `sha2`, `sha3`, `rsa`, `p256`/`p384`/`p521`, `ed25519-dalek`, `der`, `x509-cert`, `cms`, `spki`, `pkcs8`). Pin through `[workspace.dependencies]` at the **same versions es-runtime uses**.
  - Flate: the same crate es-runtime's `runtime:archive` uses (check its `Cargo.toml`)
  - Images: `otf-pixels`
- **Read everything, emit the modern subset.**
  - Read: PDF 1.0–2.0, encryption R2–R6 (including RC4).
  - Emit: PDF 1.7 (default) or 2.0, xref and object streams, AES-256 (R6) only. Never emit RC4 or AES-128.
- **No ambient I/O.**
  - Core crates take byte sources and sinks through traits.
  - Network work (OCSP, CRL, timestamp authority) goes through an injected trait.
  - Randomness comes from an injected entropy trait.
- **Untrusted input by default.** Every limit is configurable and enforced:
  - recursion depth
  - object count
  - decompression size and compression ratio
  - nesting of content streams and XObjects
  - CPU budget hooks
- **Deterministic output.** Same input and options produce identical bytes. Document IDs and timestamps are injectable.
- `#![forbid(unsafe_code)]` in every crate unless a `DECISIONS.md` entry allows it.

## 3. Workspace layout

```
otf-pdf/
  crates/
    otf-pdf-syntax    lexer, objects, xref, filters, encryption (read), repair
    otf-pdf-doc       lazy document, copy-on-write overlay, page tree, save (full/incremental)
    otf-pdf-content   content-stream parser, graphics state, interpreter → Device trait
    otf-pdf-font      font loading (Fontations + own Type1/Type3/CFF bits), CMaps, ToUnicode
    otf-pdf-text      text-extraction Device, layout reconstruction
    otf-pdf-forms     AcroForm, annotations, appearance-stream generation
    otf-pdf-sign      CMS, PAdES sign/verify, DSS, timestamps, revocation, MDP
    otf-pdf-write     high-level drawing/generation API, shaping, subsetting
    otf-pdf-conform   PDF/A and PDF/UA validation and output profiles, tagged PDF
    otf-pdf-render    raster/SVG Devices, image decoders (JBIG2, JPX, CCITT)
    otf-pdf           facade re-exporting a stable public API
    otf-pdf-cli       dev/test CLI (dump, extract, merge, sign, verify, validate)
  corpus/             test PDFs (git-lfs or fetched by script)
  fuzz/               cargo-fuzz targets
  DECISIONS.md
```

The dependency direction is strictly downward: `syntax ← doc ← content ← {font, text, forms, write, render}`, `sign` depends on `doc`, and `conform` depends on everything it validates.

## 4. Core design

**Object model**
- `Object` enum: Null, Bool, Int, Real, String (bytes and hex flag), Name, Array, Dict, Stream, Ref.
- `ObjId { num, gen }`.
- Names and strings are bytes, never `String`.

**Base layer (immutable, lazy)**
- `Source` trait gives random-access bytes (memory or file).
- The xref is parsed eagerly. Objects are parsed on demand and cached.
- The base layer is never mutated.

**Lenient parsing**
- Every parse path has a repair fallback:
  - rebuild the xref by scanning for `obj` markers
  - tolerate a wrong `/Length` (scan for `endstream`)
  - handle missing `endobj`, junk before the header, and trailers in odd places
- Each repair is recorded as a `Diagnostic` with location and kind.
- `Strictness::{Strict, Lenient}` controls whether repairs are allowed.

**Overlay layer (copy-on-write)**
- `Document` = base + overlay map (`ObjId → Object | Deleted`) + allocator for new IDs.
- Reads check the overlay first.

**Save**
- `SaveMode::Incremental` appends only overlay objects plus a new xref section. Base bytes are copied unchanged, which signatures require.
- `SaveMode::Full` rewrites and compacts: garbage-collects unreachable objects, uses object streams, and re-encrypts if asked.

**Encryption**
- Encryption sits between the syntax layer and the document layer. Strings and streams are decrypted on access.
- On write, objects are re-encrypted with AES-256 or written in clear.

**Content interpreter**
- Parses content streams into operators.
- Tracks graphics state, text state, resources, and Form XObject recursion.
- Calls a `Device` trait (`fill_path`, `stroke_path`, `draw_glyph`, `draw_image`, `begin_group`, `clip`, …).
- Text, render and conformance checks are all implementations of this trait.

**Errors**
- One `Error` type per crate with `thiserror`-style manual impls (no extra crates).
- No panics on any input. Fuzzers enforce this.

## 5. Phases

Each phase ends with the acceptance criteria met, `cargo test` and clippy clean, fuzzers running, the matching `runtime:pdf` surface added, and `DECISIONS.md` updated.

### Phase 0: Foundations
- Workspace, CI, lints, `forbid(unsafe_code)`, and the allowlisted crates in `[workspace.dependencies]`.
- Corpus fetch script: pdf.js test PDFs, PDFBox regression files, veraPDF corpus, Isartor, plus a hand-made set of malformed files.
- `otf-pdf-cli` skeleton.
- Fuzz harness skeleton.
- **Accept:** CI is green and the corpus is fetchable.

### Phase 1: Read (`syntax`, `doc` read-side)
- Lexer and object parser. Classic xref, xref streams, hybrid files, incremental chains (`/Prev`), object streams.
- Filters: Flate (with PNG/TIFF predictors), LZW, ASCII85, ASCIIHex, RunLength. DCT, JPX, JBIG2 and CCITT pass through as raw bytes.
- Decryption:
  - Standard security handler R2–R6 (RC4 40/128, AES-128, AES-256)
  - user and owner password
  - empty-password files open automatically
- Repair paths and diagnostics.
- Page tree with inherited attributes; Info dictionary and XMP metadata; outlines (read); attachments (read).
- CLI: `dump`, `info`, `pages`, `objects`.
- **Accept:**
  - The whole corpus opens without panics, and ≥ 99% of non-intentionally-broken files open with all pages enumerated.
  - Decryption matches a known-plaintext set.
  - The first fuzz targets are in place: lexer, xref and filters.

### Phase 2: Manipulate and save (`doc` write-side)
- Copy-on-write overlay, new-object allocation, and deep copy of objects across documents with ref remapping and dedup.
- Full save (object streams, xref streams, garbage collection, compression) and incremental save.
- AES-256 encryption on write, with permissions.
- Operations:
  - merge, split, extract and reorder pages
  - rotate and crop pages
  - delete pages, with cleanup of outlines and named destinations
  - edit metadata (Info and XMP kept in sync)
  - add/remove attachments, edit outlines
- **Accept:**
  - Round-trip (open → save full → open) preserves page count and content-stream bytes across the corpus.
  - An incremental save leaves the original bytes as an exact prefix of the output.
  - Output is accepted by pdf.js, qpdf `--check` and Acrobat (manual spot check).

### Phase 3: Content and text (`content`, `font`, `text`)
- Content-stream parser and interpreter with the full graphics and text state, inline images, Form XObjects, Type3 glyph procedures, marked content.
- Fonts:
  - Type1 (own parser), CFF/TrueType/OpenType through `skrifa`, Type3
  - standard-14 fonts via metrics tables (AFM data embedded)
  - encodings: predefined, Differences, built-in
  - CID fonts: CMaps (predefined and embedded), ToUnicode, CIDToGIDMap
- Text Device:
  - glyph runs with Unicode, position and font
  - reading order: lines, words, columns, rotation
  - ActualText from marked content
  - when ToUnicode is missing, fall back to glyph names then font cmaps
- API: plain-text per page, text with positions, word boxes.
- **Accept:**
  - On a labelled subset (English plus Indic plus CJK), extracted text matches reference output within a defined character-error-rate threshold.
  - No interpreter panics on the corpus.

### Phase 4: Forms and annotations (`forms`)
- AcroForm read: field tree, values, flags, choice options, calculation order.
- Fill fields, regenerate appearance streams (`/NeedAppearances` handled), flatten fields and annotations into page content.
- Annotations: read all types; create and edit Link, Text, Highlight/Underline/StrikeOut, FreeText, Stamp, Ink.
- XFA: detect and report only; no XFA engine.
- **Accept:** fill-then-flatten output renders correctly in pdf.js and Acrobat on the form subset of the corpus.

### Phase 5: Signatures (`sign`)
Verification:
- Signature formats:
  - legacy `adbe.pkcs7.detached`, `adbe.pkcs7.sha1`, `adbe.x509.rsa_sha1`
  - `ETSI.CAdES.detached`, `ETSI.RFC3161`
- Integrity:
  - ByteRange coverage and gap checks
  - classify changes made after each signature, by incremental revision (allowed vs disallowed; detects shadow attacks)
  - DocMDP and FieldMDP enforcement
- Algorithms:
  - RSA PKCS#1 v1.5 and PSS, ECDSA (P-256/384/521), EdDSA (ISO/TS 32002)
  - SHA-2 and SHA-3
  - SHA-1 accepted but flagged as weak
- Trust:
  - certificate path building and validation against configurable trust anchors
  - AATL and EUTL import
- Revocation:
  - OCSP and CRL, from DSS/VRI in the file or fetched live through the injected network trait
  - `RevocationMode::{Offline, Online, PreferEmbedded}`
- PAdES levels B-B, B-T, B-LT, B-LTA, including chains of archive timestamps.
- Report: one result per signature (integrity, signer, chain, revocation, timestamp, PAdES level, warnings).

Signing, in this order:
1. B-B: CMS SignedData, signing-certificate-v2, reserved `/Contents` placeholder, incremental save.
2. B-T: RFC 3161 timestamp through the injected timestamp-authority trait.
3. B-LT: add the Document Security Store (DSS) with certificates, OCSP responses and CRLs.
4. B-LTA: document timestamp.

Also certification signatures with DocMDP, and visible signature appearances.

The signer is a trait (`sign(digest) -> signature`) so keys can live in the runtime, a hardware module or a remote KMS.

**Accept:**
- Verification results match a reference validator (e.g. EU DSS) on a collected signed-PDF set, including tampered and shadow-attack samples.
- Our signatures validate in Acrobat and EU DSS at each PAdES level.

### Phase 6: Generation (`write`)
- High-level API: pages, paths, fills and strokes, colours (RGB, CMYK, ICC, spot), gradients, patterns, transparency groups, images, text, links, outlines, metadata.
- Text: `harfrust` shaping (Indic first-class), bidirectional text, font fallback list, font subsetting (TrueType/CFF, our own subsetter), ToUnicode generation so extraction round-trips.
- Images: decode and re-encode through `otf-pixels`; JPEG passes through untouched.
- Stable interface for the JSX layout project (it does layout; `write` emits).
- **Accept:**
  - Generated Indic and CJK text renders correctly in pdf.js and Acrobat.
  - Extracting text from generated files (using Phase 3) gives back the source text exactly.

### Phase 7: Conformance (`conform`)
- Tagged PDF: structure tree read and write; generation can emit tags.
- Validators: PDF/A-1/2/3/4 (b, u, a, plus e and f for A-4 where relevant) and PDF/UA-1/2. Rules are data-driven so new profiles can be added.
- Output profiles: generation and full save can target a profile and fail on violations.
- Factur-X/ZUGFeRD: PDF/A-3 with an embedded XML file and the matching metadata.
- **Accept:** results match veraPDF on its corpus for each supported profile.

### Phase 8: Render (`render`)
- Raster Device, drawing into `otf-pixels` buffers:
  - anti-aliased paths, all blend modes, soft masks, knockout and isolated groups
  - shading types 1–7, patterns, colour spaces
- SVG Device.
- Image decoders: JBIG2 (generic, symbol and halftone regions), JPX/JPEG 2000, CCITT G3/G4. These go in `otf-pdf-render` unless they can move into `otf-pixels`; record the choice in `DECISIONS.md`.
- **Accept:** pixel-difference comparison against reference renders (pdf.js/PDFium) on the corpus stays within a tolerance.

### Phase 9: Advanced
- True redaction: remove covered content and text from content streams, images, annotations and metadata; verify by extraction.
- Optimisation: deduplicate fonts and images, downsample images, remove unused objects.
- Linearisation (fast web view) on full save.
- Page comparison and diffing.

## 6. es-runtime integration (`runtime:pdf`)

- es-runtime depends on the `otf-pdf` facade crate.
- All heavy operations run on the CPU offload tier, never on the isolate thread.
- Capabilities:
  - none for operating on in-memory bytes
  - `fs` only through es-runtime's existing file APIs (the user passes a file handle or stream)
  - `net` for live OCSP/CRL and timestamp-authority calls
  - entropy for encryption and signing
- Injected traits are implemented by es-runtime's existing providers (entropy, net, crypto key material).
- Draft API, to be finalised per phase:
  ```js
  import { open, create } from "runtime:pdf";
  const doc = await open(bytesOrStream, { password, strict: false });
  doc.pageCount; doc.metadata; doc.diagnostics;
  await doc.page(0).text({ positions: true });
  const merged = await doc.edit().append(other).rotate(2, 90).save({ mode: "full", encrypt });
  const report = await doc.signatures.verify({ trust, revocation: "preferEmbedded" });
  const signed = await doc.sign({ signer, level: "B-LTA", tsa });
  const out = await create({ version: "1.7", profile: "PDF/A-3b" }) /* … */ .save();
  ```
- es-runtime is ESM-only and uses capability gating; follow existing `runtime:*` module conventions.

## 7. Testing

- Unit tests per crate. Corpus-wide integration tests (open, round-trip, extract, render) with a baseline file of known failures; the baseline can only shrink.
- Fuzzing: lexer, xref, filters, decryption, content interpreter, fonts, CMS parsing, from the phase each is introduced.
- Differential testing against qpdf (structure), pdf.js (text and render), veraPDF (conformance), EU DSS (signatures). These are dev-only tools, never dependencies.
- Limit tests: decompression bombs, deeply nested objects, cyclic references, huge or broken xref tables.

## 8. Rules for the coding agent

- Work one phase at a time, in order. Do not start the next phase until the current phase's acceptance criteria pass.
- Any new external crate, `unsafe`, or deviation from this plan: add a `DECISIONS.md` entry first, then stop and ask.
- Keep the public API in the `otf-pdf` facade; internal crates can change freely.
- At the end of each phase, update `DECISIONS.md` and add the matching `runtime:pdf` surface.
- Never emit legacy encryption. Never panic on input. Never do I/O outside the injected traits.
