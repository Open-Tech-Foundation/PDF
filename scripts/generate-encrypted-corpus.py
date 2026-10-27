#!/usr/bin/env python3
"""Generate the known-plaintext encryption corpus in corpus/encrypted.

The files are written by qpdf (through pikepdf), an independent
implementation, so decrypting them checks our reading of the standard against
someone else's rather than against our own encryptor. They are committed;
regenerating needs pikepdf and produces different bytes each time (qpdf picks
random salts and IVs), so CI does not regenerate them.

expected.tsv lists, per file: user password, owner password, the text drawn
on page 1 (which appears in its content stream), and the document title.

    scripts/generate-encrypted-corpus.py [out-dir]
"""

import sys
from pathlib import Path

import pikepdf

USER = "user"
OWNER = "owner"

# name, R, aes, encrypt metadata, user password, object streams
VARIANTS = [
    ("r2-rc4-40", 2, False, True, USER, False),
    ("r3-rc4-128", 3, False, True, USER, False),
    ("r3-rc4-128-empty-user", 3, False, True, "", False),
    ("r4-rc4-128", 4, False, True, USER, False),
    ("r4-aes-128", 4, True, True, USER, False),
    ("r4-aes-128-clear-metadata", 4, True, False, USER, False),
    ("r5-aes-256", 5, True, True, USER, False),
    ("r6-aes-256", 6, True, True, USER, False),
    ("r6-aes-256-empty-user", 6, True, True, "", False),
    ("r6-aes-256-unicode", 6, True, True, "pässwörd", False),
    ("r6-aes-256-object-streams", 6, True, True, USER, True),
]


def make(name):
    pdf = pikepdf.new()
    font = pdf.make_indirect(
        pikepdf.Dictionary(Type=pikepdf.Name.Font, Subtype=pikepdf.Name.Type1, BaseFont=pikepdf.Name.Helvetica)
    )
    content = f"BT /F1 24 Tf 72 720 Td (Secret {name}) Tj ET".encode()
    page = pikepdf.Page(
        pikepdf.Dictionary(
            Type=pikepdf.Name.Page,
            MediaBox=[0, 0, 612, 792],
            Resources=pikepdf.Dictionary(Font=pikepdf.Dictionary(F1=font)),
            Contents=pdf.make_stream(content),
        )
    )
    pdf.pages.append(page)
    pdf.docinfo["/Title"] = f"Title {name}"
    with pdf.open_metadata(set_pikepdf_as_editor=False) as meta:
        meta["dc:title"] = f"Title {name}"
    return pdf


def main():
    out_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent.parent / "corpus" / "encrypted"
    out_dir.mkdir(parents=True, exist_ok=True)
    rows = ["# file\tuser password\towner password\tpage text\ttitle"]
    for name, r, aes, metadata, user, objstm in VARIANTS:
        pdf = make(name)
        pdf.save(
            out_dir / f"{name}.pdf",
            # qpdf takes the metadata flag only for AES (R4 and up); otherwise the
            # file says what it does and the reader follows that.
            encryption=pikepdf.Encryption(owner=OWNER, user=user, R=r, aes=aes, metadata=metadata if aes else False),
            object_stream_mode=pikepdf.ObjectStreamMode.generate if objstm else pikepdf.ObjectStreamMode.disable,
            compress_streams=True,
            static_id=True,
        )
        rows.append(f"{name}.pdf\t{user}\t{OWNER}\tSecret {name}\tTitle {name}")
    (out_dir / "expected.tsv").write_text("\n".join(rows) + "\n", encoding="utf-8")
    print(f"wrote {len(VARIANTS)} files to {out_dir}")


if __name__ == "__main__":
    main()
