#!/usr/bin/env python3
"""Check that files we write are accepted by independent readers.

Our reader cannot validate our writer: a shared misreading of the standard
round-trips perfectly and is still wrong. So every file written here (full,
classic-xref, encrypted, incremental and merged saves of the committed
corpora and a sample of the external one) is opened by qpdf (through
pikepdf), which must report no structural problems and the right page count,
and by poppler's pdfinfo, which must agree on the page count.

    scripts/check-interop.py [external-sample-step]   (default 25)

Needs pikepdf and poppler-utils, and a built CLI (cargo build --release).
"""

import subprocess
import sys
import tempfile
from pathlib import Path

import pikepdf

root = Path(__file__).resolve().parent.parent
cli = root / "target" / "release" / "otf-pdf"
corpus = root / "corpus"


def run(*args):
    return subprocess.run([str(cli), *map(str, args)], capture_output=True, timeout=120)


def our_pages(path, password=""):
    r = run("info", "--password", password, path)
    for line in r.stdout.decode().splitlines():
        if line.startswith("Pages:"):
            return int(line.split()[1])
    return None


def poppler_pages(path, password=""):
    cmd = ["pdfinfo"] + (["-upw", password] if password else []) + [str(path)]
    r = subprocess.run(cmd, capture_output=True, timeout=60)
    for line in r.stdout.decode("utf-8", "replace").splitlines():
        if line.startswith("Pages:"):
            return int(line.split()[1])
    return None


def qpdf_problems(path, password=""):
    try:
        with pikepdf.open(path, password=password) as pdf:
            return pdf.check()
    except Exception as e:  # noqa: BLE001 - any failure to open is a finding
        return [f"cannot open: {e}"]


def check(path, expected_pages, password="", inherited=False):
    """Problems qpdf and poppler find in `path`, as strings. With
    `inherited`, qpdf's warnings are not counted: an incremental save keeps
    the original bytes, so damage qpdf already reported there remains."""
    problems = []
    try:
        with pikepdf.open(path, password=password) as pdf:
            if not inherited:
                problems += [f"qpdf: {p}" for p in pdf.check()]
            if len(pdf.pages) != expected_pages:
                problems.append(f"qpdf pages {len(pdf.pages)} != {expected_pages}")
    except Exception as e:  # noqa: BLE001 - any failure to open is a finding
        problems.append(f"qpdf cannot open: {e}")
    poppler = poppler_pages(path, password)
    if poppler != expected_pages:
        problems.append(f"poppler pages {poppler} != {expected_pages}")
    return problems


def main():
    step = int(sys.argv[1]) if len(sys.argv) > 1 else 25
    inputs = []  # (path, password)
    for p in sorted((corpus / "malformed").glob("*.pdf")):
        inputs.append((p, ""))
    for line in (corpus / "encrypted" / "expected.tsv").read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            name, user, *_ = line.split("\t")
            inputs.append((corpus / "encrypted" / name, user))
    external = sorted(p for p in (corpus / "external").rglob("*.pdf") if ".git" not in p.parts)
    inputs += [(p, "") for p in external[::step]]

    failures = 0
    checked = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        good = []
        for i, (src, pw) in enumerate(inputs):
            pages = our_pages(src, pw)
            if not pages:
                continue  # we cannot read it (or it has no pages): nothing to write
            damaged = bool(qpdf_problems(src, pw))
            variants = [
                ("full", [], ""),
                ("classic", ["--classic-xref"], ""),
                ("aes256", ["--user-password", "u", "--owner-password", "o"], "u"),
                ("incremental", ["--incremental"], pw),
            ]
            for label, flags, out_pw in variants:
                out = tmp / f"{i}-{label}.pdf"
                r = run("save", "--password", pw, *flags, src, out)
                if r.returncode != 0:
                    if label == "incremental":
                        continue  # refused by design (repaired or legacy-encrypted)
                    print(f"FAIL {src} [{label}]: save: {r.stderr.decode().strip()}")
                    failures += 1
                    continue
                checked += 1
                inherited = label == "incremental" and damaged
                for problem in check(out, pages, out_pw, inherited):
                    print(f"FAIL {src.relative_to(root)} [{label}]: {problem}")
                    failures += 1
            if not pw:
                good.append((src, pages))
        # One merge of several unencrypted inputs.
        sample = good[:: max(1, len(good) // 8)][:8]
        merged = tmp / "merged.pdf"
        r = run("merge", merged, *[s for s, _ in sample])
        if r.returncode != 0:
            print(f"FAIL merge: {r.stderr.decode().strip()}")
            failures += 1
        else:
            checked += 1
            for problem in check(merged, sum(n for _, n in sample)):
                print(f"FAIL merge: {problem}")
                failures += 1
    print(f"{checked} files written and checked, {failures} problems")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
