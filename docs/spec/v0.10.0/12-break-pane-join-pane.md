# SPEC 12 — `break-pane` / `join-pane` / `move-pane`

**Status:** Draft
**Related issue:** TBD
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** D. Feature parity with tmux

---

## 1. Background

tmux has shipped pane-mobility commands since 1.6:

- `break-pane` — detach the current pane from its window and turn it into a new
  window of its own (`prefix !`).
- `join-pane -s SRC -t DST [-h|-v]` — move a pane from another window into the
  target window as a new split.
- `move-pane` — alias for `join-pane` with explicit src + dst panes.

ezpn v0.9.0 has no equivalent. The layout tree (`src/layout.rs:67-274`) supports
`split` (insert a sibling next to a leaf) and `remove` (delete a leaf and
collapse the parent), but there is no API to *extract* a leaf — meaning a pane's
PTY, vt100 state, and child process cannot move between tabs without being
killed and respawned. `TabManager` (`src/tab.rs:46-211`) likewise only knows how
to create empty tabs, switch, and close.

The gap is concretely visible in:

- `src/layout.rs:188` — `Layout::remove(target_id) -> bool` deletes the leaf
  but does not return the id, and there is no parallel "extract" that hands
  the leaf back to the caller.
- `src/tab.rs:111-123` — `TabManager::create_tab` always builds an empty tab
  via the caller; there is no path to seed a tab with an existing
  `(Layout, Pane)` pair.
- `src/daemon/dispatch.rs:147-258` — the command palette has no `break-pane`,
  `join-pane`, or `move-pane` entries.
- `src/ipc.rs:17-43` — `IpcRequest` does not encode pane mobility.

For ezpn to claim "tmux feature parity floor" (PRD §5 line 88), these three
operations are the blocker.

---

## 2. Goal

Implement three IPC + CLI + keybinding operations:

1. **`break-pane`** — detach the active pane from its tab's layout tree, create
   a new tab containing only that pane, and switch to it. The pane's PTY,
   vt100 parser, scrollback, child process, OSC 52 buffer, and id are all
   preserved by *value* — nothing is killed or respawned.
2. **`join-pane`** — move a pane from another (inactive) tab into the *current*
   tab as a new split next to the active pane.
3. **`move-pane`** — generalisation of `join-pane`: move a pane from any tab
   into any other tab next to a specified target pane.

The acceptance bar is "the moved pane keeps its shell history, exit status of
the last command, and any running process — no-one notices the move except the
layout tree."

---

## 3. Non-goals

- **Cross-session moves.** Moving a pane from session `foo` into session `bar`
  requires the daemon to forward PTY ownership across `Pane` instances that
  belong to different `Workspace` roots. Deferred to v0.11. v0.10 supports
  cross-tab only — both src and dst tabs must live in the same daemon process.
- **Promote a split to a tab.** tmux's `break-pane -a` (move pane to a new
  window *and* keep the rest of the source window) is what we ship. We do
  *not* ship `break-pane -t` for promoting an entire `Split` subtree; that's
  effectively "split window" and gets its own SPEC if asked for.
- **Renumbering pane ids.** Pane ids are stable for the lifetime of the daemon
  (`src/layout.rs:64` `next_id`). Moving a pane preserves its id; tmux's
  `pane_id` semantics translate 1:1.
- **Undo.** `break-pane` followed by manual `join-pane` is the user-facing
  undo. No history stack.

---

## 4. Design

### 4.1 New layout-tree mutators

Extend `Layout` in `src/layout.rs` with two operations. Both are pure tree
surgery — they touch no PTYs.

```rust
impl Layout {
    /// Remove the leaf identified by `target_id` from the tree and return
    /// `Some(target_id)` on success. The parent split collapses: the sibling
    /// subtree replaces the parent slot. Returns `None` if `target_id` is
    /// unknown OR if it's the only leaf in the tree (caller should detect
    /// this via `pane_count() == 1` and turn it into a tab close instead).
    pub fn detach(&mut self, target_id: usize) -> Option<usize>;

    /// Insert `new_id` next to `target_id` as a new split.
    /// `direction` chooses Horizontal or Vertical.
    /// `place_after = true` puts the new leaf in the `second` slot (right /
    /// below); `false` puts it in the `first` slot (left / above).
    /// `ratio` is clamped to [0.1, 0.9]; pass 0.5 for the tmux default.
    /// Returns `false` if `target_id` is not in the tree.
    pub fn insert_pane(
        &mut self,
        new_id: usize,
        target_id: usize,
        direction: Direction,
        place_after: bool,
        ratio: f32,
    ) -> bool;
}
```

