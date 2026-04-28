# RFC 0005 — Memory budget SLA

| | |
|---|---|
| **Status** | Proposed |
| **Tracks issue** | #105 |
| **Depends on** | RFC 0004 (vt100-independent scrollback — provides predictable per-pane numbers) |
| **Required for** | v1.0-rc readiness gate |
| **Owner** | @subinium |

## Summary

ezpn has no published memory budget. Defaults are inherited from v0.5 expedients (`scrollback_bytes = 32 MiB` per pane, no daemon-idle target, no 100-pane stress target). PRs that introduce leaks ship undetected because nothing measures.

This RFC publishes per-workload RSS ceilings as a contract, defines the measurement methodology, and adds a `memory-regression` CI gate that fails any PR exceeding the ceiling by > 5%.

## Motivation

### Today's status

| Question | Today's answer |
|---|---|
| Daemon idle RSS target | None |
| Per-pane overhead | "loose upper bound estimate" (`src/pane.rs:106-110`) |
| 100-pane RSS ceiling | None |
| Per-client attach overhead | None |
| CI gate for memory regression | None |
| Soak-test RSS assertion | None (#95 issue still open) |

`benches/rss_proxy.rs:1-11` is honest about the gap:

> Tracked metrics are the actual RSS thresholds from #99 (12 MB empty, 60 MB at 100 panes), but criterion measures *time*, not memory. This file provides a *proxy* for steady-state allocation cost. … Real RSS gating lives in the soak test (`tests/soak/run.sh`); these benches are the fast-feedback complement that runs on every PR.

The soak test referenced does not yet exist as an RSS gate; the proxy benches measure allocation *time*, not the resident set. There is no enforced ceiling anywhere in the pipeline.

### Why this matters now

v1.0-rc requires SLAs as contract. v0.13 ships memory-aware features (#68 byte-budget eviction, snapshot v3, named copy buffers with 16 MiB per-buffer cap × 100-buffer LRU). Without enforced ceilings, those caps are individually right but compose into an unknown total. A user with 100 panes + 100 named buffers + a snapshot save in flight has no documented memory expectation.

## Design

### Proposed ceilings

| # | Workload | Ceiling | Rationale |
|---|---|---|---|
| W1 | Daemon idle, no panes, no clients | **≤ 12 MB RSS** | bootstrap + tracing-subscriber + IPC listener + signal handlers |
| W2 | Daemon + 1 pane idle (`bash -c sleep 9999`) | **≤ 18 MB RSS** | adds vt100 parser + ScrollbackBuffer (RFC 0004) + PTY plumbing |
| W3 | Daemon + 100 panes idle | **≤ 60 MB RSS** | per-pane amortised ≈ 480 KiB |
| W4 | Per-client attach overhead | **≤ 256 KiB** | render scratch + per-client renderer state |
| W5 | Per-pane scrollback peak | **≤ `scrollback_bytes` (default 32 MiB)** | hard cap enforced by RFC 0004 |
| W6 | Snapshot save peak (transient) | **≤ 1.5× workspace size** | gzip + bincode encode arena |
| W7 | Daemon + 100 panes + 100 named buffers (16 MiB each cap) | **≤ 60 MB RSS + Σ(buffer bytes)** | named-buffer LRU is opt-in; bound = sum of populated buffers |

W3 derived: 12 MB (W1) + 100 × ~480 KB (per-pane state, sparse scrollback empty, no PTY traffic) = ~60 MB. The 480 KB figure comes from `vt100::Parser` minimal grid (24 × 80 × cell ≈ 30 KB) + `ScrollbackBuffer` empty (24 bytes for empty `VecDeque` + book-keeping) + `Pane` book-keeping (terminal-state, OSC carries, channels) + PTY handles. Headroom: ~430 KB per pane for steady-state allocator overhead.

W5 is per-pane; W3 is base-only. Total daemon RSS at steady state is `W3 + Σ(W5 actual usage)` capped at `W3 + n_panes × scrollback_bytes`.

### Measurement methodology

#### RSS sampling

- **Linux**: `/proc/self/status` `VmRSS:` line, polled by an in-process helper. Authoritative.
- **macOS**: `mach_task_basic_info_data_t::resident_size` via `task_info()`. Documented as advisory because COW pages count differently.
- **CI canonical platform**: Linux. macOS budgets are 1.25× the Linux number to absorb differences; failing macOS does not block, failing Linux does.

#### Workload runner

A new bench binary `ezpn-bench-memory` (lives at `benches/memory_workloads.rs`, harness=false) sets up each workload deterministically:

```text
ezpn-bench-memory --workload W1 --warmup 10s --measure 30s
```

Steps:
1. Spawn workload (no PTY randomness — feed deterministic byte streams via `yes | head -c N` style input).
2. Warm up 10 s (let allocator hit steady state).
3. Sample RSS at 1 Hz for 30 s. Take 99th percentile.
4. Emit JSON to stdout: `{"workload": "W3", "rss_p99_bytes": 58234112, "platform": "linux"}`.

#### Statistical confidence

Three runs per workload; median across runs. Variance > 10% across runs flags an unstable measurement and aborts the gate (without failing) — the user is expected to investigate the source of noise rather than retry blindly.

### CI gate

`memory-regression` workflow runs on every PR:

```yaml
# .github/workflows/bench.yml (extension; the file already exists)
- name: Memory regression
  run: |
    cargo run --release --bin ezpn-bench-memory -- --all-workloads --json > current.json
    python scripts/compare-rss.py current.json docs/perf/memory-baseline.json --threshold 5
```

`docs/perf/memory-baseline.json` is checked into `main`; updates require a maintainer commit (same workflow as `docs/benchmarks/baseline.json` per issue #99).

Gate failure modes:
- **Fail**: any workload > 5% over baseline.
- **Warn**: variance > 10% across the three runs (does not block merge; surfaces in PR comment).
- **Skip**: PR labelled `bench-skip` (same convention used elsewhere in the repo's CI).

### Workload definitions doc

`docs/perf/memory-budget.md` (new) carries:
- Each workload's exact bring-up steps (commands, env, config).
- The byte-stream fixture for piped input.
- The expected p99 RSS and the 5% headroom number.
- Instructions for reproducing locally: `cargo run --bin ezpn-bench-memory -- --workload W3`.

This doc is the single source of truth. CI reads `docs/perf/memory-baseline.json`; humans read the markdown to understand what each row means.

## Risks & Mitigations

| Risk | Impact | Mitigation | Verify In Step |
|---|---|---|---|
| GitHub runner variance blows 5% threshold | Flaky CI | Three-run median + 10% variance escape hatch + use `actions/cache` so steady-state hits faster | step 4 |
| macOS vs Linux RSS counting differences | macOS gate is unreliable | Linux-canonical; macOS advisory at 1.25× | step 4 |
| 12 MB daemon-idle (W1) is too aggressive given current dep tree | Gate fails on day 1 | Profile with `cargo bloat`; prune unused `tracing-subscriber` features; revisit ceiling if pruning hits diminishing returns | step 2 |
| Snapshot save peak (W6) is hard to measure (short-lived) | False negatives | Use `mallinfo2` peak fields (Linux) instead of polled RSS for W6 | step 3 |
| `ScrollbackBuffer` (RFC 0004) hasn't shipped yet | Per-pane numbers are unstable | Phase: ship W1/W4/W6 in v0.13.x; W2/W3/W5 land with RFC 0004 | step 5 |

## Implementation Steps

| # | Step | Files | Depends On | Scope |
|---|------|-------|------------|-------|
| 1 | Write `docs/perf/memory-budget.md` with all workload definitions | `docs/perf/memory-budget.md` | — | M |
| 2 | Profile dep tree, prune `tracing-subscriber` features, validate W1 | `Cargo.toml`, `src/observability.rs` | — | S |
| 3 | Write `benches/memory_workloads.rs` (`ezpn-bench-memory` binary) | `benches/memory_workloads.rs`, `Cargo.toml` | 1 | M |
| 4 | CI workflow `.github/workflows/bench.yml` extended with `memory-regression` job | `.github/workflows/bench.yml`, `scripts/compare-rss.py` | 3 | S |
| 5 | RFC 0004 lands → re-baseline W2/W3/W5; update `docs/perf/memory-baseline.json` | `docs/perf/memory-baseline.json` | RFC 0004, 4 | S |
| 6 | One deliberately-introduced regression PR validates the gate goes red | (test fixture) | 4 | S |
| 7 | Soak test (#95) extended to assert RSS ceiling at 24h mark | `tests/soak/run.sh` | 5 | S |
| 8 | CHANGELOG `[Unreleased]` Performance section template | `CHANGELOG.md` | 1 | S |

Steps 1, 2 are parallel (no shared files). Step 3 depends on step 1 for the workload definitions. Step 5 gates on RFC 0004's predictability.

## Acceptance criteria (per issue #105)

- [ ] `docs/perf/memory-budget.md` published with all SLAs + workload definitions + measurement methodology.
- [ ] `benches/memory_workloads.rs` covers all seven workload rows.
- [ ] CI job `memory-regression` runs on every PR; fails at > 5% over budget.
- [ ] CHANGELOG `[Unreleased]` template gains a Performance subsection populated by every release.
- [ ] One deliberately-introduced regression caught by the gate.
- [ ] Soak test (#95) extended with RSS-ceiling assertion at 24h.

## Open Questions

- **jemalloc / mimalloc opt-in?** Glibc's allocator on Linux is known to retain freed pages aggressively. A `--features mimalloc` build feature could cut steady-state RSS by 10–20%. Captured as separate experiment, not a v1.0-rc gate dependency.
- **Production telemetry — should the daemon report its own RSS over IPC?** Useful for users debugging "why is my ezpn daemon eating memory". Defer to v0.14 — adds attack surface and a new IPC variant.
- **Per-OS budget tables vs single canonical platform** — current proposal picks Linux as canonical, treats macOS as advisory at 1.25×. Reconsider if the macOS advisory consistently misses by > 25% in practice.
- **Should W7 (named buffers) be a hard ceiling or a derived bound?** Derived bound (`W3 + Σ(buffer sizes)`) is honest — buffers are user-data; capping them artificially is a UX regression. Hard ceiling on the LRU count (100, already enforced) is the actual contract.

## Decision Path / Recommendation

**Adopt.** Numbers above are the published contract; CI enforces them.

### Reversibility

Ceilings are not API. Bumping them up with maintainer rationale is acceptable in any release; bumping them down requires a soak-test soak demonstrating no regression. The CI gate's threshold (5%) is configurable in one place (`scripts/compare-rss.py`).

### Why 5% and not tighter

GitHub runner variance for RSS (across cold-cache vs warm-cache, allocator state at process start, transient page cache) is in the 2–4% range based on prior bench-regression noise (#99 work). 5% absorbs the noise floor while still catching the kind of regression that matters (a stray `Vec<u8>` per pane = ~80 bytes × 100 = 8 KB, well within noise; a stray `Vec<u8>` of 80 KB per pane = 8 MB, which trips W3 at ~13%).

## References

- Issue #105 — this RFC's tracking issue
- Issue #95 — soak test (consumes the ceilings)
- Issue #99 — perf regression suite (shares CI machinery)
- Issue #68 — byte-budget eviction (W5 hard cap)
- Issue #91 — named copy buffers (W7 derived bound)
- RFC 0004 — vt100-independent scrollback (provides predictable per-pane numbers for W2/W3/W5)
- `benches/rss_proxy.rs:1-11` — current proxy bench (allocation-time, not RSS)
- `src/pane.rs:106-110` — current per-pane "loose upper bound" comment
- `src/pane.rs:248` — `DEFAULT_SCROLLBACK_BYTES` (W5)
- `Cargo.toml:54-55` — tracing-subscriber feature set (W1 profiling target)
- CHANGELOG `[0.13.0]` § Wiring — current scrollback eviction telemetry status

Closes #105
