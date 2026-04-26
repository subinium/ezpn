# SPEC 05 — Render Loop Micro-perf

**Status:** Draft
**Related issue:** TBD (v0.10.0 milestone)
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** A. Stability & Resource Hygiene
**Severity origin:** Audit P2 #7 (`track_dec_modes` re-scan), P2 #8 (unbounded `wake_rx`), Nit #9 (`Vec<Tab>::insert` shifts)

---

## 1. Background

Three small but compounding inefficiencies sit on the render loop's
hot path. Individually each is a few microseconds; together they
matter for the 7-day soak budget and the PRD's ≤ 16 ms input-latency
target with N-pane workspaces. None of them is a structural change —
this SPEC bundles them because each touches one file and they share
the same review-and-test cadence.

### (a) `track_dec_modes` redundantly scans raw PTY bytes

`src/pane.rs:241` calls `track_dec_modes(&data, &mut self.bracketed_paste, &mut self.focus_events);`
on every chunk read from the PTY, *immediately before* `self.parser.process(&data)`.

`src/pane.rs:632-652`:

```rust
/// Track DEC private mode changes in raw PTY output.
fn track_dec_modes(data: &[u8], bracketed_paste: &mut bool, focus_events: &mut bool) {
    // \x1b[?2004h = enable bracketed paste, \x1b[?2004l = disable
    // \x1b[?1004h = enable focus events, \x1b[?1004l = disable
    const BP_ON: &[u8] = b"\x1b[?2004h";
    const BP_OFF: &[u8] = b"\x1b[?2004l";
    const FE_ON: &[u8] = b"\x1b[?1004h";
    const FE_OFF: &[u8] = b"\x1b[?1004l";

    for window in data.windows(BP_ON.len().max(FE_ON.len())) {
        if window.starts_with(BP_ON) {
            *bracketed_paste = true;
        } else if window.starts_with(BP_OFF) {
            *bracketed_paste = false;
        } else if window.starts_with(FE_ON) {
            *focus_events = true;
        } else if window.starts_with(FE_OFF) {
            *focus_events = false;
        }
    }
}
```

This is **O(N)** in PTY chunk size and runs for every chunk on every
pane. For a pane producing 100 KB/s of output, that's 100 KB of
windowed scanning *plus* the same 100 KB re-parsed by vt100 — exactly
double the work for information vt100 already tracks internally.

`vt100::Screen` exposes the relevant flags directly:

- `screen.bracketed_paste()` — `?2004h`/`l` state.
- `screen.application_keypad()`, `screen.application_cursor()`, etc.
- For focus events (`?1004`), vt100 0.15 does **not** expose a getter.
  We must keep some form of detection for focus events specifically
  but can read bracketed paste from vt100 directly.

**Caveat verified by reading vt100 source**: `vt100::Screen` does
not have a public `wants_focus` accessor in 0.15. The cleanest fix is:

- Drop the `BP_ON`/`BP_OFF` cases entirely and read
  `screen.bracketed_paste()` after `process()`.
- Keep a *narrowed* scanner that only looks for `?1004h`/`?1004l` —
  half the constant cost, and contained to one place.

### (b) Unbounded `wake_rx` mpsc

`src/pane.rs:14-18`:

```rust
pub fn init_wake_channel() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let _ = WAKE_TX.set(tx);
    rx
}
```

`mpsc::channel()` is **unbounded**. Every PTY chunk read invokes
`wake_main_loop()` which sends a `()` (`src/pane.rs:21-25`):

```rust
pub fn wake_main_loop() {
    if let Some(tx) = WAKE_TX.get() {
        let _ = tx.send(());
    }
}
```

The main loop drains them at `src/daemon/event_loop.rs:1217-1219`:

```rust
let _ = wake_rx.recv_timeout(Duration::from_millis(timeout_ms));
// Drain accumulated wake signals
while wake_rx.try_recv().is_ok() {}
```

Drain-on-each-tick keeps the queue bounded *in steady state*, but a
single slow tick (e.g. during a synchronous snapshot encode — see
SPEC 04) can let thousands of `()` messages accumulate. Each one is
an Arc-bumped enqueue + dequeue; cheap individually, expensive in a
batch. The wake messages are **idempotent** (the receiver only cares
that *some* wake happened, not how many) so we lose nothing by dropping
overflow.