`detach` is `remove_node` (`src/layout.rs:635`) almost verbatim, but we want
the leaf id back. Implementation:

```rust
pub fn detach(&mut self, target_id: usize) -> Option<usize> {
    if self.pane_count() <= 1 { return None; }
    let old = std::mem::take(&mut self.root);
    let (new_root, found) = detach_node(old, target_id);
    self.root = new_root;
    if found { Some(target_id) } else { None }
}

fn detach_node(node: LayoutNode, target: usize) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Leaf { id } if id == target =>
            // Caller is responsible for collapsing — propagate "found" up.
            (LayoutNode::Leaf { id }, true),
        LayoutNode::Split { direction, ratio, first, second } => {
            if contains(&first, target) {
                // If `first` is exactly the target leaf → return `second`.
                if let LayoutNode::Leaf { id } = first.as_ref() {
                    if *id == target { return (*second, true); }
                }
                let (new_first, found) = detach_node(*first, target);
                (LayoutNode::Split { direction, ratio,
                                     first: Box::new(new_first), second }, found)
            } else if contains(&second, target) {
                if let LayoutNode::Leaf { id } = second.as_ref() {
                    if *id == target { return (*first, true); }
                }
                let (new_second, found) = detach_node(*second, target);
                (LayoutNode::Split { direction, ratio, first,
                                     second: Box::new(new_second) }, found)
            } else {
                (LayoutNode::Split { direction, ratio, first, second }, false)
            }
        }
        other => (other, false),
    }
}
```

`insert_pane` reuses `split_node` (`src/layout.rs:612`) — which already does
the "find leaf, replace with a Split node" walk — but lets the caller pick the
direction, ratio, and which slot the new leaf occupies (vs the existing
`split` which always equalises afterward and always puts the new id in
`second`).

### 4.2 Layout-tree surgery diagrams

Three cases must be handled correctly. (`*` = the leaf being detached.)

**Case A — sibling is a leaf:** parent collapses to that leaf.

```
Before                           After detach(*)
                  Split(H, 0.5)
                  /          \
        Leaf(*1)            Leaf(2)            Leaf(2)
```

**Case B — sibling is a Split subtree:** parent slot is replaced by the
*entire* sibling subtree.

```
Before                                   After detach(*)
                  Split(H, 0.5)
                  /          \
        Leaf(*1)              Split(V, 0.4)
                              /          \
                           Leaf(2)       Leaf(3)         Split(V, 0.4)
                                                          /          \
                                                       Leaf(2)       Leaf(3)
```

**Case C — only leaf in the tree:** `detach` returns `None`; the IPC layer
turns this into "close the source tab too" (or refuses, if you'd lose the
session). Default: refuse, return error `"cannot break the only pane in tab"`.

**Insert path:** `insert_pane(new=4, target=2, dir=V, place_after=true,
ratio=0.5)` on the post-Case-A tree:

```
Before                After insert_pane(4, target=2, V, after=true, 0.5)
                                         Split(V, 0.5)
                                         /          \
        Leaf(2)                       Leaf(2)       Leaf(4)
```

`insert_pane` also handles the case where `target_id == self.root` *and* root
is a leaf — that's just `self.root = Split { Leaf(target), Leaf(new), ... }`.

### 4.3 Pane identity preservation

`Pane` (`src/pane.rs:38-68`) holds `master`, `writer`, `child`, `reader_rx`,
`parser`, etc. — all owned by value. Moving the pane between tabs is just a
`HashMap::remove` + `HashMap::insert`:

```rust
let pane = src_tab.panes.remove(&pid).unwrap();
dst_tab.panes.insert(pid, pane);
```

