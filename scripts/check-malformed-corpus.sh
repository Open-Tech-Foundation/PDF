#!/usr/bin/env bash
# Regenerate the hand-made malformed corpus into a temporary directory and fail
# if it differs from the committed corpus/malformed (missing, extra or changed
# files), so a hand edit cannot drift from the generator that documents it.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

python3 -I "$root/scripts/generate-malformed-corpus.py" "$tmp" > /dev/null
if ! diff -r --brief "$tmp" "$root/corpus/malformed" --exclude=README.md; then
    echo "corpus/malformed is stale: run scripts/generate-malformed-corpus.py" >&2
    exit 1
fi
