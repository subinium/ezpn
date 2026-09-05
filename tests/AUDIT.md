# Test And Pipeline Audit Handoff

Observed locally on macOS arm64, 2026-09-05. This is working-tree evidence,
not a published release, universal terminal guarantee, or completed 24h soak.
Other owners are changing production files; rerun gates on the final tree.

## Fully Read

- `tests/common/mod.rs`.
- All original integration files: `main`, `attach_smoke`, `detach_reattach`,
  `ipc_version`, `kill_session`, `multi_client`, `signal_handling`.
- All property files: `main`, `layout_invariants`, `protocol_roundtrip`,
  `workspace_migration`.
- Both original soak files: `tests/soak/README.md`, `tests/soak/run.sh`.
- Original `scripts/coverage.sh` and all newly added scripts/tests.
- All 11 workflows: bench, branch-naming, changelog, ci, commitlint,
  coverage, gitleaks, labeler, release-drafter, release, supply-chain.
- `.cargo/audit.toml`, `deny.toml`, `.gitleaks.toml`.
- Parent-added `tests/vt100_allocation_probe.rs` was read but not edited.
- Relevant callers: manifest/toolchain, CLI/server startup, attach/session,
  connection/handshake/protocol, pane creation, layout, snapshot migration,
  control commands and portable-pty's process/cwd/resize implementation.

## Fixed In This Ownership

- All six ignored integration scenarios are live; two empty placeholders
  now exercise version rejection and SIGTERM persistence against a daemon.
- Private 0700 short socket paths, isolated HOME/XDG/cwd, bounded subprocess
  waits and captures, explicit socket shutdown on drop, and actual JSON
  serialization replace the stale harness. Only LLVM_PROFILE_FILE is passed
  through env_clear, enabling coverage of real daemon/client subprocesses.
- Echo cannot satisfy marker checks: expected output is assembled by the
  shell. Detach tests assert retained variables and identical shell PIDs.
- Real PTYs cover four TERM values, restoration of raw/alternate-screen
  state, shared/readonly geometry, local detach, abrupt disconnect, 512-key
  bursts, inactive-tab 1MiB output, Ctrl-W, control layout PID preservation,
  rejected tiny splits, failed-load rollback and single-launch tab restore.
- Property migration tests now call the production migration CLI; equalize
  compares the complete serialized layout rather than only unchanged IDs.
- CI/preflight explicitly use locked/default/all-feature checks and the
  manifest's cargo +MSRV. libtest counts are parsed, not hardcoded. Empty,
  ignored, filtered or failing executions cannot pass. No-fail-fast collects
  remaining targets but preserves failure status.
- Coverage uses structured LLVM line counts, one instrumented run, strict
  65% overall and 70% critical-module floors; missing data fails. Reports
  are generated after test failures, but the test exit code stays nonzero.
- Benchmark baselines use the complete source revision; candidate sources
  are frozen with hashes. Four suites, matching metrics and three runs are
  mandatory. Mean CI lower bound >5% in two runs fails, with no fallbacks.
- Fuzz/soak/perf errors no longer become warnings. Release depends on CI,
  coverage, fuzz, perf/soak, supply-chain and secret scanning before builds.
- Four release artifacts run native release tests: Intel/ARM macOS and
  x86_64/ARM64 Linux; both executables are extracted/smoked and checksummed.
  Runner labels follow [GitHub's official runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
- Broad README/docs secret-scan exclusions were removed. Existing RustSec
  exceptions were not expanded; new unsound/yanked warnings fail audit.

## Measured Results

- Genuine Rust 1.82.0 failed on time-core 0.1.8's edition2024 manifest.
  Locked metadata requires Rust 1.88.0 through time/time-core/time-macros.
  Actual +1.88.0 all-target/all-feature checks passed, including the vt100
  update. Parent owns the manifest/lockfile changes.
- `target/preflight-candidate/status.json`: check, Clippy, MSRV, release
  build, bench build, SSH and diff-check passed. At the latest run, default
  tests were 642 passed / 2 failed / 0 ignored; all-features were 647 passed
  / 2 failed / 0 ignored. These are executions, not unique definitions.
- Integration 20/20, property 66/66 and allocation probe 1/1 passed in both
  configurations. Separate helper-parser tests: 5/5; actionlint/shellcheck
  and scans of scripts/tests/docs/README passed.
- `target/preflight-candidate/ssh/status.json`: real loopback sshd and three
  ssh -tt connections passed same-PID/state retention after clean detach
  and killed SSH transport. No system SSH config or user ~/.ssh changed.
- `target/soak-five-minute/status.json`: 300.038 measured seconds after
  2.848s setup, 5 panes, 291 RSS samples, 30 real lifecycle/snapshot cycles.
  RSS 50096 -> 45568 KiB (peak 50608), ratio 0.90961354, zero zombies and
  zero snapshot growth. This passed the unchanged 1.30/0/100MiB criteria.
- LLVM source line coverage: 72.1304% overall, protocol 90.6667%, layout
  90.2626%, workspace 95.0429%. Real server main-loop coverage was 65.32%.
  Instrumented tests still failed, so the coverage command correctly failed.
- cargo-audit --deny warnings passed after parent updated anyhow. The
  original anyhow 1.0.102 warning fixes at >=1.0.103 according to
  [RUSTSEC-2026-0190](https://rustsec.org/advisories/RUSTSEC-2026-0190.html).
- Complete Criterion comparison: 22 metrics across 3 runs, 10 failed the
  unchanged +5% confidence-bound gate. `target/bench-evidence/summary.json`
  records every metric. Render means improved about 56-74%, but decoder
  means regressed about 119-369%, alongside border-cache, allocation-proxy
  and some snapshot metrics. This is not an overall performance pass.

## Remaining Integration Gates

- At last preflight, production tests for soft-wrap/wide-padding copying
  and shrinking a wide-character parser before erase still failed.
  Parent must fix those and run formatting, then rerun the full preflight.
- Criterion evidence is under `target/bench-evidence/`. Retain any failed
  verdict; local concurrent builds/soak are a confound for small changes.
  No matched tmux/Zellij benchmark was performed.
- Rerun coverage on the final passing tree. Mouse runtime coverage was 0%;
  a percentage floor is not proof of complete terminal or UI coverage.
- Native Linux/Intel CI and release artifact execution are configured, not
  locally verified on this ARM Mac. Full 30-minute and 24-hour soaks,
  real GUI emulator/version matrices and remote-network tests remain.
- No commit, push, tag, version edit or release action was performed here.
