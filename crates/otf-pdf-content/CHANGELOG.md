# Changelog

All notable changes to this crate will be documented in this file.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [SemVer](https://semver.org/).

## [Unreleased]

### Added
- Content-stream tokenizer, including inline images.
- Interpreter tracking the graphics and text state, Form XObjects (cycle-
  and depth-limited), annotation appearances, marked content and the clip.
- `Device`, `Font` and `FontLoader` traits; `Matrix`.
