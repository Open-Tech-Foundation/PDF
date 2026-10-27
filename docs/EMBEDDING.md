# Embedding otf-pdf

How to put `otf-pdf` behind a host API: a JavaScript runtime's PDF module, a
server endpoint, a CLI. Everything here is the public API of the `otf-pdf`
crate. The `otf-pdf-*` crates under it are its implementation, and the
facade re-exports what an embedder needs.

## Threading

The API is synchronous. `Document::open` and every read block until done, so
call them off the host's event loop, on whatever blocking pool it already
has. `Document`, `File`, `Store`, `OpenOptions`, `SaveOptions`, `Limits`,
`Error`, `Page`, `Object`, `Diagnostic` and `SecurityHandler` are
`Send + Sync` (a test enforces it), so a document opened on one thread can be
read from another, or from several at once behind an `Arc`:

```rust,no_run
use std::sync::Arc;
use otf_pdf::{Document, OpenOptions};

# fn main() -> Result<(), otf_pdf::Error> {
# let bytes: Vec<u8> = Vec::new();
let doc = Arc::new(Document::open(bytes, OpenOptions::default())?);
let pages = doc.page_count()?; // from any thread holding the Arc
# Ok(())
# }
```

Objects are parsed on first access and cached inside the document; the cache
is internally locked, so concurrent readers need no extra synchronization.
There is no global state and no thread pool: work runs on the calling thread.

## Input

The library does no I/O of its own. Bytes come in through the `Source`
trait (`len` and `read_at`), implemented for `Vec<u8>` and `[u8]`. A host
with files or streams implements `Source` over its own handles; reads are
random access and arrive as small windows (about 4 KiB, grown on demand) plus
whole stream bodies, so a file-backed source never needs the whole file in
memory.

Encrypted files take the password in `OpenOptions::password`, as bytes. Pass
the UTF-8 the user typed; Latin-1 and partial-SASLprep variants are tried
automatically (DECISIONS.md D20). An empty password is always tried, so
files encrypted only to set permissions open with no input.

## Untrusted input

Every file is treated as hostile. Nothing panics on any input (fuzzed), and
all work is bounded by `Limits`:

| Limit | Default | Bounds |
|---|---|---|
| `max_depth` | 256 | Nesting of arrays, dictionaries, page and outline trees |
| `max_objects` | 8,388,607 | Object numbers accepted from xref or scan |
| `max_decoded_size` | 512 MiB | One stream's decoded size |
| `max_compression_ratio` | 1100 (after 16 MiB) | Decompression bombs |
| `max_xref_sections` | 1024 | `/Prev` chain length |
| `max_object_size` | 64 MiB | One object's syntax |
| `max_pages` | 1,000,000 | Pages enumerated |
| `budget` | none | A `Budget` hook charged per unit of work; return `false` to stop |

A server handling uploads should lower `max_decoded_size` and set a `budget`
that enforces its own time or work allowance; exceeding any limit returns
`Error::Limit` and never leaves partial state behind.

## Strictness and diagnostics

`Strictness::Lenient` (the default) repairs damaged files the way viewers do:
it rebuilds the xref, recovers the trailer or catalog, fixes wrong stream
lengths and recovers broken page trees. Every repair is recorded;
`Document::diagnostics()` returns them, so a host can surface "this file was
repaired" without failing. `Strictness::Strict` turns any needed repair into
an error, for validators.

## Errors

`Error` is a plain enum with `Display`. The cases a host usually maps:

- `InvalidPassword`: ask the user for a password.
- `UnsupportedEncryption`: a certificate-encrypted file or unknown scheme.
- `Limit(kind)`: the file exceeded a configured limit.
- `NoXref`, `NoRoot`, `Syntax`: the file is not a readable PDF.
- `Source`: the host's `Source` failed; its message is passed through.

## Saving

`Document::save_to(&mut sink, &options)` writes through the `Sink` trait
(implemented for `Vec<u8>`; a host implements it over its own file or
stream); `Document::save` returns the bytes. Edits (`select_pages`,
`rotate_page`, `append_pages_from`, `set_info`, `add_attachment`,
`set_outlines`, or low-level `store_mut()`) go into a copy-on-write overlay,
so the original bytes are never changed and `SaveMode::Incremental` can
append just the edits, as signatures need. Incremental saves are refused
with an error when they would be unsafe (DECISIONS.md D24); fall back to a
full save.

Encryption needs randomness, which the library never takes from the OS: pass
an `Entropy` implementation in `SaveOptions::entropy`, backed by the host's
CSPRNG. Saving without encryption needs none.

## Determinism

There is no randomness and no clock inside the library. The document ID
defaults to a hash of the content, dates are whatever the caller writes into
`Info`, and encryption randomness comes from the injected `Entropy`. So the
same document, edits and options always produce the same bytes, and with a
deterministic `Entropy` (in tests) so does encryption.

## Build

The facade forbids `unsafe` and depends only on the allowlisted crates in
`Cargo.toml` (DECISIONS.md D2). MSRV is 1.88.
