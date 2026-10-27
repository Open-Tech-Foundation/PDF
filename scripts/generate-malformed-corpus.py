#!/usr/bin/env python3
"""Generate the hand-made malformed corpus in corpus/malformed.

These files are committed. `tsr corpus:malformed` regenerates them and fails if
the checked-in bytes differ, so a hand edit cannot drift from this script. Each
file targets one repair path or resource limit from docs/PLAN.md §4 and §7;
corpus/malformed/README.md says which.

Output is deterministic: no timestamps, no randomness.

    scripts/generate-malformed-corpus.py [out-dir]
"""

import sys
import zlib
from pathlib import Path


def build(objects, version=b"1.7", trailer_extra=b"", prefix=b""):
    """A classic-xref PDF with correct offsets. `objects` maps number -> body."""
    out = bytearray(prefix)
    base = len(out)
    out += b"%PDF-" + version + b"\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    for num in sorted(objects):
        # Offsets are relative to the header, as readers resolve them for
        # files with leading junk.
        offsets[num] = len(out) - base
        out += b"%d 0 obj\n" % num + objects[num] + b"\nendobj\n"
    size = max(objects) + 1
    xref = len(out) - base
    out += b"xref\n0 %d\n" % size
    out += b"0000000000 65535 f \n"
    for num in range(1, size):
        if num in offsets:
            out += b"%010d 00000 n \n" % offsets[num]
        else:
            out += b"0000000000 65535 f \n"
    out += b"trailer\n<< /Size %d /Root 1 0 R%s >>\n" % (size, trailer_extra)
    out += b"startxref\n%d\n%%%%EOF\n" % xref
    return bytes(out)


def stream(data, extra=b"", length=None):
    if length is None:
        length = len(data)
    return b"<< /Length %d%s >>\nstream\n" % (length, extra) + data + b"\nendstream"


CONTENT = b"BT /F1 24 Tf 72 720 Td (Hello) Tj ET"


def page_objects(content=stream(CONTENT)):
    return {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]"
        b" /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        4: content,
        5: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    }


MINIMAL = build(page_objects())

# Exactly 39 bytes, so the CLI E2E test can assert the offset.
JUNK = b"Content-Type: application/pdf\r\n\r\nJUNK\r\n"
assert len(JUNK) == 39

FILES = {
    "minimal-valid.pdf": MINIMAL,
    "empty.pdf": b"",
    "not-a-pdf.pdf": b"%!PS-Adobe-3.0\nThis is PostScript, not PDF.\n",
    "malformed-version.pdf": b"%PDF-x.y\n" + MINIMAL[len(b"%PDF-1.7\n"):],
    "unknown-version.pdf": build(page_objects(), version=b"1.9"),
    "junk-before-header.pdf": build(page_objects(), version=b"1.4", prefix=JUNK),
    # Cut mid-object: no xref, no trailer, no %%EOF.
    "truncated.pdf": MINIMAL[: len(MINIMAL) // 2],
    # No xref table and no startxref: the reader must scan for `obj`.
    "missing-xref.pdf": MINIMAL[: MINIMAL.index(b"xref\n")] + b"trailer\n<< /Root 1 0 R >>\n%%EOF\n",
    # /Length is 10 bytes short; the reader must scan for `endstream`.
    "wrong-stream-length.pdf": build(page_objects(stream(CONTENT, length=len(CONTENT) - 10))),
    # /Length points at an indirect object that does not exist.
    "missing-length-object.pdf": build(
        page_objects(b"<< /Length 99 0 R >>\nstream\n" + CONTENT + b"\nendstream")
    ),
    # Object 5 has no `endobj` before the next object starts.
    "missing-endobj.pdf": MINIMAL.replace(
        b"/BaseFont /Helvetica >>\nendobj\n", b"/BaseFont /Helvetica >>\n"
    ),
    # Indirect references that form a cycle: 6 -> 7 -> 6, and Pages -> itself.
    "cyclic-refs.pdf": build(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R /Loop 6 0 R >>",
            2: b"<< /Type /Pages /Kids [2 0 R] /Count 1 >>",
            6: b"7 0 R",
            7: b"6 0 R",
        }
    ),
    # Arrays nested far past any sane recursion limit.
    "deep-nesting.pdf": build(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R /Deep 3 0 R >>",
            2: b"<< /Type /Pages /Kids [] /Count 0 >>",
            3: b"[" * 20_000 + b"]" * 20_000,
        }
    ),
    # 64 MiB of zeros in a ~64 KiB Flate stream (ratio ~1000:1): a
    # decompression bomb.
    "flate-bomb.pdf": build(
        page_objects(
            stream(zlib.compress(bytes(64 << 20), 9), extra=b" /Filter /FlateDecode")
        )
    ),
    # The xref subsection claims two billion entries.
    "huge-xref-count.pdf": MINIMAL.replace(b"xref\n0 6\n", b"xref\n0 2000000000\n").replace(
        b"/Size 6", b"/Size 2000000000"
    ),
    # startxref points past the end of the file.
    "startxref-past-eof.pdf": MINIMAL[: MINIMAL.rindex(b"startxref\n")]
    + b"startxref\n999999999\n%%EOF\n",
}


# Every xref offset points 7 bytes too early.
def wrong_xref_offsets(pdf):
    out = bytearray()
    for line in pdf.split(b"\n"):
        if line.endswith(b" 00000 n ") and len(line) == 19:
            line = b"%010d 00000 n " % (int(line[:10]) - 7)
        out += line + b"\n"
    return bytes(out[:-1])


FILES["wrong-xref-offsets.pdf"] = wrong_xref_offsets(MINIMAL)


def main():
    out_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent.parent / "corpus" / "malformed"
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, data in sorted(FILES.items()):
        (out_dir / name).write_bytes(data)
    print(f"wrote {len(FILES)} files to {out_dir}")


if __name__ == "__main__":
    main()