### (c) `Vec<Tab>::insert` shifts tail on every tab switch

`src/tab.rs:89-107` (`switch_to`):

```rust
pub fn switch_to(&mut self, target_idx: usize, current: Tab) -> Option<Tab> {
    if target_idx == self.active_idx || target_idx >= self.count {
        return None;
    }

    // Step 1: Insert current tab at its logical position.
    let insert_pos = self.active_idx.min(self.tabs.len());
    self.tabs.insert(insert_pos, current);
    // Now `self.tabs` has all `count` tabs in logical order.

    // Step 2: Remove the target tab by its logical index (direct index).
    let target_tab = self.tabs.remove(target_idx);

    self.active_idx = target_idx;
    Some(target_tab)
}
```

Every tab switch does one `Vec::insert` + one `Vec::remove`, both of
which shift the tail. With N tabs the cost per switch is O(N) — a
fork-of-tmux user with 30+ tabs feels this on every Ctrl+B n. Each
`Tab` is a non-trivial struct (HashMap moves), so the shift cost
matters.

`VecDeque<Tab>` doesn't help directly because we still want random
access. The fix is more structural: replace the "gap in vec" data
model with a `VecDeque<Tab>` that holds *all* tabs (no unpacking) plus
a cursor index. The active tab's locals stop being unpacked locals
and become reads through `tabs.get(cursor)`. **However**, that's a
bigger refactor than this SPEC's scope.

The targeted fix: replace `Vec<Tab>` with `VecDeque<Tab>` and keep the
gap model. `VecDeque::insert` and `VecDeque::remove` are still O(N)
in the worst case (they shift the *shorter* end), so the worst-case
constant-factor improvement is ~2×. The bigger win is a separate
follow-up; this SPEC adopts the `VecDeque` switch as a minimal,
forward-compatible scaffold.

---

## 2. Goal

Reduce per-frame and per-keystroke CPU on the daemon main loop:

1. Eliminate the linear-in-PTY-bytes scan for bracketed paste; halve
   the cost of `track_dec_modes` by scoping it to focus events only.
2. Cap `wake_rx` at 64 pending wakes; safe because the message is
   idempotent.
3. Switch `TabManager::tabs` from `Vec<Tab>` to `VecDeque<Tab>` for a
   ~2× constant-factor improvement on switch/insert/remove.

Combined target: ≥ 5 % reduction in main-loop tick CPU under the soak
workload (4 panes × `yes`); zero functional regression.

---

## 3. Non-goals

- Replacing `vt100` for performance reasons. Out of scope.
- Full restructuring of `TabManager` to use `VecDeque<Tab>` *with* a
  cursor (no gap model). Tracked as a v0.11 follow-up; current SPEC
  is a drop-in container swap.
- Replacing `std::sync::mpsc` with crossbeam everywhere. Only
  `wake_rx` switches to a `sync_channel` for bounding.
- A full perf budget framework. v0.10 just ships the three fixes; the
  audit's larger "render-loop budget" idea is for a future SPEC.

---

## 4. Design

### 4.1 Read bracketed paste from vt100, scope `track_dec_modes` to focus events

`src/pane.rs:241` (call site) becomes:

```rust
self.parser.process(&data);
// Bracketed paste flag — read from vt100 after process(), not by
// re-scanning raw bytes. (vt100 already parses ?2004h/l.)
self.bracketed_paste = self.parser.screen().bracketed_paste();
// Focus events still need a manual scan because vt100 0.15 does not
// expose a getter for ?1004h/l state.
track_focus_events(&data, &mut self.focus_events);
```

The new helper, replacing `track_dec_modes`:

```rust
/// Track focus-event mode changes in raw PTY output.
///
/// vt100 0.15 does not expose `?1004h`/`l` state via the public API,
/// so we still scan for it — but only for this single mode pair, not
/// for everything. (Bracketed paste `?2004` is read from
/// `Screen::bracketed_paste()` in the caller.)
fn track_focus_events(data: &[u8], focus_events: &mut bool) {
    const FE_ON: &[u8] = b"\x1b[?1004h";
    const FE_OFF: &[u8] = b"\x1b[?1004l";
    // FE_ON.len() == FE_OFF.len() == 8; one window size suffices.
    for window in data.windows(FE_ON.len()) {
        if window == FE_ON {
            *focus_events = true;
        } else if window == FE_OFF {
            *focus_events = false;
        }
    }
}
```

