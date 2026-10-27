# Changelog

All notable changes to this project will be documented in this file.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [SemVer](https://semver.org/).

## [Unreleased]

### Added
- Phase 0 foundations: Cargo workspace (edition 2024, MSRV 1.88) with the
  external dependency allowlist pinned in `[workspace.dependencies]`, and
  workspace lints that forbid `unsafe` and deny undocumented public items,
  `unwrap`, `expect` and `panic`.
- `otf-pdf-syntax`: the injected `Source` trait for random-access input, and
  `find_header`, which finds the `%PDF-M.m` header in the first 1024 bytes,
  tolerating leading junk.
- `otf-pdf` facade re-exporting the public API.
- `otf-pdf` CLI (`otf-pdf-cli`) with `header`, `help` and `--version`.
- Test corpus: `scripts/fetch-corpus.sh` fetches pdf.js, PDFBox and veraPDF
  test files at pinned commits (`corpus/sources.tsv`); Isartor is optional via
  `ISARTOR_ZIP`. A generated, committed set of hand-made malformed files in
  `corpus/malformed`; corpus tests with a shrink-only known-failure baseline.
- Fuzz harness (`fuzz/`) with a `header` target, seeded from the malformed
  corpus.
- CI on GitHub Actions running `tsr` tasks: fmt, clippy, tests, docs, MSRV,
  the malformed-corpus check, external-corpus tests and a short fuzz run.
- `CHANGELOG.md`, and decisions D13–D18 in `docs/DECISIONS.md`.
