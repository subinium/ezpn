# RFC 0002 — vt100 strategy commitment

| | |
|---|---|
| **Status** | Proposed |
| **Tracks issue** | #102 |
| **Supersedes** | RFC 0001 (`0001-vt100-strategy.md`) — *spike*, no decision logged |
| **Blocks** | #68 (byte-budget scrollback eviction), #93 (cell-grid render diff), RFC 0004 (vt100-independent scrollback) |
| **Decision deadline** | First commit of v0.13.1 memory work |
| **Owner** | @subinium |

## Summary

RFC 0001 (filed during v0.12.0) catalogued the vt100 0.15 API gap and laid out four strategy options (A: upstream-only, B: soft fork, C: hard fork, D: shim above the parser). The decision was deferred — `0001-vt100-strategy.md` ends with `Status: Draft` and an empty decision log.

v0.13.0 wiring made the deferral untenable. Two load-bearing v0.13/v0.14 features (#68 byte-budget scrollback eviction; #93 cell-grid render diff) ship today as **telemetry-only stubs** because vt100 0.15 has no public API for either. This RFC commits to a path so the wiring can become real.

The committed path is **B (soft fork) preferred, with A (upstream PRs) in parallel**, with a hard 4-week SLA before falling back to D (shim) for any individual blocker. C (hard fork) is contingency-only.

## Motivation

### What ships today as a stub

`src/pane.rs:283-298` (`evict_if_oversized`):

```text
What we do instead, honestly:
  * Maintain a running upper-bound estimate of bytes flowed through
    the parser (`scrollback_byte_estimate`).
  * When over budget, reset the estimate (the line cap baked into
    `vt100::Parser` still bounds memory in the worst case) and emit
    a telemetry event (#71). The estimate reset acts as a cooldown
    so we don't log every chunk after the first overflow.
```

That is the entire eviction implementation. The "policy is plumbed end-to-end and will become load-bearing once vt100 (or a fork) exposes the missing API" comment at line 282 is honest documentation that the feature is not actually implemented.

`src/pane.rs:1477-1489` (`compute_eviction`): pure function returning a *synthetic* row count derived from byte overflow ÷ `(cols × 4)`. No row is ever evicted; the count exists solely so the telemetry event has a non-zero `evicted_rows` field.

`src/pane.rs:521-563` (`dump_text`): walks `parser.set_scrollback(N)` from `max_scrollback` down to `0` in row-sized steps because there is no `screen().history_rows()` iterator. This is the documented workaround for the same gap that blocks #68.

### vt100 0.15 API audit — what's missing per blocker

Verified against the published `vt100 = "0.15"` source pinned in `Cargo.toml:39`. Public API surface that ezpn consumes today:

| Symbol | Mutability | What it gives us |
|---|---|---|
| `Parser::new(rows, cols, scrollback_lines)` | constructor | line-count cap only |
| `Parser::process(&[u8])` | `&mut` | feed bytes |
| `Parser::set_scrollback(offset)` | `&mut` | viewport offset; clamps silently |
| `Parser::screen()` | `&self` | read-only `&Screen` |
| `Screen::scrollback()` | `&self` | current viewport offset (not capacity, not byte size) |
| `Screen::rows(start, end)` | `&self` | iterator over visible row text |
| `Screen::size()` | `&self` | `(rows, cols)` |
| `Screen::cell(row, col)` | `&self` | `Option<&Cell>` |
| `Screen::title()`, `Screen::mouse_protocol_mode()` | `&self` | per-screen state |
| `Color`, `MouseProtocolMode`, `MouseProtocolEncoding` | enums | SGR + mouse |

Per blocker:

#### Blocker #68 — byte-budget scrollback eviction
Required APIs that **do not exist**:
1. `Screen::scrollback_byte_size() -> usize` — total bytes held in history rows.
2. `Parser::history_len() -> usize` — number of rows currently in history (`Screen::scrollback()` returns the *viewport offset*, not history capacity).
3. `Parser::evict_history_front(n: usize)` — drop oldest `n` history rows.
4. `Parser::evict_history_largest(n: usize)` — drop top-`n` by byte size, for `LargestLine` policy.

Without (3)+(4), `ScrollbackEviction::OldestLine` and `LargestLine` cannot be honoured. Without (1)+(2), telemetry is a guess.

#### Blocker #93 — cell-grid render diff
Required APIs:
5. `Screen::dirty_cells() -> impl Iterator<Item = (u16, u16)>` — cells modified since last clear.
6. `Screen::clear_dirty()` — caller checkpoint.
7. Stable hash or generation counter on `Cell` so the renderer can detect attribute-only changes.

vt100 0.15's `Cell` is `pub` but its `Eq` is structural over private fields; there is no per-cell version stamp. Diff producers must keep a shadow grid, doubling the per-pane cell-grid memory footprint.

#### Blocker RFC 0004 — vt100-independent scrollback
Required:
8. `Parser::on_row_scrolled_off(callback)` — hook fired when a row leaves the visible viewport. Without this, ezpn cannot capture rows into a parallel `ScrollbackBuffer` without polling.
9. Public `RowSnapshot`-equivalent type so the captured rows survive serialization without ezpn re-encoding from the row-text iterator (which loses SGR attribute fidelity).

### Upstream state (verified 2026-04-28; copied from RFC 0001 §Context)

- Latest published: `vt100 = "0.16.2"` (2025-07-12). ezpn pinned to `0.15`.
- Repo: <https://github.com/doy/vt100-rust>. 113 stars, 11 open issues, not archived.
- Maintainer: `@doy` — single-maintainer crate, bus factor 1.
- License: MIT.

The 0.15 → 0.16 bump landed MSRV/CI changes but no new public APIs that would close any of the gaps above; bumping the pin alone solves nothing.

## Design

### Decision: B (soft fork) preferred, A (upstream PRs) in parallel

Concretely:

1. **Fork** `doy/vt100-rust` to `subinium/vt100-rust` on a `feat/ezpn-extensions` branch. Each missing API (numbered 1–9 above) is one commit.
2. **Pin via `[patch.crates-io]`** in `Cargo.toml`:
   ```toml
   [patch.crates-io]
   vt100 = { git = "https://github.com/subinium/vt100-rust", branch = "feat/ezpn-extensions" }
   ```
   No new direct dep entry; the existing `vt100 = "0.15"` line stays.
3. **File upstream PRs** the day each commit lands locally. Do not batch.
4. **As upstream merges land**, drop the corresponding commit from the fork and bump the pin to the upstream release (eventually `0.16.x` or later).
5. **4-week SLA per gap.** If a specific PR has had no upstream review activity in 4 weeks, fall back to **D (shim)** for that one gap — *not* a wholesale strategy change.

The choice is the same as RFC 0001's recommendation; this RFC is the formal commitment plus the SLA.

### Why not C (hard fork)

vt100 is ~3500 LOC of CSI / OSC / SGR / DEC dispatch. Owning it permanently means owning every future terminal-emulation edge case (DECCOLM, BCE, alternate screen, character sets, CR/LF policy, …). Maintenance cost dwarfs the four-API-gap problem. C is justified only if `@doy` becomes unresponsive AND we cannot find a graceful exit (e.g., adopting `wezterm-term` per the v0.15 evaluation gate below).

### Why not (a) replacement (`avt` / `wezterm-term`)

`wezterm-term` carries a much larger API surface (window/font/glyph concerns we do not need) and pulls in non-trivial transitive deps. `avt` is single-maintainer too and does not store SGR-rich rows the way ezpn's renderer needs. A migration is a 16-call-site rewrite (per RFC 0001 §Context table) plus a regression-test corpus we do not have. The cost-benefit fails for v0.13–v1.0.

### Forked branch contract

The `feat/ezpn-extensions` branch is **rebased on top of upstream `main`**, never merged. Each commit:
- Adds exactly one public API (one of #1 through #9).
- Has an independent upstream PR open against `doy/vt100-rust`.
- Is tagged in the commit message with `Upstream-PR: doy/vt100-rust#<num>` once filed.

Upstream PRs are dropped via `git rebase --onto` when merged. Local-only patches accumulate at the tip; the `[patch.crates-io]` pin's git ref always points at the tip commit.

### Distribution risk — `cargo install ezpn`

`[patch.crates-io]` works for `cargo build` from this repo but is **not respected** by `cargo install` from crates.io (cargo deliberately strips `[patch]` when publishing). Two acceptable mitigations:

1. **Publish the fork to crates.io as `vt100-ezpn`**, rename the dep, drop `[patch]`. Cleanest, requires a one-line edit in every callsite.
2. **Document the constraint** in `CONTRIBUTING.md` and ship binaries via Homebrew / GitHub releases (which build from source against the patched workspace), accepting that `cargo install ezpn` from crates.io produces a degraded build with the v0.15 stubs.

This RFC picks **(1)** at the moment the first upstream PR is rejected (or stalls past 4 weeks) — until then, the patch flow is friction-free for contributors who clone the repo.

## Open Questions

- Does `@doy` accept serde derive on `Cell` / `Screen` for blocker (9), or do we need a parallel `as_serializable()` shape? File a discussion issue *before* the first PR.
- Is the `LargestLine` eviction policy worth shipping at all? Real workloads are dominated by oldest-row eviction. If `OldestLine` is the only policy users ever pick, blocker (4) drops out and the SLA shortens.
- Should the per-cell version stamp (blocker (7)) be a `u32` generation or a 64-bit hash? Generation is cheaper but wraps; hash is collision-prone at small widths. Defer to the upstream PR review.

## Decision Path / Recommendation

**Adopt B + A.** Concrete next-step list, in order:

| # | Step | Owner | ETA |
|---|------|-------|-----|
| 1 | Cut `subinium/vt100-rust` fork at upstream `0.15.2` tag | @subinium | day 0 |
| 2 | Land commit for blocker (1) + (2) — `Screen::scrollback_byte_size`, `Parser::history_len`. File upstream PR. | @subinium | day 1–3 |
| 3 | Land commit for blocker (3) — `Parser::evict_history_front`. File upstream PR. | @subinium | day 4–7 |
| 4 | Switch ezpn to `[patch.crates-io]`, replace `evict_if_oversized` stub with real eviction. Closes the #68 stub. | @subinium | day 8 |
| 5 | Land commit for blockers (5)+(6) — dirty-cell iterator + checkpoint. File upstream PR. | @subinium | day 9–14 |
| 6 | Replace `dump_text` scrollback walk with `Screen::history_rows()` iterator (blocker (8) prereq). | @subinium | day 15 |
| 7 | Re-evaluate at v0.13.1 ship: any blocker still open and >4 weeks since PR → activate D (shim) for that blocker. | @subinium | day 28 |

### Numbers

- **Per-pane scrollback after real eviction**: `≤ scrollback_byte_budget` (default 32 MiB, see `pane.rs:248`). Today: bounded only by line-cap × column-width × cell-size (~8 KiB/row × 10K rows = 80 MiB worst case).
- **Render-diff bytes-on-wire after blocker (5)+(6)**: target 30–60% reduction on text-heavy workloads. Today: full-frame is the only path.
- **Snapshot scrollback fidelity after blocker (9)**: lossless SGR round-trip. Today: `RowSnapshot.attrs` is a `Vec<u8>` placeholder (see `src/workspace.rs:155-158`).

### Reversibility

If 12 months in, `@doy` archives the repo or a competing crate (avt 1.0, wezterm-term split-out, etc.) becomes obviously better, the cost to switch is the same 16 call-sites RFC 0001 §Context catalogues. Soft-fork does not increase that cost; the API surface ezpn consumes is small enough to keep behind a thin internal wrapper if/when needed.

## References

- RFC 0001 — `docs/rfcs/0001-vt100-strategy.md` (prior art; spike that opened the four-option space)
- Issue #68 — byte-budget scrollback eviction
- Issue #93 — cell-grid render diff
- Issue #102 — this RFC's tracking issue
- `src/pane.rs:222` — `vt100::Parser::new` callsite
- `src/pane.rs:264-298` — `evict_if_oversized` stub with the limitation note
- `src/pane.rs:521-563` — `dump_text` set_scrollback walk
- `src/pane.rs:1460-1489` — `compute_eviction` synthetic row count
- `Cargo.toml:39` — `vt100 = "0.15"` pin
- `CHANGELOG.md` § [0.13.0] "Scrollback eviction telemetry" — current stub status

Closes #102
