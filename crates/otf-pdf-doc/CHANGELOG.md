# Changelog

All notable changes to this crate will be documented in this file.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [SemVer](https://semver.org/).

## [Unreleased]

### Added
- `Document`: page tree with inherited attributes and recovery of broken
  trees, version, Info, XMP, outlines, named destinations and attachments.
- Copy-on-write `Store` overlay for edits.
- Full save (garbage collection, deterministic renumbering, compression,
  object and xref streams, stream deduplication, page-tree rebuild) and
  incremental save (original bytes kept as a prefix), with AES-256
  encryption.
- Page operations: select, reorder, delete (with outline and named
  destination cleanup), rotate, crop and media boxes; merge and extract
  across documents.
- Outline writing, Info editing with a matching XMP packet, attachments,
  and `sanitize` (scripts, risky actions, attachments, XFA, multimedia,
  optionally links and metadata).
- Text-string encoding and decoding.