Cost change: roughly halved — one constant comparison per window
instead of four, and the body is shorter so LLVM can keep it tight.

The struct field `bracketed_paste: bool` (`src/pane.rs:59`) becomes
either:

- A computed accessor (`pub fn bracketed_paste(&self) -> bool {
  self.parser.screen().bracketed_paste() }`), removing the field
  entirely; **or**
- A cached field updated after every `process()` call.

**Decision**: cache (option 2). The accessor is called per-keystroke
encoding (key handler decides whether to wrap pasted text in `\e[200~ … \e[201~`);
the screen lookup is cheap but a field load is cheaper, and the
cache is updated exactly where the underlying state can change
(after `process()`).

### 4.2 Bound `wake_rx`

`src/pane.rs:14-25` becomes:

```rust
const WAKE_CHANNEL_CAPACITY: usize = 64;

pub fn init_wake_channel() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::sync_channel::<()>(WAKE_CHANNEL_CAPACITY);
    let _ = WAKE_TX.set(tx);
    rx
}

pub fn wake_main_loop() {
    if let Some(tx) = WAKE_TX.get() {
        // Wake messages are idempotent — drop on overflow. The receiver
        // already drains all pending wakes per tick (event_loop.rs).
        let _ = tx.try_send(());
    }
}
```

`WAKE_TX` type changes from `OnceLock<mpsc::Sender<()>>` to
`OnceLock<mpsc::SyncSender<()>>`. All call sites of `wake_main_loop`
(no signature change) remain valid.

The main loop's drain at `event_loop.rs:1217-1219` is unchanged:

```rust
let _ = wake_rx.recv_timeout(Duration::from_millis(timeout_ms));
while wake_rx.try_recv().is_ok() {}
```

Verification: the receiver's contract is "block until something woke
me, then drain". Capacity 64 is large enough that under steady state
(< 60 fps × handful of panes) the queue is always near-empty;
overflow only happens when something genuinely stalls the main loop,
in which case extra wake messages are noise and dropping them is
safe.

### 4.3 `VecDeque<Tab>` swap

`src/tab.rs:46-55` (`TabManager` struct):

```rust
pub struct TabManager {
    /// Inactive tabs stored in logical order with the active position as a gap.
    tabs: std::collections::VecDeque<Tab>,
    pub active_idx: usize,
    pub count: usize,
    next_id: usize,
}
```

All call sites of `self.tabs.insert(pos, …)` and `self.tabs.remove(pos)`
remain semantically identical — `VecDeque` exposes the same methods
with the same indexing semantics, just better worst-case constants.

The iterator chain in `tab_names` (`src/tab.rs:198-210`) uses
`self.tabs.iter()`, which `VecDeque` supports directly.

Existing tests (`src/tab.rs:213-429`) construct `TabManager` only
through public methods (`new`, `from_tabs`, `create_tab`, `switch_to`)
and inspect via `tab_names` / `active_idx` / `count`. All public API
and behaviour are preserved; tests pass with the container swap.

For `from_tabs` (`src/tab.rs:72-85`) the `let mut tabs: Vec<Tab>`
parameter stays a `Vec<Tab>` (caller convenience); we convert with
`tabs.into()` when storing.

### 4.4 (Optional, in-scope) Cursor-index forward-compat

To make a future v0.11 "no-gap, cursor-only" model a clean diff, we
introduce **no other changes** in this SPEC. The current "gap model"
contract is documented at the top of the file already; we leave that
unchanged.

---

## 5. Surface changes

### IPC / wire protocol

None.

### CLI (ezpn-ctl)

None.

### Config (TOML)

None.

---

## 6. Touchpoints

