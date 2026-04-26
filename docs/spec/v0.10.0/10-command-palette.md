# SPEC 10 — Fuzzy Command Palette

**Status:** Draft
**Related issue:** TBD
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** C. UX & Discoverability

## 1. Background

ezpn already has a `:`-mode "command palette" — but it is a linear,
no-completion text-input prompt. The user types `split-window -v`
verbatim, hits Enter, and the parser at
`src/daemon/dispatch.rs:147-258` matches the first whitespace-split
token against a hand-rolled match arm. There is:

- no auto-completion
- no fuzzy matching
- no listing of valid commands (the only way to discover them is to
  read the source)
- no preview of what each command does
- no way to fuzzy-search across sessions / panes / tabs
- no command history

The PRD explicitly names "command palette (`prefix + p` default) reachable
in ≤ 1 keystroke from any mode" as a UX release-gate metric (§6).
This is the single biggest discoverability lever ezpn has against
tmux's "memorise 40 prefix combos" status quo. The
[Zellij feature page](https://zellij.dev/features/) — which the PRD
cites as the modern competitor benchmark — leads with the command
palette.

This SPEC replaces today's linear `:`-mode UX with a VS Code Cmd+P /
Telescope-style fuzzy palette while keeping the legacy `:` typing
behaviour available for muscle memory.

## 2. Goal

A floating, fuzzy command palette that:

1. Opens on `prefix + p` (configurable via SPEC 09's `[keymap.prefix]`).
2. Indexes four sources (commands, sessions, panes, tabs) plus a
   recent-commands LRU.
3. Renders as a bordered overlay on top of the existing pane
   composition; ≤ 5 ms to first paint after open.
4. Type-to-filter, Up/Down or Ctrl-N/Ctrl-P to move, Enter to execute,
   Tab to switch source tab, Esc to close.
5. Selecting a `command` runs it via the same `execute_action` entry
   point SPEC 09 standardises; selecting a `session` calls
   `attach-session`; selecting a `pane` sets active pane; selecting
   a `tab` switches to it.
6. Maintains a 50-entry LRU of recent commands persisted across
   restarts under `$XDG_STATE_HOME/ezpn/history.json`.

## 3. Non-goals

- **Not** a generic search bar: source set is closed (commands,
  sessions, panes, tabs, recent). Plugins / arbitrary providers
  deferred to v0.11+.
- **Not** a full file picker — no filesystem walking.
- **Not** a help replacement. The help overlay (`prefix + ?`) remains
  the static reference; the palette is action-oriented.
- **Not** RPC-callable from outside the daemon. Scripted execution
  goes through `ezpn-ctl` / SPEC 06 / SPEC 07, not the palette.
- **Not** a custom matcher. We use `nucleo-matcher` (PRD §10).
- **Not** a fuzzy-search across **scrollback contents** — that is
  copy-mode's job (SPEC 13).

## 4. Design

### 4.1 ASCII mockup of the rendered overlay

```
┌─ ezpn-default ──────────────────────────────────────────────────────────┐
│ pane 1                          │ pane 2                                │
│                                 │                                       │
│   ╭──────────────────────────────────────────────────────────────╮      │
│   │ ❯ split                                                       │     │
│   ├───────────────────────────────────────────────────────────────┤     │
│   │ ▌ commands  sessions  panes  tabs  recent                  ⌥ │     │
│   ├───────────────────────────────────────────────────────────────┤     │
│   │ ▶ ⌘ split-window           Split active pane horizontally     │     │
│   │   ⌘ split-window -v        Split active pane vertically       │     │
│   │   ⌘ select-layout 7:3/5:5  Apply a layout DSL spec            │     │
│   │   ⌘ kill-pane              Close active pane                  │     │
│   │   ⌘ swap-pane next         Swap with next pane                │     │
│   │                                                               │     │
│   │   5 of 38 results                                  Esc close  │     │
│   ╰───────────────────────────────────────────────────────────────╯     │
│                                                                         │
│ pane 3                                                                  │
└─────────────────────────────────────────────────────────────────────────┘
```

After typing `dev` while on the **sessions** tab:

```
   ╭──────────────────────────────────────────────────────────────╮
   │ ❯ dev                                                         │
   ├───────────────────────────────────────────────────────────────┤
   │   commands  ▌ sessions  panes  tabs  recent               ⌥  │
   ├───────────────────────────────────────────────────────────────┤
   │ ▶ 🖥 dev-frontend       (3 panes, attached)                   │
   │   🖥 dev-backend        (2 panes)                             │
   │   🖥 deploy             (1 pane, last seen 14:02)             │
   │                                                               │
   │   3 of 7 results                              Tab next source │
   ╰───────────────────────────────────────────────────────────────╯
```

Width: `min(80, term_w - 4)`. Height: 3 (header) + 10 (results) + 2
(borders + footer) = 15 rows when there are >= 10 results, shrinks
otherwise. Centred horizontally; vertically positioned at the top
third of the screen so the active pane below is still partially
visible.

### 4.2 State machine and data model

```rust
// src/daemon/palette/mod.rs (new)

pub struct PaletteState {
    pub query: String,
    pub source: Source,        // active tab
    pub cursor: usize,         // selected index in `results`
    pub results: Vec<Hit>,     // top-N after the latest filter pass
    pub indices: Indices,      // pre-built per source on open
    pub matcher: nucleo_matcher::Matcher,
}

pub enum Source { Commands, Sessions, Panes, Tabs, Recent }

pub struct Indices {
    pub commands: Vec<CommandEntry>,    // built once from action registry
    pub sessions: Vec<SessionEntry>,    // re-built on open
    pub panes:    Vec<PaneEntry>,       // re-built on open
    pub tabs:     Vec<TabEntry>,        // re-built on open
    pub recent:   VecDeque<RecentEntry>,// loaded from disk, capped to 50
}

pub struct Hit {
    pub source: Source,
    pub idx: usize,            // index into the per-source Vec
    pub score: u32,            // nucleo-matcher score
    pub matches: SmallVec<[usize; 8]>, // char positions to highlight
}
```

`InputMode` (in `src/daemon/state.rs:19-39`) gets a new variant:

```rust
pub enum InputMode {
    // … existing variants …
    CommandPalette { buffer: String },          // existing — kept for ":"
    FuzzyPalette(Box<PaletteState>),            // new
}
```

### 4.3 Matcher choice and dependency posture

PRD §7 already names `nucleo-matcher`:

> **Risk:** Command palette + fuzzy matcher adds dependency surface.
> **Mitigation:** Use `nucleo-matcher` (already in zellij ecosystem,
> audited) — add to `deny.toml` allowlist.

Crate profile:

| Crate            | Pulls in                  | Justification                          |
|------------------|---------------------------|----------------------------------------|
| `nucleo-matcher` | none beyond stdlib + `memchr` (which is already transitively present) | Audited (used by Helix, Zed, Yazi). Single-thread API; no async surface. |

`Cargo.toml` add:

```toml
nucleo-matcher = "0.3"
```

`deny.toml` allowlist entry mirrors what was added for `toml`:

```toml
[bans]
multiple-versions = "warn"

[advisories]
ignore = []                      # nucleo-matcher itself; revisit if RUSTSEC posts an advisory

# explicit allow comment so future audits don't relitigate
# nucleo-matcher: SPEC 10 fuzzy palette; audited (used by Helix/Zed/Yazi)
```

The matcher constructs once per palette open and is dropped on close.
`Matcher` is small (~64 bytes plus a scratch buffer) so re-allocation
on every open is cheaper than carrying it in daemon state.

### 4.4 Indexing: when, what, how much

> **Performance:** indexing happens on palette open, not every keystroke;
> matcher runs incrementally. (PRD-derived requirement.)

Per source:

| Source     | Source of truth                                                 | Build cost                            | Refresh                  |
|------------|-----------------------------------------------------------------|---------------------------------------|--------------------------|
| `commands` | static action registry from SPEC 09                             | O(N≈40), built once at daemon start   | only on `reload-config`  |
| `sessions` | `session_dir()` walked for socket files                         | O(S≤20), I/O syscalls                 | on each palette open     |
| `panes`    | `panes: HashMap<usize, Pane>` snapshot                          | O(P≤24)                               | on each palette open     |
| `tabs`     | `tab_names: Vec<(usize, String, bool)>`                         | O(T≤16)                               | on each palette open     |
| `recent`   | `~/.local/state/ezpn/history.json` LRU, capped at 50            | one read at boot                      | append on each execution |

Total index size at the realistic upper bound: ~150 entries. Filtering
all of them per keystroke through `nucleo-matcher` is sub-millisecond
in practice (Helix indexes ~30k files this way and stays interactive).

The matcher is **incremental in the sense that it scores per-entry on
demand**; we re-filter the full source on each query change but cap
visible results at 10 and rank by score. No need for a worker thread
at this scale.

### 4.5 Source switching (Tab key)

The tab strip across the top renders the five sources:

```
▌ commands  sessions  panes  tabs  recent                  Tab→
```

Tab cycles forward (commands → sessions → panes → tabs → recent →
commands…). Shift+Tab cycles back. The query string carries across
sources so a user typing `dev` and pressing Tab through sources can
quickly find a `dev-*` session, a pane named `dev`, or a tab named
`dev` in turn.

### 4.6 Action execution (single ingress)

Selecting a result calls a single dispatch shim:

```rust
fn execute_palette(hit: &Hit, idx: &Indices, ctx: &mut Ctx) {
    match hit.source {
        Source::Commands => {
            let entry = &idx.commands[hit.idx];
            execute_action(&entry.action, ctx);   // same fn SPEC 09 introduces
            push_recent(idx, RecentEntry::Command(entry.action.clone()));
        }
        Source::Sessions => {
            let s = &idx.sessions[hit.idx];
            execute_action(&Action::AttachSession(s.name.clone()), ctx);
        }
        Source::Panes => {
            let p = &idx.panes[hit.idx];
            ctx.set_active(p.id);
        }
        Source::Tabs => {
            let t = &idx.tabs[hit.idx];
            ctx.tab_action = TabAction::GoToTab(t.idx);
        }
        Source::Recent => {
            // re-execute as the original source
            let r = &idx.recent[hit.idx];
            execute_recent(r, ctx);
        }
    }
}
```

The `execute_action` function is the one SPEC 09 lifts out of
`dispatch.rs`. Both `:` palette and fuzzy palette share it. **Do not
re-implement the action vocabulary inside the palette module.**

### 4.7 Keys inside the palette

| Key                   | Action                                               |
|-----------------------|------------------------------------------------------|
| any printable char    | append to `query`, re-filter                         |
| `Backspace`           | drop last char from `query`, re-filter               |
| `Up` / `Ctrl-P`       | `cursor = cursor.saturating_sub(1)`                  |
| `Down` / `Ctrl-N`     | `cursor = (cursor + 1).min(results.len() - 1)`       |
| `PageUp` / `PageDown` | move cursor by 10                                    |
| `Tab`                 | next source                                          |
| `Shift-Tab`           | previous source                                      |
| `Enter`               | `execute_palette(&results[cursor])` then close       |
| `Esc` / `Ctrl-G`      | close (no execute)                                   |
| `Ctrl-U`              | clear `query`                                        |

These are hard-coded **inside** the palette mode (i.e. not subject to
`[keymap.*]` rebinding) because the palette is itself a chord-resolved
sub-modal and re-bindable internal keys would be a usability footgun.

### 4.8 Recent history file

```
$XDG_STATE_HOME/ezpn/history.json  (default ~/.local/state/ezpn/history.json)
```

Format:

```json
{
  "version": 1,
  "entries": [
    { "kind": "command", "value": "split-window", "ts": 1735012345 },
    { "kind": "command", "value": "rename-tab demo", "ts": 1735012380 },
    { "kind": "session", "value": "dev-frontend",   "ts": 1735012400 }
  ]
}
```

- LRU cap: 50 entries (oldest evicted on insert when full).
- Loaded on daemon start; written on each push, debounced 1 s.
- Atomic write via tmp + rename, mirroring `config::save_settings`
  (`src/config.rs:190-214`).
- File missing or malformed → start with empty list, log a warning,
  recreate on next push. Never crash the daemon over this.
- Path uses `$XDG_STATE_HOME` if set, else `$HOME/.local/state`.
  This is **state**, not config or cache, so it deserves its own
  XDG dir per the spec.

### 4.9 Render path

The palette is drawn after `render_panes` and the status bar, before
the cursor-show:

```
render_panes(...)
draw_status_bar_full(...)         // unchanged
draw_palette_overlay(...)         // new — only when InputMode::FuzzyPalette
queue!(stdout, cursor::Hide)      // tail unchanged
```

In `src/daemon/render.rs:170-178` the existing match on `InputMode`
already dispatches to text-input overlays for `RenameTab` and
`CommandPalette`. Add a third arm for `FuzzyPalette` that calls into
`src/render/palette.rs` (new module) which composes the box from
the same `BorderStyle::Rounded` chars used elsewhere
(`src/render.rs:86-98`).

Cost budget: a full palette repaint allocates ≤ 1 KiB of byte-buffer
output (one screen region) and runs in < 1 ms on a warm cache.

### 4.10 Trigger and inter-mode navigation

- Default: `prefix + p` opens the palette (set in
  `assets/keymaps/default.toml`; user can rebind via SPEC 09).
- The legacy `:` palette stays. Inside it, typing the literal string
  `palette` then Enter promotes to fuzzy palette (smooth migration
  for users with `:`-mode muscle memory).
- The palette can be opened from **any** mode that already accepts
  prefix entry. From inside CopyMode the prefix key is intercepted
  by copy-mode's vi machine; that's intentional and unchanged.

## 5. Surface changes

### Config (TOML)

```toml
[keymap.prefix]
"p" = "command-palette"

[ui]
# How many results to show in the palette.  Defaults to 10.
palette_max_results = 10

# Whether to persist the recent-commands history (default: true).
palette_history = true
```

### CLI

No new CLI commands. The palette is a UI affordance only; scripted
callers should use `ezpn-ctl` (SPEC 06 / 07).

### Keybindings (default)

| Chord                | Action                                  |
|----------------------|-----------------------------------------|
| `prefix + p`         | open fuzzy palette (commands tab)       |
| `prefix + :`         | open legacy `:` linear palette          |
| inside palette: `Tab`| cycle source                            |
| inside palette: `↑/↓`| move selection                          |
| inside palette: `Enter`| execute, close                        |
| inside palette: `Esc`| close                                   |

## 6. Touchpoints

| File                              | Lines       | Change |
|-----------------------------------|-------------|--------|
| `src/daemon/state.rs`             | 19-39       | Add `InputMode::FuzzyPalette(Box<PaletteState>)` variant. |
| `src/daemon/keys.rs`              | 153-191     | Add new arm: when `mode == FuzzyPalette`, route key into palette state machine. After SPEC 09, this becomes ~30 lines. |
| `src/daemon/keys.rs`              | 491-496     | `KeyCode::Char(':')` still opens linear palette; new binding in `[keymap.prefix]` opens fuzzy palette (bound to `p`). |
| `src/daemon/dispatch.rs`          | 145-258     | `execute_command` lifted to shared `execute_action` (per SPEC 09); palette calls the same function. |
| `src/daemon/palette/mod.rs`       | new         | `PaletteState`, `Source`, `Hit`, `Indices`, transition logic. |
| `src/daemon/palette/index.rs`     | new         | Per-source builders (`build_commands`, `build_sessions`, etc.). |
| `src/daemon/palette/history.rs`   | new         | LRU + atomic JSON read/write under `$XDG_STATE_HOME/ezpn/`. |
| `src/render/palette.rs`           | new         | `draw_palette_overlay`: bordered box, source tabs, results list, footer. |
| `src/daemon/render.rs`            | 170-178     | Add palette overlay arm to the post-status-bar overlay match. |
| `Cargo.toml`                      | —           | Add `nucleo-matcher = "0.3"`. |
| `deny.toml`                       | —           | Allowlist `nucleo-matcher` with comment pointing at this SPEC. |
| `docs/palette.md`                 | new         | User-facing reference for palette UX, sources, recent history. |

## 7. Migration / backwards-compat

- `:` linear palette **stays**. Users with muscle memory who type
  `:split-window` continue to work. The fuzzy palette is opt-in via
  `prefix + p`.
- The `attach-session` action does not exist in v0.9's `dispatch.rs`
  vocabulary. SPEC 09 adds it; this SPEC depends on it.
- `~/.local/state/ezpn/history.json` is a brand-new path. No legacy
  file to migrate.
- Removing the palette is non-destructive: deleting the daemon state
  file resets recents but loses no user data.

## 8. Test plan

Unit tests:

- `Indices::build_commands()` returns one entry per action string in §4.2 of SPEC 09.
- `nucleo-matcher` integration smoke: query "split" against the command index returns `split-window` and `split-window -v` as the top two with a non-zero score; query "xxxxx" returns empty.
- `history::push` evicts the oldest entry when at cap (50).
- `history::load` on a missing file returns `Default::default()` (no error).
- Atomic write: kill the process between `tmp` write and `rename`; on next start the previous file is intact.

Integration tests:

- Open palette, type `kil`, navigate to `kill-pane`, hit Enter — the active pane is closed.
- Open palette, Tab to **sessions**, type the prefix of a known session name, hit Enter — the daemon emits an `attach-session` action (verified via SPEC 07's event stream).
- Open palette, Tab to **panes**, hit Enter — `active` pane id changes to the selection.
- Open palette, type `xyz` (no matches), result list is empty, footer shows "0 of N results".
- Hot-reload (`prefix r` per SPEC 09) refreshes the command index without restarting the daemon.

Performance gates (PRD §6):

- First paint after `prefix + p`: median < 5 ms across 100 opens (microbench in `cargo bench`).
- Filter latency on a 150-entry index, 5-character query: median < 1 ms.

Manual:

- VS Code-style smoke: rapidly open / type / Esc / open in a 4-pane session for ≥ 60 s, no panics, no corrupt repaints.
- Tab strip wraps correctly when terminal is exactly 60 cols wide (min usable size).

## 9. Acceptance criteria

- [ ] `prefix + p` opens a bordered floating palette overlay in ≤ 1 keystroke from any prefix-reachable mode.
- [ ] Five source tabs (commands, sessions, panes, tabs, recent) each populated and switchable via Tab / Shift-Tab.
- [ ] `nucleo-matcher` is the matcher; added to `Cargo.toml` and to `deny.toml` allowlist.
- [ ] Indexing happens on open, not on each keystroke; first-paint latency < 5 ms (instrumented).
- [ ] Selecting a `command` runs it via the same `execute_action` SPEC 09 introduces — no parallel match in palette code.
- [ ] Recent commands persist to `$XDG_STATE_HOME/ezpn/history.json`, capped at 50, atomic write.
- [ ] Esc / Ctrl-G closes without executing; Enter executes and closes.
- [ ] Legacy `:` palette continues to work for users with v0.9 muscle memory.
- [ ] `docs/palette.md` documents source tabs, keybindings, history file location.

## 10. Risks

| Risk | Mitigation |
|---|---|
| `nucleo-matcher` API breaks at 0.x. | Pin to `0.3.x`; track upstream via Renovate; the matcher API surface we use (`Matcher::new`, `Pattern::parse`, `match_list`) is stable across the 0.3 line. |
| Palette overlay clobbers a pane mid-render under bad terminal redraw. | Use `BeginSynchronizedUpdate` (already wrapped in `daemon/render.rs:99-145`) to flush the palette as part of the same atomic frame. |
| Indexing sessions makes `prefix + p` slow on first open if the user has dozens of orphan sockets. | Cap session enumeration at 50; show ellipsis if more. Building 50 entries from the socket directory is < 500 µs in a stat-heavy filesystem. |
| Recent history file grows unbounded if the cap regresses. | Test asserts cap = 50 after pushing 100 entries; CI fails on regression. |
| `nucleo-matcher` allocates per query. | The crate documents its allocations; the matcher reuses an internal buffer. We re-use one `Matcher` instance per palette session; no per-keystroke `new`. |
| Discoverability of palette itself. | Mention `prefix + p` in the help overlay (`src/render.rs:1411-1463`) and add a one-line hint in the empty status-bar segment when no command is being typed. |

## 11. Open questions

1. **Empty-query behaviour** — should the palette open with all entries listed (current Helix behaviour) or empty until the user types? Default proposal: list all entries on open; ranking falls back to source order (commands alphabetical; recents most-recent-first).
2. **Source-specific actions** — should `attach-session` be replaceable with `switch-client -t` to match tmux verbatim? Default proposal: support both as aliases (SPEC 09 vocabulary already does aliases).
3. **Live preview** — VS Code Cmd+P highlights the file under the cursor before Enter. Should selecting a `pane` highlight the pane while still in the palette? Default proposal: yes for `panes` and `tabs` (cheap, render path already supports highlight); skip for `commands` and `sessions` because preview semantics are unclear.
4. **`nucleo` (full crate) vs `nucleo-matcher` (just the matcher)** — `nucleo` adds parallel indexing for huge corpora; we don't need it. Default proposal: stick with `nucleo-matcher`. (PRD also names this.)