The pane id, vt100 parser (and its scrollback ringbuffer), `osc52_pending`
queue, restart policy, `bracketed_paste` / `focus_events` flags, and child
process all travel by value. The PTY's slave is already dropped after spawn
(`src/pane.rs:156`), so there is no implicit binding to the source tab.

There are three pieces of "tab-attached" state to also migrate:

1. `restart_policies: HashMap<usize, RestartPolicy>` — move the entry.
2. `restart_state: HashMap<usize, (Instant, u32)>` — move the entry.
3. `zoomed_pane: Option<usize>` — clear in src tab if zoomed pane is the one
   we're moving; never set in dst.

After the move, both tabs need `lifecycle::resize_all(panes, layout, tw, th,
settings)` so the pane resizes to its new rect. The PTY resize ioctl is
idempotent and cheap.

### 4.4 Active-pane handling

After **`break-pane`**:
- Source tab: active becomes the leaf id that *replaced* the parent slot
  (in Case A, the sibling; in Case B, the leftmost leaf of the sibling
  subtree found by `pane_ids().first()`).
- Destination (new) tab: active is the moved pane (only pane present).
- `last_active` in the source tab is reset to the new active.

After **`join-pane`** / **`move-pane`**:
- Source tab: active follows the same rule as `break-pane`'s source side.
- Destination tab: active becomes the moved pane.
- `last_active` in dst is the previous active (so `prefix ;` toggles back).

If the source tab becomes empty (impossible today since we refuse to detach
the last leaf, but defensive against a future "promote-subtree" variant),
close the source tab via `TabManager::close_active`.

### 4.5 Cross-tab orchestration

The unpacked-active-tab pattern (`src/tab.rs:46-86`) means at most one tab is
"hot" at a time. For `join-pane --src OTHER`, the source tab lives in
`tab_manager.tabs: Vec<Tab>`. We need a mutator on `TabManager`:

```rust
impl TabManager {
    /// Pop a pane out of an inactive tab. Returns `(pane, restart_policy,
    /// restart_state, became_empty)`. If `became_empty`, the caller should
    /// close that tab.
    pub fn extract_pane_from_inactive(
        &mut self,
        logical_idx: usize,
        pane_id: usize,
    ) -> Option<ExtractedPane>;
}

pub struct ExtractedPane {
    pub pane: Pane,
    pub policy: Option<RestartPolicy>,
    pub state: Option<(Instant, u32)>,
    pub source_became_empty: bool,
}
```

For `break-pane`, the active tab (which is unpacked) is the source — caller
extracts the pane directly from local `panes: HashMap<usize, Pane>` and feeds
the new tab to `TabManager::create_tab` *with state*. We need a small tweak to
`create_tab` — accept an optional seed tuple instead of leaving the new tab
empty for the caller to fill:

```rust
pub fn create_tab_with_seed(&mut self, current: Tab, seed: Tab) -> String;
```

(`create_tab` becomes `create_tab_with_seed(current, Tab::new(...empty...))`
internally.)

### 4.6 Failure modes

| Condition                                     | Behaviour                                          |
|-----------------------------------------------|----------------------------------------------------|
| `break-pane` on a tab with only 1 pane        | Reject with error `"cannot break only pane"`       |
| `join-pane` from src that doesn't exist       | Reject with `"no such tab/pane"`                   |
| `join-pane` src == dst tab                    | Reject with `"src and dst tab must differ"`        |
| `join-pane` dst rect too small to split       | Reject with `"target pane too small"` (use `can_split`) |
| `move-pane` src pane is the source tab's only | Move succeeds, source tab is closed                |
| Pane id collision after move                  | Cannot happen — `next_id` is monotonic; ids are unique across all tabs |

---

## 5. Surface changes

### 5.1 IPC / wire protocol

Add three variants to `IpcRequest` in `src/ipc.rs`:

```rust
pub enum IpcRequest {
    // ...existing variants...
    BreakPane {
        /// Pane to detach. None = active pane.
        pane: Option<usize>,
        /// Optional name for the new tab. Defaults to next auto-id.
        new_tab_name: Option<String>,
    },
    JoinPane {
        /// Source tab logical index.
        src_tab: usize,
        /// Source pane id within that tab.
        src_pane: usize,
        /// Destination tab logical index. None = current/active tab.
        dst_tab: Option<usize>,
        /// Target pane id in dst to split next to. None = active pane of dst.
        dst_pane: Option<usize>,
        direction: SplitDirection,
        /// 0.1..=0.9, clamped. None = 0.5.
        ratio: Option<f32>,
        /// If true, place new pane after target (right / below).
        /// If false, place before (left / above). Default: true.
        place_after: Option<bool>,
    },
    MovePane {
        src_tab: usize,
        src_pane: usize,
        dst_tab: usize,
        dst_pane: usize,
        direction: SplitDirection,
        ratio: Option<f32>,
        place_after: Option<bool>,
    },
}
```

`MovePane` is sugar over `JoinPane` with both src and dst fully specified;
keeping it separate matches tmux's command surface and avoids ambiguity in CLI
parsing.

No protocol-version bump is required — `IpcRequest` is JSON over a Unix socket
with `serde(tag = "cmd")`, so unknown variants on an older daemon are rejected
by serde with a clear error. The `PROTOCOL_VERSION` constant in
`src/protocol.rs:47` governs the binary client↔server stream and is unchanged.

### 5.2 CLI

Add to `src/bin/ezpn-ctl.rs`:

```
ezpn-ctl break-pane [--pane N] [--name NAME]
    Detach pane N (default: active) into a new tab.
    --name sets the new tab's name (default: auto-numbered).

