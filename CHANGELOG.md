# Changelog

All notable changes to the `otf-pdf` crates are documented in this file. The crates
release in lockstep: one version, one tag, one set of notes. Prefix each entry with the crate it
touches.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [SemVer](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-10-27

### Added
- **otf-pdf-syntax:** `Error::IncrementalSaveRefused`, so a refused
  incremental save has its own variant instead of being told apart by its
  message.
- **otf-pdf-font:** `FontCache` clones share one cache; `len` and `is_empty`.
- **otf-pdf-text:** `TextExtractor::with_fonts` and `TextExtractor::fonts`,
  so separate extraction calls on one document reuse loaded fonts.
- **otf-pdf:** `FontCache` re-exported at the top level.
- **otf-pdf-doc:** Sanitize categories `comments`, `hidden_annotations`,
  `thumbnails`, `navigation`, `web_capture` and `structure`, each counted in
  `SanitizeReport`; `metadata` also removes `/LastModified`.
- **otf-pdf-doc:** Sanitize profiles `SanitizeOptions::BASIC`, `PRIVACY` and
  `STRICT`, and `NONE` to build single categories from (D30).
- **otf-pdf-cli:** `sanitize --profile basic|privacy|strict`.
- **otf-pdf-content:** Content-stream writer `write_content` and
  `write_operation`, the inverse of `parse_content`; it refuses operations
  that would not read back as themselves and hex-encodes inline images whose
  data could be mistaken for `EI` (D31).
- **otf-pdf-doc:** `Document::set_page_content` replaces a page's content.
- **otf-pdf:** `content::write_content` and `content::write_operation`.
- **otf-pdf-doc:** Sanitize categories `hidden_layers` and `invisible_text`,
  with `SanitizeOptions::without_content` and `rewrites_content`;
  `PRIVACY` and `STRICT` include hidden layers, and only `ALL` removes
  invisible (OCR) text (D32).
- **otf-pdf-content:** `sanitize`, which cuts hidden optional content and
  invisible text from page content, Form XObjects and appearances without
  moving the visible text, then runs the document-level pass.
- **otf-pdf:** `otf_pdf::sanitize`, the full sanitizer.
- Decision D29: es-runtime builds `runtime:pdf` on the published facade
  in its own repository, amending D22.
- **otf-pdf-syntax:** Object model (`Object`, `Dict`, `Name`, `PdfString`, `Stream`, `ObjId`)
  with names and strings kept as bytes, and a deterministic serializer.
- **otf-pdf-syntax:** `Source` and `Sink` traits for input and output; no I/O of its own.
- **otf-pdf-syntax:** Header detection tolerating leading junk.
- **otf-pdf-syntax:** Lenient lexer and parser with `Strictness` and a `Diagnostic` for every
  repair; configurable `Limits` and a CPU `Budget` hook.
- **otf-pdf-syntax:** Filters: Flate (also raw deflate) with PNG/TIFF predictors, LZW, ASCII85,
  ASCIIHex, RunLength; image codecs passed through encoded.
- **otf-pdf-syntax:** `File`: classic and stream xrefs, hybrid files, `/Prev` chains, object
  streams, lazy cached object loading, and repair by whole-file scan.
- **otf-pdf-syntax:** Standard security handler R2–R6 for reading (RC4, AES-128, AES-256,
  Latin-1 and partial SASLprep password variants, embedded-files-only
  encryption), and AES-256 (R6) writing with `Permissions` and an injected
  `Entropy`.
- **otf-pdf-content:** Content-stream tokenizer, including inline images.
- **otf-pdf-content:** Interpreter tracking the graphics and text state, Form XObjects (cycle-
  and depth-limited), annotation appearances, marked content and the clip.
- **otf-pdf-content:** `Device`, `Font` and `FontLoader` traits; `Matrix`.
- **otf-pdf-font:** Simple fonts (Type1 with built-in encodings, TrueType, Type3) and Type0/CID
  fonts, implementing the content layer's `Font`.
- **otf-pdf-font:** Predefined, Differences and built-in encodings; embedded and predefined
  CMaps; ToUnicode; glyph-name and embedded-cmap fallbacks.
- **otf-pdf-font:** Standard 14 font metrics and the Adobe Glyph List (from pdf.js) and
  Adobe-Japan1, GB1, CNS1 and Korea1 CID → Unicode tables (from Adobe
  cmap-resources).
- **otf-pdf-font:** `FontCache`, loading each font once.
- **otf-pdf-doc:** `Document`: page tree with inherited attributes and recovery of broken
  trees, version, Info, XMP, outlines, named destinations and attachments.
- **otf-pdf-doc:** Copy-on-write `Store` overlay for edits.
- **otf-pdf-doc:** Full save (garbage collection, deterministic renumbering, compression,
  object and xref streams, stream deduplication, page-tree rebuild) and
  incremental save (original bytes kept as a prefix), with AES-256
  encryption.
- **otf-pdf-doc:** Page operations: select, reorder, delete (with outline and named
  destination cleanup), rotate, crop and media boxes; merge and extract
  across documents.
- **otf-pdf-doc:** Outline writing, Info editing with a matching XMP packet, attachments,
  and `sanitize` (scripts, risky actions, attachments, XFA, multimedia,
  optionally links and metadata).
- **otf-pdf-doc:** Text-string encoding and decoding.
- **otf-pdf-text:** `TextExtractor`: page text, `PageText` lines and words with positions in
  displayed page space, `/ActualText`, invisible OCR text, clipped text
  dropped.
- **otf-pdf:** The public API: reading, editing, saving, encryption, sanitizing and text
  extraction, re-exported from the `otf-pdf-*` crates; the `content` module
  for custom devices.

### Changed
- **otf-pdf-doc:** Incremental-save refusals (damaged xref, no `startxref`,
  RC4/AES-128 input, an encryption change) return
  `Error::IncrementalSaveRefused` instead of `Syntax` or
  `UnsupportedEncryption`.
- **otf-pdf-doc:** `SanitizeOptions` and `SanitizeReport` gain fields, so
  struct literals must name them or use `..` from a profile or `NONE`.
  `SanitizeOptions::ALL` now also covers the new categories.
- **otf-pdf-doc:** `Document::sanitize` refuses `hidden_layers` and
  `invisible_text` with `Error::WrongType`; use `otf_pdf::sanitize`.
- **otf-pdf-cli:** `sanitize` runs the full sanitizer, so `--profile
  privacy|strict` and `--all` also cut hidden layers (and `--all` invisible
  text).

### Fixed
- **otf-pdf-content:** A page content stream, Form XObject or annotation
  appearance that exceeds a `Limits` bound fails the page with
  `Error::Limit` instead of being skipped, so the page no longer reads as
  empty.
