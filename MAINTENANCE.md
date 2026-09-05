# Maintenance

ezpn is a Unix terminal multiplexer distributed as executables through Cargo and
GitHub Releases. A successful upload, passing unit tests, and a working terminal
session are different claims; report each separately.

## Invariants

- Read inactive as well as active PTYs; background jobs must not block on tab focus.
- Detach is client-local. A client failure is not evidence that a server is dead.
- Keep the last PTY geometry while headless. Readonly viewers cannot shrink writers.
- Authenticate local peers; never unlink a live socket after a failed probe.
- Stage replacement processes before destroying a current layout or snapshot.
- Make unknown/unsupported commands explicit errors, not successful placeholders.
- Track host capabilities separately from child negotiation. Re-encode keyboard
  and mouse input for the child; do not blindly forward host control bytes.
- Every input/output queue, control string, decoded snapshot and hook has a bound.
- Preserve failing reproductions. Do not ignore tests or lower coverage/security
  gates to make a release appear green.
- No absolute no-leak, universal compatibility or tmux/Zellij speed claims without
  matching measured evidence.

## Development

The pinned development toolchain is in rust-toolchain.toml. Actual MSRV is
Rust 1.88.0; CI invokes it explicitly, so the directory toolchain cannot override it.

```sh
python3 scripts/preflight.py --mode ci --ssh optional
```

Evidence lands in target/preflight. Exit codes and PASS/FAIL/SKIP are retained.
Use --ssh required on a host with isolated loopback sshd support. A skipped SSH
check is not a passed SSH test.

Preflight includes default and all-feature tests, real PTYs, strict Clippy,
actual MSRV, release/bench builds and script checks. Coverage preserves test
failures even when a report can still be generated.

## Runtime validation

Test shared and readonly clients, rapid resize, tiny viewports, CJK/wide glyphs,
application mouse modes, modal paste, Ctrl+D/Ctrl+E/Ctrl+W passthrough, background
tab output, failed snapshot load and clean/abrupt detach. Verify shell PID and
variables remain unchanged across live reattach.

Use isolated HOME/XDG paths and unique session names. Never inspect or terminate
the maintainer's unrelated sessions. Reap every process started by a test.
Forward LLVM_PROFILE_FILE explicitly to child test processes when collecting
coverage; do not forward arbitrary user environment.

GUI-emulator certification is separate from TERM-string/PTY tests. Maintain the
tested/untested distinction in docs/terminal-protocol.md and the release audit.

## Performance and soak

```sh
cargo bench --locked --bench render_hotpaths
python3 scripts/bench-regression.py --base BASE_COMMIT
bash tests/soak/run.sh --profile=smoke
```

The comparison freezes complete revisions and records all requested estimates.
Measure on a quiet machine. Snapshot/RSS proxy benchmarks are not end-to-end
snapshot timings or daemon RSS; label them as proxies. Record the real soak
duration, workloads, setup/steady-state samples, cycles, RSS and zombie counts.
Five minutes is not a 24-hour leak proof.

The optional render-diff path is bounded and bypasses unsupported frames.
Keep the default rendering path until the delta tradeoff has sufficient evidence.

## Bundled parser

src/vt100 contains the MIT vt100 0.16.2 source as a private module. See
src/vt100/UPSTREAM.md. Only documented boundary fixes differ from upstream plus
mechanical module paths/formatting. Keep original license and provenance.
Do not patch the global Cargo cache or use a local Cargo patch that disappears
from the published crate. Run parser regressions from the packaged sources.

## Release sequence

1. Review the complete diff and audit findings; keep unresolved work open.
2. Update README and all five translations, user guides, CHANGELOG, Cargo.toml
   and Cargo.lock. Never claim parser-only/reserved functionality is implemented.
3. Run local preflight and release-mode tests, coverage, audit/deny and benchmark
   comparisons. Inspect actual exit statuses, not the last command of a pipeline.
4. Use a conventional branch and PR. Never push release fixes directly to main.
   Wait for checks tied to the current head SHA, including performance and soak.
5. Merge only the verified head. Verify main checks and clean git state.
6. Create one annotated version tag on that merged revision. Published tags are
   immutable; a broken release needs a new patch version, not a moved tag.
7. Let the Release workflow build/test/package all native targets, attach checksums
   and publish to crates.io. Do not publish locally in parallel.
8. Verify the workflow, public registry version, release assets/checksums, and an
   extracted binary's version/smoke behavior before reporting release completion.

The build matrix targets macOS ARM64/Intel and Linux ARM64/x86-64.
Only completed jobs are evidence that a target passed. SSH on a loopback host
does not certify real WAN loss, remote reboot, arbitrary bastions or all terminals.

## Security upkeep

Run strict cargo audit and cargo deny. Existing unmaintained exceptions have
explicit rationale; new safety advisories require fixes. Scans include docs and
translated READMEs. Secrets must never appear in logs, diagnostics or snapshots
through interpolation. Hooks using sh -c are explicit shell execution and remain
the user's trust responsibility.

See docs/audits/v0.14.0.md for this release's measured evidence and residual risks.
