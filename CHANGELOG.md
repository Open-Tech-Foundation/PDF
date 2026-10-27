# Changelog

All notable changes to the `otf-pdf` crates are documented in this file. The crates
release in lockstep: one version, one tag, one set of notes. Prefix each entry with the crate it
touches.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [SemVer](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-10-27

### Added
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
