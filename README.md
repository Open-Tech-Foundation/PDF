# PDF
A from-scratch Rust toolkit to parse, edit, sign, verify, generate, validate and render PDFs.

Status: Phase 0 (foundations). See [docs/PLAN.md](docs/PLAN.md) for the
roadmap and [docs/DECISIONS.md](docs/DECISIONS.md) for why things are the way
they are.

## Development

Tasks run through [tsr](https://tsr.opentechf.org); `tasks.toml` lists them.

```sh
tsr ci                 # what a pull request must pass
tsr corpus:fetch       # fetch the external test corpus (~450 MB)
tsr corpus:test        # corpus checks against it
tsr fuzz               # short fuzz run (nightly + cargo-fuzz)
cargo run -p otf-pdf-cli -- header file.pdf
```
