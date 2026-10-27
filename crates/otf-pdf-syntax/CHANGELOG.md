# Changelog

All notable changes to this crate will be documented in this file.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [SemVer](https://semver.org/).

## [Unreleased]

### Added
- Object model (`Object`, `Dict`, `Name`, `PdfString`, `Stream`, `ObjId`)
  with names and strings kept as bytes, and a deterministic serializer.
- `Source` and `Sink` traits for input and output; no I/O of its own.
- Header detection tolerating leading junk.
- Lenient lexer and parser with `Strictness` and a `Diagnostic` for every
  repair; configurable `Limits` and a CPU `Budget` hook.
- Filters: Flate (also raw deflate) with PNG/TIFF predictors, LZW, ASCII85,
  ASCIIHex, RunLength; image codecs passed through encoded.
- `File`: classic and stream xrefs, hybrid files, `/Prev` chains, object
  streams, lazy cached object loading, and repair by whole-file scan.
- Standard security handler R2–R6 for reading (RC4, AES-128, AES-256,
  Latin-1 and partial SASLprep password variants, embedded-files-only
  encryption), and AES-256 (R6) writing with `Permissions` and an injected
  `Entropy`.