| File | Lines (approx) | Change |
|---|---|---|
| `src/pane.rs` | 14–25 | Switch `WAKE_TX` to `SyncSender`, `init_wake_channel` to `sync_channel(64)`, `wake_main_loop` to `try_send`. |
| `src/pane.rs` | 38–68 | `bracketed_paste` field stays as cache; comment updated. |
| `src/pane.rs` | 232–248 | Replace `track_dec_modes(&data, …)` with `screen().bracketed_paste()` cache + `track_focus_events`. |
| `src/pane.rs` | 632–652 | Rename / shrink `track_dec_modes` to `track_focus_events`. |
| `src/tab.rs` | 1, 46–55 | Add `use std::collections::VecDeque;`; swap `Vec<Tab>` → `VecDeque<Tab>`. |
| `src/tab.rs` | 72–85 | Convert `Vec<Tab>` parameter to `VecDeque` via `.into()`. |
| `src/tab.rs` | tests | No semantic test changes; verify all pass. |
| `tests/property_layout.rs` or new `tests/render_perf.rs` | + ~80 LoC | Property + bench-style assertions; see §8. |
| `benches/render_hotpaths.rs` | + ~60 LoC | Add `track_focus_events` and `wake_drain` micro-benches. |

Approximate net: +120 / −40 LoC.

---

## 7. Migration / backwards-compat

- **Wire protocol**: unchanged.
- **Config**: unchanged.
- **Snapshots**: unchanged.
- **Public API**: `Pane::bracketed_paste()` and `Pane::wants_focus()`
  retain identical signatures and semantics. Internal restructuring
  is invisible to callers.
- **TabManager public API**: unchanged. The container swap is purely
  internal; tests cover all public behaviours.
- **Behavioural change**: `wake_main_loop` may *silently drop* wake
  messages under extreme load (queue full of 64 idempotent wakes).
  Verified safe by the receiver's drain-on-tick semantics.

---

## 8. Test plan

### Unit tests — `src/pane.rs`

- `bracketed_paste_set_via_vt100`: feed `\e[?2004h` to a pane's
  reader-channel test fixture; after one `read_output()` tick,
  `pane.bracketed_paste()` returns `true`.
- `bracketed_paste_cleared_via_vt100`: feed `\e[?2004h` then
  `\e[?2004l`; `pane.bracketed_paste()` ends `false`.
- `bracketed_paste_state_matches_screen`: random-bytes fuzz: feed
  varying mode strings, assert `pane.bracketed_paste() ==
  pane.screen().bracketed_paste()` after each tick.
- `focus_events_still_tracked`: feed `\e[?1004h`, assert
  `pane.wants_focus()` true.
- `track_focus_events_no_false_positives`: feed bytes containing
  `\e[?1004` *as a substring of larger sequences*, ensure no spurious
  toggles. (vt100's parser swallows the surrounding context, but our
  scanner runs on raw bytes; tests must guarantee the existing
  behaviour is preserved.)
- `wake_main_loop_overflow_safe`: spawn 1000 wakes back-to-back from
  a thread; main thread does *not* drain. Assert no panic, no growth
  beyond capacity 64. Then drain and assert at least one wake was
  observed (the channel was non-empty).

### Unit tests — `src/tab.rs`

All existing tests (lines 213–429) must pass without modification.
Add:

