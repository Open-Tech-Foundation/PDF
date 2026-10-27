# Changelog

Repository-level changes: the development CLI (`otf-pdf-cli`, not
published), CI, test corpus, scripts and docs. Each published crate keeps
its own `CHANGELOG.md` in its directory.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Workspace, lints, dependency allowlist and decisions log (`docs/`).
- CLI `otf-pdf`: `header`, `info`, `pages`, `objects`, `dump`, `text`,
  `save`, `merge`, `extract`, `delete`, `rotate`, `sanitize`.
- Test corpus: fetch script for pdf.js, PDFBox and veraPDF files at pinned
  commits; generated malformed, encrypted (qpdf) and Indic/CJK (Chromium,
  LibreOffice) fixtures; poppler page-count and text references; shrink-only
  baselines.
- Fuzz targets: header, lexer, filters, document, save, content, text.
- CI on GitHub Actions through `tsr` tasks, including qpdf/poppler interop
  and the external corpus; release configuration.
- Docs: `API.md`, `EMBEDDING.md`, `DECISIONS.md`.
