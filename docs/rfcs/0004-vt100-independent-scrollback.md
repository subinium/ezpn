# RFC 0004 — vt100-independent scrollback storage

| | |
|---|---|
| **Status** | Proposed |
| **Tracks issue** | #104 |
| **Blocked by** | RFC 0002 (vt100 strategy commitment — needs the soft-fork hook to capture scrolled-off rows) |
| **Required by** | RFC 0005 (memory budget SLA), real #68 eviction, #93 cell-grid render diff |
| **Owner** | @subinium |

## Summary

Today every pane stores its scrollback inside `vt100::Parser`. That has three consequences ezpn cannot work around without changing where the scrollback lives:

1. **No byte-aware eviction.** vt100 caps by line count only.
2. **No sparse-row encoding.** Every history row is a `Vec<Cell>` at full terminal width even when the row holds three visible characters.
3. **No public eviction primitive.** Per RFC 0002, vt100 0.15 has no public way to drop history rows.

The proposal: split the model. The active viewport stays in vt100. The history-of-scrolled-off-rows moves into an ezpn-owned `ScrollbackBuffer` with byte-bounded storage, sparse rows, and an eviction policy that actually evicts.

## Motivation

### Today's storage shape

`src/pane.rs:56` — `parser: vt100::Parser` owns *both* the active grid and the scrollback `VecDeque<Row>`. `src/pane.rs:222` — `vt100::Parser::new(rows, cols, scrollback_lines)` takes only a row count for the cap.

`src/pane.rs:222-298` — when bytes flow in:
1. `self.parser.process(&data)` — vt100 parses, scrolls oldest rows into history.
2. `self.scrollback_byte_estimate += data.len()` — telemetry counter.
3. `evict_if_oversized()` — checks budget, emits a tracing event when over, **does not actually evict** (see `src/pane.rs:264-298` honesty comment).

The eviction shim is documented at `src/pane.rs:282-283`:

> The eviction policy field is plumbed end-to-end and will become load-bearing once vt100 (or a fork) exposes the missing API.

### Workload pathologies the line-cap can't bound

- **Single 10 MiB long line.** A row at 80 columns is `Vec<Cell>` of length 80; line-cap × cell-size suggests ~640 KiB worst case per row, which is wrong by a factor of ~16 on this input. vt100 wraps the long line into many display rows but the underlying string slab stays large; the line-cap counts wrapped rows, not bytes.
- **Wide terminals.** A 300-column terminal with a 10K-line cap stores ~7.2 MB of `Cell` structs even when 99% of cells are default. Sparse encoding would cut this by ~10×.
- **Many quiet panes.** 100 panes × 10K-line cap × 80-column = ~240 MB of `Cell` objects sitting idle.

None of these are theoretical. `dump_text` (`src/pane.rs:521-563`) walks scrollback row-by-row precisely because vt100 stores them densely; the per-row iteration cost is what makes `ezpn-ctl dump` viable today, but the underlying slab is still there.

## Design

### Architecture

```
PaneState
├── viewport:   vt100::Parser              // active rows × cols, ~few KB
└── history:    ScrollbackBuffer (ours)    // byte-bounded VecDeque<HistoryRow>
```

vt100 (post RFC 0002 fork) calls a hook the moment a row scrolls off the visible screen. ezpn captures it into `ScrollbackBuffer`, applies sparse encoding, accounts the bytes, and (when over budget) evicts.

### Types

```rust
// src/scrollback.rs (new module)

/// One scrolled-off row, sparsely encoded.
///
/// `cells` is truncated to the index of the last non-default cell + 1
/// at capture time. A 300-col terminal with a row of `"hello"` stores
/// 5 cells, not 300.
pub struct HistoryRow {
    cells: Vec<Cell>,
    bytes_estimate: u32,        // sum of per-cell costs, cached for budget math
    timestamp: Option<u64>,     // OSC 133 prompt epoch, optional
}

/// Byte-bounded scrollback ring.
pub struct ScrollbackBuffer {
    rows: VecDeque<HistoryRow>,
    bytes: usize,
    byte_budget: usize,                 // 0 = unbounded; default 32 MiB
    eviction: ScrollbackEviction,       // OldestLine | LargestLine
    eviction_count: u64,                // telemetry, exposed via #71
}

pub enum ScrollbackEviction {
    OldestLine,    // pop_front until under budget
    LargestLine,   // remove top-N by bytes_estimate
}
```

`Cell` reuses ezpn's existing per-cell type (the same struct the renderer consumes). Mirroring `vt100::Cell` is intentional — the eventual snapshot v4 (RFC 0006) round-trips this directly without re-encoding.

