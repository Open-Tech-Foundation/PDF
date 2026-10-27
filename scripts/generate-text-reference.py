#!/usr/bin/env python3
"""Pick the labelled text-extraction subset and record poppler's text for it.

From the fetched external corpus, files whose `pdftotext -raw` output is
mostly Latin, Indic or CJK text are chosen (deterministically, by path) and
their first pages' text is written to corpus/reference/text/, with
manifest.tsv listing file, script group and pages. The text corpus test
compares our extraction with it by character error rate.

    scripts/generate-text-reference.py

Needs pdftotext (poppler-utils) and the fetched corpus; the output is
committed, so CI needs neither poppler nor this script.
"""

import hashlib
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parent.parent
corpus = root / "corpus"
out_dir = corpus / "reference" / "text"
PAGES = 5
WANT = {"latin": 40, "indic": 15, "cjk": 20}


def group(text):
    letters = [c for c in text if c.isalpha()]
    if len(letters) < 200:
        return None
    indic = sum(0x0900 <= ord(c) <= 0x0DFF for c in letters)
    cjk = sum(
        0x3040 <= ord(c) <= 0x30FF or 0x4E00 <= ord(c) <= 0x9FFF or 0xAC00 <= ord(c) <= 0xD7AF
        for c in letters
    )
    latin = sum(c.isascii() for c in letters)
    n = len(letters)
    if indic / n > 0.5:
        return "indic"
    if cjk / n > 0.5:
        return "cjk"
    if latin / n > 0.9:
        return "latin"
    return None


def main():
    out_dir.mkdir(parents=True, exist_ok=True)
    for old in out_dir.glob("*.txt"):
        old.unlink()
    files = sorted(p for p in (corpus / "external").rglob("*.pdf") if ".git" not in p.parts)
    chosen = {g: [] for g in WANT}
    for path in files:
        if all(len(chosen[g]) >= WANT[g] for g in WANT):
            break
        try:
            r = subprocess.run(
                ["pdftotext", "-raw", "-enc", "UTF-8", "-l", str(PAGES), str(path), "-"],
                capture_output=True,
                timeout=60,
            )
        except subprocess.TimeoutExpired:
            continue
        if r.returncode != 0:
            continue
        text = r.stdout.decode("utf-8", "replace")
        g = group(text)
        if g is None or len(chosen[g]) >= WANT[g] or len(text) > 60000:
            continue
        key = path.relative_to(corpus).as_posix()
        name = hashlib.sha1(key.encode()).hexdigest()[:16] + ".txt"
        (out_dir / name).write_text(text, encoding="utf-8")
        chosen[g].append((key, name))
    rows = ["# file\tgroup\treference (poppler pdftotext -raw, first %d pages)" % PAGES]
    for g, items in chosen.items():
        rows += [f"{key}\t{g}\t{name}" for key, name in items]
    (out_dir / "manifest.tsv").write_text("\n".join(rows) + "\n", encoding="utf-8")
    print({g: len(v) for g, v in chosen.items()})


if __name__ == "__main__":
    main()