- `switch_to_with_30_tabs_constant_time`: build a `TabManager` with
  30 tabs, perform 1000 random switches, assert wall time
  < 50 ms (loose bound for CI; real assertion is "no quadratic
  blow-up").
- `vec_deque_invariants`: after a sequence of `create_tab`,
  `switch_to`, `close_active`, assert that the *logical* order
  returned by `tab_names` matches a separately-tracked oracle.

### Integration tests — `tests/render_perf.rs` (new)

- `pty_throughput_does_not_regress`: spawn a daemon, attach a pane
  running `seq 1 1000000`. Sample main-loop tick rate (counter
  exposed via debug accessor). Compare against a baseline from
  before this SPEC; assert ≥ 5 % improvement OR no regression.

### Microbenches — `benches/render_hotpaths.rs`

- Add `bench_track_focus_events_vs_track_dec_modes`: feed 1 MB of
  representative PTY output to both functions, compare ns/op.
  Expectation: `track_focus_events` ≥ 1.5× faster.
- Add `bench_wake_drain`: 64 wakes pending, measure time to drain.
  Expectation: < 5 µs.
- Add `bench_tab_switch_n10` and `bench_tab_switch_n30`: assert
  per-switch cost stays < 5 µs at N=30.

### Manual smoke

1. Run `seq 1 10000000` in a pane, scroll back; confirm output is
   responsive and bracketed paste / focus events still function (try
   a `vim` instance — focus loss/gain handling).
2. Open 30 tabs (`for i in $(seq 1 30); do ezpn-ctl new-tab; done`)
   and rapidly cycle with Ctrl+B n / Ctrl+B p; confirm no perceptible
   lag.
3. Run a flood-of-output workload (`yes`); `top -pid $(pgrep -f
   ezpn-server)` baseline before/after the patch; confirm ≥ 5 %
   reduction in CPU.

---

## 9. Acceptance criteria

- [ ] `bracketed_paste` is read from `vt100::Screen::bracketed_paste()`
      after each `process()` call, not from raw-byte scanning.
- [ ] `track_dec_modes` is renamed/shrunk to `track_focus_events`
      and only scans for `?1004h`/`l`.
- [ ] `wake_rx` uses `mpsc::sync_channel(64)`; `wake_main_loop` uses
      `try_send`.
- [ ] `TabManager::tabs` is `VecDeque<Tab>`; all public API
      preserved.
- [ ] All existing unit tests pass without modification.
- [ ] New tests in §8 pass: bracketed-paste round-trip, focus events
      preserved, wake overflow safe, 30-tab switch fast.
- [ ] Microbench `bench_track_focus_events_vs_track_dec_modes`
      shows ≥ 1.5× speedup.
- [ ] Microbench `bench_tab_switch_n30` shows ≤ 5 µs per switch.
- [ ] No new clippy warnings.
- [ ] Manual smoke: ≥ 5 % CPU reduction under `yes` workload (4
      panes).
- [ ] PROTOCOL_VERSION unchanged.

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| `vt100::Screen::bracketed_paste()` semantics differ subtly from our raw-byte scan (e.g., vt100 may apply `?2004h` only at the *end* of a chunk if it's split mid-sequence). | Fuzz test (`bracketed_paste_state_matches_screen`) drives this directly. If a divergence is found, fall back to keeping the raw scan for `?2004` *only* — but the simpler shape is preferred. |
| Dropped wake messages under saturation cause a missed redraw in a corner case. | The receiver's `recv_timeout` has its own ticker (8 ms idle), so even a fully dropped wake batch leads to at most 8 ms of UI lag — not a missed frame in steady state. |
| `VecDeque` swap surfaces an unexpected ordering bug because internal storage is a ring buffer (not contiguous). | All public API (`get`, `iter`, `insert`, `remove`) preserves logical ordering. Existing tests cover the visible behaviour exhaustively (16 tests in `src/tab.rs`). |
| Microbench results are noisy on shared CI runners. | Use criterion's built-in noise filtering; assert relative speedup, not absolute ns/op. |
| The `bracketed_paste` cache field becomes inconsistent if `process()` is called from somewhere we don't update the cache. | Only one call site in `read_output`; add a `debug_assert!` that the cache matches `screen().bracketed_paste()` in test builds. |
| `track_focus_events` window of length 8 is the shorter sequence; previous `track_dec_modes` used 8 (max of 8 and 8). No actual change. | Sanity check: both `?1004h` and `?2004h` are 8 bytes including the prefix `\e[`. Confirmed by re-reading the constants in `src/pane.rs:636-639`. |
| Tab cursor model gap-fill semantics rely on `tabs.len() == count - 1` invariant. `VecDeque` preserves this (just swap underlying type). | Add `debug_assert!(self.tabs.len() == self.count.saturating_sub(1))` at every public mutation entry point. |

---

## 11. Open questions

1. Should we **remove** the `bracketed_paste` field entirely and
   compute on demand? **Default proposal:** keep cache for one-load
   per-keystroke read in the encoder; the field is one bool, the
   read site is hot.
2. Should we ship a feature flag `--legacy-bracketed-scan` for one
   release in case the vt100 path differs in the wild? **Default
   proposal:** no — the fuzz test gives sufficient confidence; if a
   real regression surfaces post-release, ship a 0.10.1 patch.
3. Should `WAKE_CHANNEL_CAPACITY` be `64` or `8`? **Default
   proposal:** 64. It's a `Vec<()>` under the hood (64 bytes); cost
   is negligible and we want headroom for transient bursts.
4. Should `TabManager` go straight to the v0.11 cursor-only model in
   v0.10? **Default proposal:** no. The container swap is risk-free
   and lands in one PR; a full data-model change deserves its own
   SPEC + bench cycle.