### Lifecycle

```
PTY bytes
    │
    ▼
Pane::intercept       (OSC + Kitty CSI passes here first; see pane.rs:802)
    │
    ▼
parser.process(...)   (vt100 owns active screen)
    │
    ├── vt100 internal: row scrolls off
    │     │
    │     └─► [forked-only hook]  ScrollbackBuffer::push(HistoryRow)
    │                              ├── compute bytes_estimate
    │                              ├── self.bytes += bytes_estimate
    │                              └── while self.bytes > self.byte_budget: evict
    ▼
Pane::on_output_done  (frame coalescer / event publisher)
```

The hook is implemented inside the soft-fork branch (RFC 0002 blocker #8) as a callback registered at `Parser` construction. Within ezpn, `Pane::spawn_inner` (`src/pane.rs:164-249`) registers the hook before any byte processing begins.

### Memory accounting — `bytes_estimate`

Per `HistoryRow`:

```text
bytes_estimate = sum over cells of (
    sizeof(char_storage) +        // 4–16 bytes for SmallString-style inline; up to grapheme width
    sizeof(SgrAttrs) +            // 8 bytes packed
    1                             // dirty/wide flag byte
) + sizeof(Vec<Cell>) overhead    // 24 bytes (ptr, len, cap)
```

For a sparsely-populated ASCII row of 5 cells: ~85 bytes (vs ~5 KiB at 300-cols dense). For a fully-populated 80-col SGR-rich row: ~1.4 KiB.

Budget check fires after each push; eviction loops until `bytes <= byte_budget`. The loop is bounded by `rows.len()` — even pathological eviction is O(history depth).

### Eviction policies

- **`OldestLine`** (default). `pop_front` until under budget. Predictable, preserves recency.
- **`LargestLine`**. Walk `rows` once, remove the top-`k` by `bytes_estimate` until budget cleared. Useful when a single huge `terraform plan` line evicts hundreds of useful small rows under `OldestLine`. Cost: O(N) per overflow event; mitigated by overflow being rare under steady state.

The eviction enum already exists end-to-end (`src/pane.rs:103, 256-258, 1481`) plumbed from config; only the implementation changes.

### Sparse encoding rule

Capture-time truncation: walk `cells` from the right, find the last index where `cell != Cell::default()`, allocate `Vec<Cell>` with that length. Renderer reads of an out-of-bounds column return `Cell::default()` — equivalent to vt100's behaviour for trailing-blank cells today.

Indented blank lines (cell with non-default attrs but blank text) round-trip lossy unless the blank attrs differ from default; this matches vt100 0.15's behaviour and is acceptable per `src/workspace.rs:135-160` (`RowSnapshot` SGR handling).

### Copy-mode and search

`copy_mode.rs` reads from `vt100::Screen` today (8 sites — see RFC 0001 §Context). Two paths:

1. **Federated read.** `Pane::scrollback_lines()` returns an iterator that walks `ScrollbackBuffer.rows` first then yields `vt100` viewport rows. Copy-mode's existing logic stays unchanged; only the iterator backing it moves.
2. **`fn iter_text(&self) -> impl Iterator<Item = String>` on `ScrollbackBuffer`.** Used by regex search and `dump_text`. Avoids the per-row `vt100::Screen::rows(0, cols)` allocation cost that `dump_text` pays today (`src/pane.rs:548`).

Path (1) is mandatory for backward compat. Path (2) is an optimisation gated on the `ScrollbackBuffer` shipping.

## Risks & Mitigations

| Risk | Impact | Mitigation | Verify In Step |
|---|---|---|---|
| vt100 fork hook (RFC 0002 #8) lands late | This RFC blocks indefinitely | Polling fallback: `Pane::tick()` snapshots `parser.screen().scrollback()` and detects shrinkage; rows lost between ticks are unrecoverable but bounded | hook prereq |
| Sparse `Vec<Cell>` allocation churn fragments heap | RSS climbs over hours despite within-budget bytes | Use `smallvec::SmallVec<[Cell; 32]>` for rows ≤ 32 cells; benchmark against allocator overhead | step 3 |
| `LargestLine` O(N) per overflow | Pathological p99 latency on big writes | Cap to 32 evictions per overflow event; allow more on next overflow | step 4 |
| Copy-mode regex search across federated iterator | Search semantics change (e.g., wrapped lines) | Property test: equivalence to current vt100-only path on a 10K-row corpus | step 5 |
| Snapshot v4 round-trip diverges from live | User detaches, reattaches, sees different scrollback | RFC 0006 round-trip property test consumes `HistoryRow` directly | step 6 |

## Implementation Steps

| # | Step | Files | Depends On | Scope |
|---|------|-------|------------|-------|
| 1 | Create `src/scrollback.rs` with `ScrollbackBuffer`, `HistoryRow`, byte accounting unit tests | `src/scrollback.rs` | — | M |
| 2 | Land vt100 fork hook (RFC 0002 blocker #8) | external (vt100-rust fork) | RFC 0002 | M |
| 3 | Wire `ScrollbackBuffer` into `Pane`; remove `scrollback_byte_estimate` shim | `src/pane.rs` | 1, 2 | M |
| 4 | Implement `OldestLine` + `LargestLine` real eviction; close #68 telemetry comment | `src/pane.rs`, `src/scrollback.rs` | 3 | S |
| 5 | Federated `scrollback_lines()` iterator; update copy-mode reads | `src/pane.rs`, `src/copy_mode.rs` | 3 | M |
| 6 | Property test: federated read equivalence vs vt100-only baseline | `tests/property/scrollback.rs` | 5 | S |
| 7 | Snapshot v4 capture path — `HistoryRow` → `RowSnapshot` (handed off to RFC 0006) | `src/workspace.rs` | 3, RFC 0006 | S |

Steps 1 and 2 form the parallel group; 1 is pure ezpn-side and can land before the fork ships if `ScrollbackBuffer` is unit-tested in isolation.

## Acceptance criteria (per issue #104)

- [ ] **100 panes × 1M lines each, average 80 cols, peak 80 MB scrollback budget → daemon RSS < 60 MB** (RFC 0005 ceiling).
- [ ] **Single 10 MB long line → triggers `LargestLine` eviction; smaller history rows survive.**
- [ ] Soak test (#95) 24h: zero monotonic memory growth.
- [ ] Snapshot v4 round-trips `HistoryRow` losslessly (RFC 0006).
- [ ] Existing `dump_text` API preserved on `Pane`.

## Open Questions

- Should `HistoryRow.timestamp` be wall-clock or monotonic? OSC 133 (#82) wants wall-clock for replay; monotonic is cheaper and unique. Keep as `Option<u64>` and let the producer decide; document the convention.
- Is `LargestLine` worth shipping at all? `OldestLine` covers the typical workload; `LargestLine` is a guard against single-pathological-line inputs. Ship both — the cost is one match arm and one O(N) walk path.
- Compression of inactive rows? `gzip` of a row's text adds a CPU/RSS tradeoff; defer to v0.15 if RSS is still tight after sparse encoding.

## Decision Path / Recommendation

**Adopt.** Ship `ScrollbackBuffer` once RFC 0002's hook lands. Without the hook, the polling fallback exists but loses fidelity on bursty workloads — accepted only as a stopgap.

### Numbers

| Workload | vt100-only today | `ScrollbackBuffer` target |
|---|---|---|
| 100 panes × 10K lines × 80 cols, idle | ~240 MB cell-grid (line cap) | ≤ 60 MB (RFC 0005) |
| 1 pane, 10 MB single line | unbounded by line cap | ≤ `byte_budget` (32 MiB default) |
| 1 pane, 10K rows of 5-char output @ 300 cols | ~7.2 MB | ~850 KB (sparse 5 cells/row) |
| `dump_text` of 10K-row pane | O(rows) `set_scrollback` calls | O(1) iterator over `VecDeque` |

### Reversibility

If sparse encoding turns out wrong (heap fragmentation outweighs savings), the `Vec<Cell>` representation can be swapped for a packed string-of-runs without changing the public `ScrollbackBuffer` API. The hook contract is the load-bearing part; the row encoding is an internal detail.

## References

- Issue #104 — this RFC's tracking issue
- Issue #68 — byte-budget eviction (this RFC unblocks)
- Issue #93 — cell-grid render diff (consumes the same hook)
- Issue #71 — eviction telemetry (already wired; `eviction_count` field reuses the same emit site)
- Issue #95 — soak test (verifies steady-state RSS)
- RFC 0002 — vt100 strategy commitment (provides the fork hook)
- RFC 0005 — memory budget SLA (consumes the byte budget)
- RFC 0006 — snapshot v4 (round-trips `HistoryRow`)
- `src/pane.rs:56,222` — vt100 ownership of scrollback today
- `src/pane.rs:264-298` — telemetry-only eviction stub
- `src/pane.rs:521-563` — `dump_text` set_scrollback walk
- `src/pane.rs:1460-1489` — `compute_eviction` synthetic count
- `src/workspace.rs:135-160` — `RowSnapshot` SGR handling

Closes #104
