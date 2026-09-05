#!/usr/bin/env bash
# Source line coverage: 65% overall, 70% on protocol/layout/workspace.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo-llvm-cov >/dev/null
# One instrumented execution; subsequent reports do not rerun the tests.
test_status=0
bash scripts/isolated.sh cargo llvm-cov --locked --workspace --all-features --no-fail-fast --no-report || test_status=$?
cargo llvm-cov report --lcov --output-path lcov.info
cargo llvm-cov report --json --output-path coverage.json
cargo llvm-cov report --summary-only
coverage_status=0
python3 scripts/check-evidence.py coverage coverage.json || coverage_status=$?
if [ "$test_status" -ne 0 ]; then
    echo "FAIL: instrumented tests exited $test_status; report is incomplete execution evidence" >&2
    exit "$test_status"
fi
exit "$coverage_status"
