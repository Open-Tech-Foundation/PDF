#!/usr/bin/env python3
"""Record poppler's page count for every fetched external corpus file.

The page-enumeration corpus test compares our count with this reference, an
independent implementation's answer. Files poppler cannot open are left out
(our count is then not compared). Needs pdfinfo (poppler-utils) and the
fetched corpus; the output is committed, so CI needs neither.

    scripts/generate-page-reference.py
"""

import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
corpus = root / "corpus"


def passwords():
    out = {}
    tsv = corpus / "passwords.tsv"
    for line in tsv.read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            key, _, pw = line.partition("\t")
            out[key] = pw
    manifest = corpus / "external/pdfjs/test/test_manifest.json"
    if manifest.exists():
        import json

        for e in json.loads(manifest.read_text(encoding="utf-8")):
            if "password" in e:
                out["external/pdfjs/test/" + e["file"]] = e["password"]
    return out


def main():
    pws = passwords()
    rows = []
    files = sorted(p for p in (corpus / "external").rglob("*") if p.suffix.lower() == ".pdf" and ".git" not in p.parts)
    for path in files:
        key = path.relative_to(corpus).as_posix()
        cmd = ["pdfinfo"]
        if key in pws:
            cmd += ["-upw", pws[key]]
        try:
            r = subprocess.run(cmd + [str(path)], capture_output=True, timeout=60)
        except subprocess.TimeoutExpired:
            continue
        for line in r.stdout.decode("utf-8", "replace").splitlines():
            if line.startswith("Pages:"):
                rows.append(f"{key}\t{line.split()[1]}")
                break
    out = corpus / "reference" / "pages.tsv"
    out.write_text(
        "# Page counts reported by poppler's pdfinfo for the external corpus.\n"
        "# Regenerate: scripts/generate-page-reference.py\n" + "\n".join(rows) + "\n",
        encoding="utf-8",
    )
    print(f"{len(rows)} of {len(files)} files have a reference count", file=sys.stderr)


if __name__ == "__main__":
    main()
