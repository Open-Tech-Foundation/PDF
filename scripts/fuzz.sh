#!/usr/bin/env bash
# Run every fuzz target briefly: a regression gate, not a discovery campaign.
# `-max_total_time` bounds each run; any crash fails it. The hand-made files in
# corpus/malformed seed every target. Needs cargo-fuzz and a nightly toolchain.
#
#   scripts/fuzz.sh [seconds-per-target]   (default 60)
#
# `--target` is the nightly toolchain's host, passed explicitly: cargo-fuzz
# otherwise defaults to the triple it was itself built for, and a prebuilt
# cargo-fuzz (as CI installs) is static musl, for which no std is installed.
set -euo pipefail

seconds="${1:-60}"
target="$(rustc +nightly -vV | sed -n 's/^host: //p')"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/fuzz"

for fuzz_target in header; do
    mkdir -p "corpus/$fuzz_target"
    cargo +nightly fuzz run --target "$target" "$fuzz_target" \
        "corpus/$fuzz_target" "$root/corpus/malformed" \
        -- -max_total_time="$seconds"
done
