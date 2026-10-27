#!/usr/bin/env bash
# Fetch the external test corpus listed in corpus/sources.tsv into
# corpus/external/<name>, each at its pinned commit. Idempotent: a source
# already at its pinned commit is skipped.
#
#   scripts/fetch-corpus.sh [name...]     (default: every source)
#
# Isartor (PDF/A-1b) has no public download that scripts can reach, so it is
# optional: set ISARTOR_ZIP to a local copy of isartor-pdfa-2008-08-13.zip and
# it is unpacked into corpus/external/isartor.
#
# corpus/external is git-ignored; the hand-made files in corpus/malformed are
# committed and need no fetch. Needs git (2.27+ for --filter with sparse
# checkout) and, for Isartor, unzip.
set -euo pipefail
# The manifest's sparse patterns (`/*`) must reach git unexpanded.
set -f

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/corpus/sources.tsv"
dest="$root/corpus/external"
mkdir -p "$dest"

wanted=("$@")
want() {
    [[ ${#wanted[@]} -eq 0 ]] && return 0
    local w
    for w in "${wanted[@]}"; do [[ "$w" == "$1" ]] && return 0; done
    return 1
}

fetched=0
while IFS=$'\t' read -r name repo commit paths; do
    [[ -z "$name" || "$name" == \#* ]] && continue
    want "$name" || continue
    dir="$dest/$name"
    if [[ -f "$dir/.otf-pdf-commit" && "$(cat "$dir/.otf-pdf-commit")" == "$commit" ]]; then
        echo "$name: already at ${commit:0:12}"
        fetched=$((fetched + 1))
        continue
    fi
    echo "$name: fetching $repo at ${commit:0:12}"
    rm -rf "$dir"
    git init -q "$dir"
    git -C "$dir" remote add origin "$repo"
    git -C "$dir" sparse-checkout set --no-cone $paths
    git -C "$dir" fetch -q --depth 1 --filter=blob:none origin "$commit"
    git -C "$dir" -c advice.detachedHead=false checkout -q FETCH_HEAD
    echo "$commit" > "$dir/.otf-pdf-commit"
    count="$(find "$dir" -path "$dir/.git" -prune -o -type f -iname '*.pdf' -print | wc -l)"
    echo "$name: $count PDF files"
    if [[ "$count" -eq 0 ]]; then
        echo "$name: no PDF files fetched; check the paths in $manifest" >&2
        exit 1
    fi
    fetched=$((fetched + 1))
done < "$manifest"

if want isartor && [[ -n "${ISARTOR_ZIP:-}" ]]; then
    echo "isartor: unpacking $ISARTOR_ZIP"
    rm -rf "$dest/isartor"
    mkdir -p "$dest/isartor"
    unzip -q "$ISARTOR_ZIP" -d "$dest/isartor"
    fetched=$((fetched + 1))
elif [[ ${#wanted[@]} -eq 0 ]]; then
    echo "isartor: skipped (set ISARTOR_ZIP to a local copy to include it)"
elif want isartor; then
    echo "isartor: set ISARTOR_ZIP to a local copy of the Isartor zip" >&2
    exit 1
fi

if [[ ${#wanted[@]} -gt 0 && "$fetched" -lt ${#wanted[@]} ]]; then
    echo "unknown source name in: ${wanted[*]} (see $manifest)" >&2
    exit 1
fi
