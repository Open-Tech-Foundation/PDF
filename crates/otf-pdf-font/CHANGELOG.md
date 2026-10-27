# Changelog

All notable changes to this crate will be documented in this file.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [SemVer](https://semver.org/).

## [Unreleased]

### Added
- Simple fonts (Type1 with built-in encodings, TrueType, Type3) and Type0/CID
  fonts, implementing the content layer's `Font`.
- Predefined, Differences and built-in encodings; embedded and predefined
  CMaps; ToUnicode; glyph-name and embedded-cmap fallbacks.
- Standard 14 font metrics and the Adobe Glyph List (from pdf.js) and
  Adobe-Japan1, GB1, CNS1 and Korea1 CID → Unicode tables (from Adobe
  cmap-resources).
- `FontCache`, loading each font once.