ezpn-ctl join-pane --src TAB:PANE [--dst TAB[:PANE]]
                   [--horizontal | --vertical] [--ratio R] [--before]
    Move pane TAB:PANE into --dst (default: current tab) as a new split.
    --horizontal (default) splits side-by-side; --vertical stacks.
    --ratio is 0.1..=0.9 (default 0.5).
    --before places the new pane before the target (default: after).

ezpn-ctl move-pane --src TAB:PANE --dst TAB:PANE
                   [--horizontal | --vertical] [--ratio R] [--before]
    Like join-pane but with both src and dst fully specified.
```

`TAB:PANE` syntax: tab is a 0-based logical index, pane is the numeric pane
id printed by `ezpn-ctl ls`. Tab name is *not* accepted in v0.10 (defer to
v0.11 once names are unique-enforced).

### 5.3 Config (TOML)

```toml
[panes]
# Default ratio for new splits created by break/join/move-pane.
# Range: 0.1..=0.9. Clamped silently.
default_split_ratio = 0.5
```

Optional. Existing `~/.config/ezpn/config.toml` files keep working; this key
is read with a 0.5 fallback.

### 5.4 Keybindings (default)

These align with tmux. Custom keymap (SPEC 09) can override.

| Mode   | Binding         | Action                                        |
|--------|-----------------|-----------------------------------------------|
| Prefix | `!`             | `break-pane` on active pane                   |
| Prefix | `m`             | Mark active pane (`pane_id` stored daemon-side)|
| Prefix | `M`             | Clear marked pane                             |
| Prefix | `J`             | `join-pane --src MARKED --dst CURRENT -h`     |
| Prefix | `Shift+J`       | (same as `J`; alias)                          |

The "marked pane" state lives on `Workspace` (one slot, `Option<(tab_idx,
pane_id)>`). `prefix m` overwrites; `prefix M` clears; `prefix J` consumes
(does not auto-clear, matching tmux). Status bar shows `[marked]` next to the
pane title when marked.

---

## 6. Touchpoints

| File | Lines | Change |
|---|---|---|
| `src/layout.rs` | 67-274 | Add `Layout::detach`, `Layout::insert_pane`. |
| `src/layout.rs` | 635-684 | Refactor `remove_node` → share core walk with `detach_node`. |
| `src/layout.rs` | 786-1052 | Add unit tests for detach (Cases A/B/C) + insert_pane. |
| `src/tab.rs` | 9-38 | Add `Tab::from_existing(name, layout, panes, active, policies, state)` constructor. |
| `src/tab.rs` | 109-149 | Add `create_tab_with_seed`, `extract_pane_from_inactive`. |
| `src/tab.rs` | 213-429 | Add tests for `extract_pane_from_inactive`. |
| `src/ipc.rs` | 17-43 | Add `BreakPane`, `JoinPane`, `MovePane` variants. |
| `src/protocol.rs` | — | No changes (binary protocol untouched). |
| `src/daemon/dispatch.rs` | 147-258 | Add `break-pane`, `join-pane`, `move-pane`, `mark-pane` palette commands. |
| `src/daemon/keys.rs` | 297-510 | Add prefix `!`, `m`, `M`, `J` bindings. |
| `src/daemon/server.rs` (or wherever IPC dispatch lives) | — | Wire the new IPC variants to layout/tab mutators. |
| `src/app/state.rs` | — | Add `marked_pane: Option<(usize, usize)>` field on workspace state. |
| `src/bin/ezpn-ctl.rs` | — | Add `break-pane`, `join-pane`, `move-pane` subcommands + `TAB:PANE` parser. |
| `src/config.rs` | — | Read `[panes].default_split_ratio`. |
| `tests/break_join_pane.rs` | new | Integration test (see §8). |

---

## 7. Migration / backwards-compat

- **No new direct dependencies.** Everything reuses existing crates.
- **No `Cargo.toml` changes.**
- **JSON IPC is forward-compatible by serde-tag.** Older clients that don't
  send the new variants are unaffected. Newer clients hitting older daemons
  get `invalid request: unknown variant 'break_pane'` — clear enough.
- **No protocol version bump.** `PROTOCOL_VERSION` (`src/protocol.rs:47`)
  governs the attach-stream binary protocol, which has no break/join messages.
- **Snapshot format** (`src/snapshot.rs` if present): if snapshots store layout
  trees, format is unchanged — the tree shape is the same; only how we *get*
  to a given tree shape is new. No migration needed.
- **Existing keybindings** `prefix m`, `prefix M`, `prefix J`, `prefix !` are
  currently unbound (verified against `src/daemon/keys.rs:297-510`); no
  collisions with shipped bindings. SPEC 09 (custom keymap) inherits these as
  defaults.

---

## 8. Test plan

### Unit tests (`src/layout.rs::tests`)

- **`detach_collapses_to_leaf_sibling`** — Case A: split with two leaves,
  detach one, root becomes the other leaf.
- **`detach_collapses_to_split_sibling`** — Case B: split where left is leaf,
  right is a 2-leaf split. Detach the left; root becomes the right subtree
  intact (ratios + leaf ids preserved).
- **`detach_only_leaf_returns_none`** — Case C: 1-leaf tree, `detach(0)`
  returns `None`, tree unchanged.
- **`detach_unknown_id_returns_none`** — `detach(999)` on a 4-leaf tree
  returns `None`.
- **`insert_pane_into_leaf_root`** — root is a single leaf;
  `insert_pane(new=1, target=0, H, after=true, 0.5)` makes
  `Split(H, 0.5, Leaf(0), Leaf(1))`.
- **`insert_pane_before_target`** — `place_after=false` puts new id in
  `first` slot.
- **`insert_pane_unknown_target_returns_false`** — leaves tree untouched.
- **Property test (proptest)** — `detach_then_layout_invariants_hold`:
  generate a random tree of depth ≤ 5 with 1–16 leaves, pick a random leaf,
  detach it, assert: (a) `pane_count()` decreases by 1, (b) all surviving ids
  are still present in `pane_ids()`, (c) every leaf has a positive rect in an
  80×24 area, (d) round-tripping detach + insert restores `pane_count()`.

### Tab manager tests (`src/tab.rs::tests`)

- **`extract_pane_from_inactive_basic`** — 2-tab manager, extract a pane from
  the inactive tab, assert it's no longer in that tab's panes map and the
  layout no longer references it.
- **`extract_makes_source_empty_signals_caller`** — extracting the only pane
  from a 1-pane inactive tab sets `source_became_empty = true`.
- **`create_tab_with_seed_assigns_next_name`** — seeded tab gets the same
  auto-name treatment as the empty form.

### Integration test (`tests/break_join_pane.rs`)

```
1. Spawn a daemon harness with 2 tabs, 2 panes each (ids: tab0 → [0,1], tab1 → [2,3]).
2. Write a marker line to pane 1 ("BREAK_TEST_42\n") and let it land in vt100.
3. Send IPC: BreakPane { pane: Some(1), new_tab_name: Some("broken".into()) }.
4. Assert: tab count == 3; new tab "broken" contains exactly pane 1; tab0
   contains exactly pane 0; vt100 of pane 1 still contains "BREAK_TEST_42".
