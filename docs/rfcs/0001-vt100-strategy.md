# RFC 0001 — vt100 crate strategy

| | |
|---|---|
| **Status** | Draft |
| **Tracks issue** | #72 |
| **Blocks** | #67 (`history-bytes` cap), #68 (long-line sparse storage), #69 (scrollback v3) |
| **Decision deadline** | Before the first commit of v0.13 memory work lands on `feat/v0.12.0-foundations` |
| **Owner** | @subinium |

## Summary

ezpn depends on the [`vt100`](https://github.com/doy/vt100-rust) crate for its terminal emulation kernel. Three of v0.13's load-bearing features (byte-budget scrollback, sparse long-line storage, scrollback persistence v3) require behavior the upstream crate does not currently expose. This RFC chooses among four implementation paths and locks the choice before any memory-management code lands.

## Context

### Current usage in ezpn

| File | Symbol | Purpose |
|------|--------|---------|
| `src/pane.rs:43, 172` | `vt100::Parser` field + constructor | Owns terminal grid + scrollback per pane |
| `src/pane.rs:277` | `screen()` accessor | Read-only handle to current grid |
| `src/pane.rs:331, 343` | `MouseProtocolMode`, `MouseProtocolEncoding` | Mouse mode dispatch |
| `src/copy_mode.rs` (8 sites) | `&vt100::Screen` parameter | Cell read for word motion, selection, search |
| `src/render.rs:1738` | `vt100::Color` → crossterm `Color` | SGR color conversion |
| `src/main.rs:2234` | `&vt100::Screen` parameter | One read site |

Total: ~16 call sites across 4 modules. The crate is not pervasively coupled — the surface is small enough to wrap behind a trait if we choose a shim approach.

### Upstream state (verified 2026-04-28)

- Latest published version: **0.16.2** (2025-07-12). ezpn currently pins `0.15`.
- Repository: <https://github.com/doy/vt100-rust>
- Stars: 113. Open issues: 11. Not archived.
- Recent commit cadence: PRs merged in July 2025; MSRV check added; not abandoned.
- License: MIT (compatible with our re-use).
- Maintainer: `@doy` (single maintainer — bus factor of 1).

### What v0.13 needs that vt100 0.16.2 does not provide

1. **Per-pane byte accounting on the scrollback ring.** vt100 caps scrollback by line count (`Parser::new(rows, cols, scrollback)`). It does not expose total bytes held nor a hook for byte-budget eviction.
2. **Sparse trailing-default-cell encoding.** vt100 stores `Vec<Cell>` per row at full width even when most cells are default. We need either a parser internal change or a wrapping layer that re-encodes scrolled-off rows.
3. **Scrollback serialization API.** Snapshot v3 needs to dump and restore the full scrollback state in a stable format. vt100 has no public serde derives or stable wire shape on its grid types.
4. **Eviction hook.** When the byte budget is exceeded, the eviction policy (`oldest_line`, `largest_line`) needs to call into the parser to drop rows.

## Options

### A. Upstream-only

File PRs against `doy/vt100-rust` exposing the four needs above. Wait for review/merge.

**Pros**
- Zero ezpn-side maintenance burden.
- Other downstream consumers benefit.
- Keeps the door open for future vt100 features (no fork drift).

**Cons**
- Single maintainer; merge timing unpredictable. v0.12 release blocks on third-party throughput.
- API design has to satisfy other downstream needs, not just ezpn's. Negotiation overhead.
- If `@doy` rejects the design (e.g., serde exposure of internal types), we are back to options B/C/D anyway, with sunk cost.

**Cost**: low LOC, high schedule risk.

### B. Soft fork (carry-patch)

Maintain a series of patches on top of `doy/vt100-rust`, applied locally. Use `[patch.crates-io]` in `Cargo.toml` to redirect resolution. PR each patch upstream over time; rebase as needed.

**Pros**
- Unblocks v0.13 immediately.
- Provides a working artifact to demonstrate the API to upstream during PR review.
- Easy revert if a patch lands upstream — drop our copy.

**Cons**
- Requires CI discipline to keep the patch series rebasing cleanly.
- Cargo `[patch]` adds friction for downstream binary distribution (cargo-install users get the upstream version unless we pin transitively).
- Diverges silently if upstream fixes our patch's bug differently.

**Cost**: medium LOC, medium maintenance.

### C. Hard fork (`ezpn-vt100` crate)

Publish a forked crate with a renamed namespace. Take ownership of the divergence permanently.

**Pros**
- No upstream coordination.
- Free to remove unused features and bend the API to ezpn's needs.

**Cons**
- ezpn now owns a terminal emulator. This is a large, persistent maintenance commitment — VT/CSI parsing has decades of edge cases.
- Misses upstream bug fixes unless we manually mirror.
- Cannot be justified for the four needs alone; scope creep risk is real.

**Cost**: high LOC, high persistent maintenance.

### D. Shim above the parser

Treat `vt100::Parser` as opaque. Manage byte accounting, sparse encoding, and serialization in an ezpn-owned layer (`ScrollbackLayer`) that intercepts at `Pane::read_output`. The layer keeps its own ring of compact line representations alongside the parser's grid.

**Pros**
- No vt100 modification.
- Full control of memory budget at our boundary.
- Fastest to implement: we already own `Pane`.

**Cons**
- Duplicates state (vt100's internal grid + our ring). Memory cost not necessarily lower than A/B if vt100 still holds its own copy.
- Copy mode (`copy_mode.rs`) reads from `vt100::Screen` directly today; with a shim the source of truth bifurcates. Either keep copy mode reading vt100 (and accept that scrollback in vt100 is bounded only by line count, not bytes) or rewrite copy mode against our shim.
- Eviction policy is enforced after vt100 has already allocated. Worst-case spikes (a single 100 MB line) still hit vt100 first.

**Cost**: medium LOC, no upstream dependency, but design tax on the bifurcated source of truth.

## Decision criteria

Evaluate options against these in order:

1. **Schedule certainty.** Can we ship v0.13 in ≤ 4 weeks of memory work without external dependencies?
2. **Memory budget enforcement strength.** Can a hostile workload (1 GB single line, 10 K panes) be bounded *before* vt100 allocates, or only after?
3. **Maintenance burden.** Quantified as LOC ezpn would own + expected hours/year for upkeep.
4. **Reversibility.** If the choice turns out wrong in 6 months, how expensive is the swap?

Quick scoring (1 = worst, 5 = best):

| Criterion | A. Upstream | B. Soft fork | C. Hard fork | D. Shim |
|-----------|-------------|--------------|--------------|---------|
| Schedule | 1 | 4 | 3 | 5 |
| Budget enforcement strength | 4 | 4 | 5 | 2 |
| Maintenance | 5 | 3 | 1 | 4 |
| Reversibility | 5 | 4 | 2 | 4 |

## Recommendation

Open this RFC with **B (soft fork) preferred, with A in parallel**. Concretely:

1. Fork `doy/vt100-rust` to `subinium/vt100-rust` on `feat/ezpn-extensions` branch.
2. Implement the four needs as separate commits, each PR-able upstream individually.
3. File PRs upstream the day each commit lands locally; do not wait.
4. Use `[patch.crates-io]` in `Cargo.toml` to consume our fork.
5. As upstream merges land, drop the corresponding patches and bump the pin.
6. If after one upstream review cycle (~2 weeks) the API shape is rejected, fall back to D (shim) for that specific need only — not a wholesale change of strategy.

C (hard fork) is a contingency only if `@doy` becomes unresponsive or the repo is archived during v0.12 work. Tracking: re-evaluate at the start of v0.13 memory work.

## Out of scope for this RFC

- Replacing vt100 entirely with a different VT parser (e.g., `alacritty_terminal`'s parser). Different scope; would warrant its own RFC.
- Upstreaming OSC 7 / OSC 8 / OSC 52 handling. Those are intercepted at the ezpn boundary today (`pane.rs:479-503`), not inside vt100.

## Open questions

- Does `@doy` accept serde derive on the grid types, or do we need to expose a parallel `as_serializable()` shape? File a discussion issue first.
- Is `[patch.crates-io]` compatible with `cargo install ezpn` from crates.io? Verify via test publish to a personal alt-registry before relying on it for distribution.

## Decision log

| Date | Decision | By |
|------|----------|----|
| TBD  | TBD      | TBD |

(Append rows in chronological order. Final decision must be recorded before the v0.13 memory work begins.)
