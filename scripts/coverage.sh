#!/usr/bin/env bash
#
# Coverage gate — fails if `cargo llvm-cov` reports total line coverage below
# the threshold. Used by CI's `coverage` job (cron + `area:test`-labeled PRs).
#
# Run locally:
#   cargo install cargo-llvm-cov
#   bash scripts/coverage.sh
#
# Override the threshold for local exploration:
#   COVERAGE_THRESHOLD=80 bash scripts/coverage.sh
#
# We deliberately keep the floor lower than the eventual goal (70%+). The
# current measured baseline is unknown; the floor exists to prevent
# *regressions* until we have a baseline to ratchet up to. See issue #25
# acceptance criteria.

set -euo pipefail

THRESHOLD="${COVERAGE_THRESHOLD:-65}"

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "ERROR: cargo-llvm-cov not installed." >&2
    echo "       Install with: cargo install cargo-llvm-cov" >&2
    exit 2
fi

# Run once to produce both lcov.info (for codecov upload) and the summary
# we parse the threshold from. `--workspace` keeps multi-crate setups
# correct; `--all-features` ensures the soak/test gates get measured too.
cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info

# `--summary-only` prints a TOTAL line like:
#   TOTAL    1234   200   83.79%   ...
# We extract the percentage column. Tools downstream of llvm-cov shuffle
# the column order across versions, so we grep for the literal `TOTAL`
# row and pick the first `NN.NN%` field.
summary="$(cargo llvm-cov --workspace --all-features --summary-only)"
echo "$summary"

cov="$(echo "$summary" \
    | awk '/^TOTAL/ {
        for (i = 1; i <= NF; i++) {
            if ($i ~ /[0-9]+\.[0-9]+%/) {
                gsub(/%/, "", $i);
                print $i;
                exit;
            }
        }
    }')"

if [[ -z "$cov" ]]; then
    echo "ERROR: could not parse TOTAL coverage from llvm-cov summary." >&2
    exit 3
fi

# Use awk for floating-point compare so we don't need bc on minimal CI images.
below="$(awk -v cov="$cov" -v t="$THRESHOLD" 'BEGIN { print (cov < t) ? "1" : "0" }')"

if [[ "$below" == "1" ]]; then
    echo "FAIL: coverage ${cov}% < threshold ${THRESHOLD}%"
    exit 1
fi

echo "PASS: coverage ${cov}% >= threshold ${THRESHOLD}%"