5. Send IPC: JoinPane { src_tab: 2 /*"broken"*/, src_pane: 1, dst_tab: Some(0),
   dst_pane: Some(0), direction: Horizontal, ratio: Some(0.5), place_after: Some(true) }.
6. Assert: tab count == 2 (broken closed because empty); tab0 layout is
   Split(H, 0.5, Leaf(0), Leaf(1)); pane 1 vt100 STILL contains
   "BREAK_TEST_42" (no respawn, no clear).
7. Verify pane 1's child PID is unchanged across the move (record before
   step 3, compare after step 6).
```

### Manual / smoke

- Run `ezpn` interactively, spawn 2 tabs × 2 panes, hit `prefix !`, confirm
  the active pane jumps to a new tab, source tab still works.
- `prefix m` on a pane in tab 1, switch to tab 0, `prefix J` — pane lands as
  a horizontal split; status bar `[marked]` indicator clears? (Defaulting to
  *not* auto-clear matches tmux; revisit if confusing.)

---

## 9. Acceptance criteria

- [ ] `Layout::detach`, `Layout::insert_pane` implemented with all unit tests
      passing.
- [ ] Property test (`detach_then_layout_invariants_hold`) runs ≥ 256 cases,
      all green.
- [ ] `IpcRequest::{BreakPane, JoinPane, MovePane}` round-trip through JSON
      with no field loss.
- [ ] `ezpn-ctl break-pane` end-to-end: pane id, vt100 contents, child PID
      survive the move (integration test asserts).
- [ ] `ezpn-ctl join-pane --src 1:3 --dst 0` works in a 2-tab session.
- [ ] `prefix !` / `prefix m` / `prefix J` keybindings dispatch the right
      IPC actions.
- [ ] Source-tab "only pane" guard rejects with a clear error message;
      daemon does not panic.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] Manual smoke: PRD §6 "tmux Getting Started" walkthrough completes the
      `break-pane` step on ezpn.

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| `detach` + `insert_pane` get the binary tree restructure wrong on edge cases (unbalanced trees, deeply nested splits) | Property test with proptest covers random trees up to depth 5; explicit unit tests for Cases A/B/C. |
| Moving a pane mid-output races with the PTY reader thread on the source tab's main loop | `Pane`'s `reader_rx` is owned by `Pane`; moving the struct moves the channel handle. The reader thread keeps writing to the same `Sender`, which now drains in the destination tab's render iteration. No locks needed. |
| `restart_policies` / `restart_state` left dangling on source tab | Explicit migration step in IPC handler; covered by integration test (assert source tab's policies map no longer contains moved pane id). |
| `zoomed_pane` left pointing at a moved pane's id | IPC handler clears `zoomed_pane` on src if it equals the moved id; never sets on dst. |
| Pane id reuse across the move corrupts state | `next_id` is monotonic and global to `Layout`; ids are never reused. Verified by existing tests and unchanged here. |
| Status bar / border cache stale after move | Set `update.full_redraw = true` on both tabs after the operation completes. |

---

## 11. Open questions

1. **Marked pane scope.** tmux's marked pane is global (single slot per
   server). Should ezpn match (one slot per daemon) or scope per session?
   *Default proposal:* per daemon, matching tmux. Revisit if confusing in
   multi-session workflows.
2. **`break-pane -d`** (don't switch to new tab). tmux supports it; we'd
   add `--no-switch` flag. *Default proposal:* defer to v0.11 unless a user
   asks before merge.
3. **Auto-clear marked pane after `J`?** tmux does not. *Default proposal:*
   don't auto-clear. Status-bar indicator stays until `prefix M` or
   the marked pane is closed.
4. **`join-pane` into a zoomed tab.** Should we silently un-zoom, or refuse?
   *Default proposal:* un-zoom, then insert. Match tmux behaviour where
   layout-mutating commands implicitly un-zoom.
