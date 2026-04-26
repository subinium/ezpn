# SPEC 13 — Copy-mode upgrades: regex search, named buffers, emacs keys

**Status:** Draft
**Related issue:** TBD
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** D. Feature parity with tmux

---

## 1. Background

Copy mode in ezpn (`src/copy_mode.rs`) shipped in v0.7 with a vi-style
keymap, substring-only search, and OSC 52 yank to a single transient
clipboard slot. tmux has shipped beyond all three for years:

- **Regex search.** tmux's `search-forward` / `search-backward` accept a
  POSIX extended regex. `ezpn` does substring-only at
  `src/copy_mode.rs:529-546` (`lower_text.find(&lower_query)`). Power users
  searching scrollback for `ERROR.*\d{3,}` get nothing.
- **Named paste buffers.** tmux maintains a stack of named buffers; you can
  `set-buffer`, `list-buffers`, `paste-buffer -b name`, `delete-buffer`.
  `ezpn` only writes OSC 52 (`src/daemon/keys.rs:277-282`) — no daemon-side
  storage, no listing, no naming, and no programmatic paste.
- **Emacs key table.** tmux ships `mode-keys vi` and `mode-keys emacs` for
  copy mode. `ezpn`'s copy-mode handler (`src/copy_mode.rs:110-325`) is
  hard-coded vi.

The PRD calls these out explicitly (line 89: "Copy-mode upgrades — regex
search, named paste buffers, emacs key table"). They're bundled in one SPEC
because they all touch `copy_mode.rs` and the daemon clipboard plumbing.

---

## 2. Goal

Three independently-shippable improvements to copy mode and clipboard:

1. **Regex search** — a `regex` crate-backed engine selectable via config
   or runtime command, with substring as the default for backwards
   compatibility.
2. **Named paste buffers** — daemon-side LRU registry of named buffers,
   keyboard + IPC + palette access, optional XDG-state persistence,
   transparent OSC 52 mirror.
3. **Emacs key table** — a parallel keymap for copy mode chosen by a
   config key, with all the same actions wired up.

---

## 3. Non-goals

- **Multi-line regex search.** Search remains line-scoped (one match per
  row, walking left-to-right). Cross-line matches require building a
  flat string of the entire visible buffer + scrollback, which doubles
  memory peak during the search; defer to v0.11 if asked.
- **Structured paste-buffer search** (`tmux choose-buffer` fuzzy filter).
  v0.10 ships flat list + name-prefix selection. Fuzzy lands once the
  command palette (SPEC 10) is in.
- **System clipboard sync.** Named buffers are daemon-local (+ optional
  disk). Cross-machine clipboard sync is out of scope; OSC 52 is the
  only system-clipboard surface.
- **Per-pane buffer scope.** Buffers are workspace-global, matching
  tmux. Per-pane buffers are not on the roadmap.
- **Vim-script-style register expressions** (`"ay`, `"+p`). The named
  buffer system is keyboard-discoverable but not modelled on vim
  registers.

---

## 4. Design

### 4.1 (a) Regex search

#### Dependency

`regex` is currently a *transitive* dep of `criterion` (a dev-dep) — it
shows up in `Cargo.lock` but not `Cargo.toml`. We need to add it as a
real production dep:

```toml
[dependencies]
# ...existing...
regex = { version = "1", default-features = false, features = ["std", "unicode"] }
```

`default-features = false` + `["std", "unicode"]` keeps the build size
modest (drops perf features we don't need: `perf-dfa-full`,
`perf-onepass`, `perf-backtrack`). Build cost: roughly +250 KB to the
release binary, no new transitive C deps.

#### State machine

`CopyModeState` (`src/copy_mode.rs:21-32`) gains one field:

```rust
pub struct CopyModeState {
    // ...existing fields...
    pub search_engine: SearchEngine,  // NEW
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SearchEngine {
    Substring,   // default; current behaviour
    Regex,       // POSIX-ish via the `regex` crate
}
```

`execute_search` (`src/copy_mode.rs:494-579`) becomes a dispatch:

```rust
fn execute_search(state: &mut CopyModeState, screen: &vt100::Screen) {
    let query = match &state.phase { Phase::Search { query, .. } => query.clone(),
                                     _ => state.last_search.clone().unwrap_or_default() };
    if query.is_empty() {
        state.search_matches.clear();
        state.current_match_idx = None;
        return;
    }
    let matches = match state.search_engine {
        SearchEngine::Substring => find_substring(&query, screen, state.pane_rows, state.pane_cols),
        SearchEngine::Regex => find_regex(&query, screen, state.pane_rows, state.pane_cols),
    };
    state.search_matches = matches;
    // ... existing jump-to-nearest logic ...
}

fn find_regex(query: &str, screen: &vt100::Screen, rows: u16, cols: u16)
    -> Vec<(u16, u16, u16)>
{
    // Build with case-insensitive flag if query contains no uppercase
    // (smart-case, matching vim/ripgrep convention).
    let pattern = if query.chars().any(|c| c.is_uppercase()) { query.to_string() }
                  else { format!("(?i){query}") };
    let Ok(re) = regex::Regex::new(&pattern) else {
        return Vec::new();   // invalid regex = no matches; status bar surfaces error elsewhere
    };
    let mut matches = Vec::new();
    for r in 0..rows {
        // Reuse the row-text + col-map building from find_substring.
        let (row_text, col_map) = build_row_text(screen, r, cols);
        for m in re.find_iter(&row_text) {
            if let Some(&col) = col_map.get(m.start()) {
                let display_len = row_text[m.start()..m.end()].width() as u16;
                matches.push((r, col, display_len));
            }
        }
    }
    matches
}
```

The col-map fix from `src/copy_mode.rs:533-543` (display width vs byte
length, issue #15) carries over verbatim — emoji/wide-char highlights
stay correct.

#### Toggling

Three ways to switch engine:

1. **Config** (preferred):
   ```toml
   [copy_mode]
   search = "regex"   # or "substring" (default)
   ```
2. **Command palette**: `:set search regex` / `:set search substring`.
3. **In-mode toggle**: pressing `Ctrl+R` while in copy-mode `Search`
   phase flips the current search's engine and re-executes.

The `SearchEngine` enum is loaded into `CopyModeState::new` from
`Settings` at copy-mode entry (`src/daemon/keys.rs:373-379`).

#### Invalid regex handling

A bad regex (e.g. unclosed `[`) does *not* panic — `Regex::new` returns
`Err`. `find_regex` returns an empty match list and the status-bar
search prompt renders `ERR: <message>` in the regex error style. The
search phase stays open so the user can fix the pattern.

### 4.2 (b) Named paste buffers

#### Daemon-side registry

A new module `src/buffers.rs`:

```rust
use std::collections::VecDeque;
use std::time::{Instant, SystemTime};

const MAX_BUFFERS: usize = 50;

pub struct Buffer {
    pub name: String,
    pub content: String,
    pub created: SystemTime,    // wall-clock for display + persistence
    pub last_used: Instant,     // monotonic for LRU
    pub size_bytes: usize,      // cached for status / list output
}

#[derive(Default)]
pub struct BufferRegistry {
    /// Front = MRU, back = LRU. Bounded at MAX_BUFFERS.
    buffers: VecDeque<Buffer>,
    next_auto_id: usize,
}

impl BufferRegistry {
    /// Add or overwrite a buffer. If `name` is None, generates "buffer<N>".
    /// Evicts LRU if registry is full.
    pub fn set(&mut self, name: Option<String>, content: String) -> String;

    /// Touch + return content of buffer named `name`. None if not found.
    pub fn get(&mut self, name: &str) -> Option<&str>;

    pub fn delete(&mut self, name: &str) -> bool;
    pub fn list(&self) -> Vec<&Buffer>;        // MRU-first
    pub fn clear(&mut self);
}
```

Stored on `Workspace` (one registry per daemon, matching tmux).

#### Yank flow

Today's flow (`src/daemon/keys.rs:277-283`):
```
y/Enter → CopyAndExit(text) → osc52_pending.push(formatted) → OSC 52 to terminal
```

New flow:
```
y/Enter        → CopyAndExit { text, name: None }      → registry.set(None, text)
                                                       + OSC 52 (if osc52 enabled)
"<NAME>y       → CopyAndExit { text, name: Some(NAME) }→ registry.set(Some(NAME), text)
                                                       + OSC 52 (if osc52 enabled)
```

`"<NAME>` is a vi-style register prefix entered in copy mode before the
yank action. State machine adds a `Phase::Register { buffer: String }`
sub-phase entered on `"`, accepting alphanumeric chars + underscore (max
32 chars), exited on yank or Esc.

For emacs mode, the equivalent is `M-x set-buffer-name NAME` typed in
the command palette, *or* `M-w` followed by a palette `:rename-buffer
NAME`. Cleaner: yank-with-name is exclusively a vi-table action;
emacs-table users yank to the auto-named buffer and rename via palette.

#### Palette commands

```
:list-buffers                  → opens overlay listing all buffers (name, size, age)
:show-buffer NAME              → dump content into the active pane (writes raw bytes)
:paste-buffer NAME             → write content to active pane via standard paste path
                                 (bracketed if pane has BP enabled)
:delete-buffer NAME            → remove from registry
:set-buffer NAME [-- CONTENT]  → set named buffer; if CONTENT omitted, reads stdin
                                 of the ezpn-ctl invocation (CLI only path)
:save-buffer NAME PATH         → write buffer content to PATH on disk
:load-buffer PATH [NAME]       → read PATH from disk into a buffer
```

Names allowed: `[a-zA-Z0-9_]{1,32}`. Reserved auto-names: `buffer0`,
`buffer1`, ... — overwritable by user but auto-incremented to avoid
collision.

#### OSC 52 interop

OSC 52 is the bridge to the system clipboard. The default behaviour
when a user yanks:

- Always write to the named buffer registry (the new path).
- Also push an OSC 52 sequence to the terminal *if and only if*
  `[clipboard].osc52 = true` (already an existing config knob; check
  `src/config.rs`). Default remains `true`.

So the user gets both: a daemon-side buffer (queryable via `ezpn-ctl
list-buffers`) and the system clipboard. Setting a buffer
programmatically via `:set-buffer foo bar` does *not* trigger an OSC 52
push by default — explicit `osc52 = true` AND a new `[clipboard]
mirror_set_buffer = false` knob (default false) gates that path. This
matches the principle of least surprise: programmatic register set
shouldn't hijack the user's clipboard.

#### Persistence

```toml
[clipboard]
osc52 = true                # existing
persist_buffers = false     # NEW; default off
```

When `persist_buffers = true`, `BufferRegistry` writes to
`$XDG_STATE_HOME/ezpn/buffers.json` (default
`~/.local/state/ezpn/buffers.json`) on every mutation,
debounced by 500 ms. Format:

```json
{
  "version": 1,
  "buffers": [
    { "name": "errors", "content": "...", "created": "2026-04-26T10:15:00Z" }
  ]
}
```

Loaded on daemon startup. `last_used` resets to `Instant::now()` on
load (we don't persist monotonic time). Max file size: 4 MiB
(buffers larger than this are dropped from the persisted snapshot but
remain in-memory; logged as a warning).

### 4.3 (c) Emacs key table

#### Mode selection

```toml
[copy_mode]
keys = "vi"      # default, current behaviour
# or
keys = "emacs"
```

Loaded into `Settings`; passed to `CopyModeState::new` and stored as a
`KeyTable` enum. `handle_key` dispatches to either
`handle_key_vi` (existing logic, lifted unchanged) or `handle_key_emacs`
(new).

#### State machine difference

Both tables share `Phase` (`src/copy_mode.rs:10-19`). The vi mode's
explicit `Navigate` / `VisualChar` / `VisualLine` distinction is
preserved for emacs, *but emacs has no separate "visual-line" phase
on its keymap* — `C-Space` enters a single mark-based selection that's
character-grained. The line-grained selection is reachable via
palette `:select-line`. Other than that, the phase set is identical.

```
                  ┌─────────────┐         ┌──────────────┐
   Enter copy ──▶ │  Navigate   │◀──Esc──┤   Search     │
   (prefix [)     │             ├─/ or ?▶│              │
                  └─────────────┘         └──────────────┘
                       │  ▲
                  C-Space │  C-g (cancel selection)
                       ▼  │
                  ┌─────────────┐
                  │ VisualChar  │
                  │  (mark-set) │
                  └─────────────┘
                       │ M-w (yank) or C-w (yank+exit)
                       ▼
                  Exit copy mode + buffer set
```

#### Equivalences

| Action                | vi (current)    | emacs (new)              |
|-----------------------|-----------------|--------------------------|
| Enter copy mode       | `prefix [`      | `prefix [`               |
| Exit                  | `q` / `Esc`     | `C-g` / `Esc` / `q`      |
| Char left / right     | `h` / `l`       | `C-b` / `C-f`            |
| Line up / down        | `k` / `j`       | `C-p` / `C-n`            |
| Word forward / back   | `w` / `b`       | `M-f` / `M-b`            |
| Line start / end      | `0` / `$`       | `C-a` / `C-e`            |
| First non-blank       | `^`             | `M-m`                    |
| Buffer top / bottom   | `g` / `G`       | `M-<` / `M->`            |
| Half-page up / down   | `C-u` / `C-d`   | `M-v` / `C-v`            |
| Page up / down        | `PageUp/Dn`     | (same)                   |
| Top / mid / btm row   | `H` / `M` / `L` | `M-r` / `M-R` / (n/a)    |
| Search forward        | `/`             | `C-s`                    |
| Search backward       | `?`             | `C-r`                    |
| Next / prev match     | `n` / `N`       | `C-s` / `C-r` (re-press) |
| Begin selection       | `v`             | `C-Space`                |
| Begin line selection  | `V`             | (palette only)           |
| Yank + exit           | `y` / `Enter`   | `M-w` / `C-w`            |
| Cancel selection      | `v` (toggle)    | `C-g` (re-press)         |
| Yank to named buffer  | `"NAME` then `y`| (palette `:set-buffer`)  |

`C-s` / `C-r` doubling for both "enter search" and "next match" follows
emacs/isearch convention: in Search phase, re-pressing `C-s` advances to
the next match without re-typing the query.

#### Implementation

Refactor `src/copy_mode.rs`:

```rust
pub fn handle_key(
    key: KeyEvent,
    state: &mut CopyModeState,
    screen: &vt100::Screen,
    keys: KeyTable,    // NEW
    scroll_up: &mut dyn FnMut(usize),
    scroll_down: &mut dyn FnMut(usize),
) -> CopyAction {
    // Search-phase handling is shared (text input is the same for both tables).
    if matches!(state.phase, Phase::Search { .. }) {
        return handle_search_input(key, state, screen);
    }
    match keys {
        KeyTable::Vi    => handle_navigate_vi(key, state, screen, scroll_up, scroll_down),
        KeyTable::Emacs => handle_navigate_emacs(key, state, screen, scroll_up, scroll_down),
    }
}
```

`handle_navigate_vi` is the current `handle_key` body, lifted with no
behavioural changes. `handle_navigate_emacs` is new.

The dispatcher in `src/daemon/keys.rs:267-273` passes
`settings.copy_mode_keys` to `copy_mode::handle_key`.

---

## 5. Surface changes

### 5.1 IPC / wire protocol

Additions to `IpcRequest` (`src/ipc.rs`):

```rust
pub enum IpcRequest {
    // ...existing...
    SetBuffer  { name: Option<String>, content: String },
    GetBuffer  { name: String },
    ListBuffers,
    DeleteBuffer { name: String },
    PasteBuffer  { name: String, pane: Option<usize> },
    SaveBuffer   { name: String, path: String },
    LoadBuffer   { path: String, name: Option<String> },
}
```

`IpcResponse` (`src/ipc.rs:56-65`) gains an optional field:

```rust
pub struct IpcResponse {
    pub ok: bool,
    pub message: Option<String>,
    pub error: Option<String>,
    pub panes: Option<Vec<PaneInfo>>,
    pub buffers: Option<Vec<BufferInfo>>,    // NEW
    pub buffer_content: Option<String>,      // NEW (for GetBuffer)
}

pub struct BufferInfo {
    pub name: String,
    pub size: usize,
    pub created: String,    // RFC3339
}
```

No binary protocol bump (all changes are JSON over the IPC socket).

### 5.2 CLI

```
ezpn-ctl set-buffer [--name NAME] [--from-file PATH | --]
    Set a named buffer. With --, reads stdin until EOF.

ezpn-ctl get-buffer NAME
    Print buffer content to stdout.

ezpn-ctl list-buffers
    Print buffers as TSV: NAME\tSIZE\tCREATED.

ezpn-ctl delete-buffer NAME

ezpn-ctl paste-buffer NAME [--pane N]
    Paste named buffer into pane N (default: active pane).

ezpn-ctl save-buffer NAME PATH

ezpn-ctl load-buffer PATH [--name NAME]
```

### 5.3 Config (TOML)

```toml
[copy_mode]
keys = "vi"                 # vi | emacs (default vi)
search = "substring"        # substring | regex (default substring)

[clipboard]
osc52 = true                # existing
persist_buffers = false     # NEW: write/read ~/.local/state/ezpn/buffers.json
mirror_set_buffer = false   # NEW: also OSC52 when programmatically set
max_buffers = 50            # NEW: registry cap (LRU-evicted beyond this)
```

All keys are optional; existing configs keep working with the defaults
(which match v0.9 behaviour: vi keys, substring search, no persistence).

### 5.4 Keybindings (default)

In addition to the per-mode tables in §4.3:

| Mode      | Binding   | Action                                       |
|-----------|-----------|----------------------------------------------|
| Copy (vi) | `"NAME y` | Yank to named buffer NAME                    |
| Copy (vi) | `Ctrl+R`  | Toggle search engine (substring ⟷ regex)     |
| Copy (em) | `Ctrl+R`  | Search backward (also toggle on second press)|
| Normal    | `prefix =`| Open buffer-list overlay (`:list-buffers`)   |
| Normal    | `prefix ]`| Paste most-recent buffer (tmux compatible)   |

`prefix =` and `prefix ]` are unbound today (`src/daemon/keys.rs:297-510`);
no collisions.

---

## 6. Touchpoints

| File | Lines | Change |
|---|---|---|
| `Cargo.toml` | 23-34 | Add `regex = { version = "1", default-features = false, features = ["std", "unicode"] }`. |
| `src/copy_mode.rs` | 10-32 | Add `Phase::Register { buffer: String }`, `SearchEngine` enum, `key_table` plumbing. |
| `src/copy_mode.rs` | 110-325 | Split `handle_key` into vi and emacs paths; share `handle_search_input`. |
| `src/copy_mode.rs` | 494-579 | Refactor `execute_search` to dispatch on `SearchEngine`; add `find_regex` + `find_substring`. |
| `src/buffers.rs` | new | `BufferRegistry` + `Buffer` + persistence (debounced JSON writer). |
| `src/lib.rs` (or `src/main.rs`) | — | `mod buffers;`. |
| `src/app/state.rs` (Workspace) | — | `pub buffers: BufferRegistry` field. |
| `src/daemon/keys.rs` | 267-294 | Plumb registry + key table into copy-mode dispatch; `Phase::Register` handling. |
| `src/daemon/keys.rs` | 297-510 | Add `prefix =` and `prefix ]` bindings. |
| `src/daemon/dispatch.rs` | 147-258 | Add palette commands `set-buffer`, `list-buffers`, `paste-buffer`, etc. |
| `src/ipc.rs` | 17-95 | Add buffer-related variants + `BufferInfo` + `buffers`/`buffer_content` response fields. |
| `src/config.rs` | — | Read `[copy_mode]` and new `[clipboard]` keys. |
| `src/settings.rs` | — | Add `copy_mode_keys: KeyTable`, `search_engine: SearchEngine`. |
| `src/bin/ezpn-ctl.rs` | — | Add buffer subcommands. |
| `tests/copy_mode_regex.rs` | new | Regex search integration test. |
| `tests/named_buffers.rs` | new | Set/get/list/paste round-trip. |
| `tests/copy_mode_emacs.rs` | new | Key-by-key emacs navigation test. |

---

## 7. Migration / backwards-compat

- **New direct dependency**: `regex` 1.x (already transitive via
  `criterion`, so the lockfile entry is unchanged; Cargo.toml gains an
  explicit line).
- **No protocol bump**. JSON IPC is forward-compatible; clients on
  v0.9 don't speak the buffer commands and don't need to.
- **Config additions are all optional with backwards-compatible
  defaults** (`vi`, `substring`, no persistence). Existing
  `~/.config/ezpn/config.toml` files continue to work unchanged. Issue
  #28 (config schema) tracks documenting the additions.
- **Behavioural change**: yanking in copy mode now *also* writes to a
  daemon-side buffer (auto-named `buffer0`, `buffer1`, ...) in addition
  to the OSC 52 push. This is additive — the OSC 52 path is unchanged.
  Documented in CHANGELOG; no opt-out (the registry has a 50-entry cap
  with LRU eviction, so cost is bounded).
- **Persistence file** (`~/.local/state/ezpn/buffers.json`) is
  schema-versioned (`"version": 1`). Future schema bumps must
  preserve-or-migrate; v0 → v1 isn't a concern (this is the first
  version).

---

## 8. Test plan

### Unit tests (`src/copy_mode.rs::tests`, `src/buffers.rs::tests`)

- **`regex_smart_case_lowercase_query_matches_uppercase`** —
  searching `error` finds `ERROR`.
- **`regex_uppercase_in_query_disables_smart_case`** —
  searching `ERROR` does *not* find `error`.
- **`regex_invalid_pattern_returns_empty`** — `[unclosed`
  produces 0 matches, no panic.
- **`regex_match_display_width_correct`** — searching `🔍.` against
  `xx🔍xx` returns highlight length 3 cells (emoji=2 + char=1), not 5
  bytes.
- **`substring_path_unchanged`** — regression for issue #15
  (display-width vs byte-length highlighting).
- **`buffer_set_evicts_lru`** — fill registry with 50 buffers, set
  one more, the oldest-touched is gone.
- **`buffer_get_touches_lru`** — `get("a")` then add 50 new ones;
  `"a"` is still present (it was touched).
- **`buffer_auto_name_no_collision`** — call `set(None, ...)` 60
  times; all 50 surviving have unique names.
- **`buffer_persist_round_trip`** — write to a temp dir, drop
  registry, reload; contents and names match.
- **`emacs_yank_with_mark_set`** — synthesize keys (`C-Space` then
  `C-f`*5 then `M-w`); selection extracted matches expected substring.
- **`vi_register_prefix_yanks_to_named_buffer`** — sequence
  `"foo<v><$><y>` puts the line into buffer `foo`.

### Integration test (`tests/copy_mode_regex.rs`)

```
1. Spawn pane, write 5 lines including "ERROR 404", "warning ERR-12", "ok".
2. Enter copy mode, set search engine = regex.
3. Search /^ERR/ — assert exactly 1 match (line 1), cursor lands on line 1.
4. Search /ERR.*\d+/ — assert 2 matches (lines 1 and 2).
5. Press n — cursor advances to second match.
6. Set search engine = substring; same /^ERR/ — assert 0 matches
   (substring search doesn't anchor).
```

### Integration test (`tests/named_buffers.rs`)

```
1. Spawn daemon, open one pane.
2. ezpn-ctl set-buffer --name greet -- (stdin "hello\nworld\n")
3. ezpn-ctl list-buffers — assert "greet\t12\t<rfc3339>" present.
4. ezpn-ctl get-buffer greet — stdout == "hello\nworld\n".
5. ezpn-ctl paste-buffer greet --pane 0 — assert "hello\nworld\n"
   appears in pane 0's vt100 buffer.
6. Spawn 51 buffers via repeated set-buffer (no --name) — assert
   list-buffers count == 50, the oldest is gone, "greet" still present
   (it was set first but then accessed — touched LRU).
7. Enable persist_buffers=true, set 3 buffers, kill -9 the daemon,
   restart, assert all 3 still listed.
```

### Integration test (`tests/copy_mode_emacs.rs`)

```
1. Spawn pane, write 3 lines of distinct text.
2. With copy_mode.keys = "emacs", enter copy mode (prefix [).
3. Send C-p C-p C-a — cursor at row=0 col=0.
4. Send C-Space C-f C-f C-f M-w — assert clipboard (named buffer
   "buffer0") contains the 3 chars highlighted.
5. Verify Ctrl+G in selection cancels back to Navigate phase
   without yanking.
```

### Property test (proptest)

- **`regex_substring_equivalence_for_literals`** — for any literal
  string `s` (no regex metachars), regex search and substring search
  return the same match positions.

---

## 9. Acceptance criteria

- [ ] `regex` added as a direct dependency in `Cargo.toml` with
      minimised features.
- [ ] `[copy_mode] search = "regex"` enables regex search; bad
      patterns don't panic.
- [ ] `Ctrl+R` in copy-mode `Search` phase toggles engine and
      re-runs the search.
- [ ] `BufferRegistry` evicts LRU at 50 entries; `last_used` is
      updated by `get`.
- [ ] `ezpn-ctl set-buffer` / `list-buffers` / `paste-buffer` round
      trip for content > 1 MiB.
- [ ] `[clipboard] persist_buffers = true` survives daemon restart.
- [ ] `[copy_mode] keys = "emacs"` makes `C-n`/`C-p`/`C-Space`/`M-w`
      navigate + select + yank correctly.
- [ ] OSC 52 still fires on yank when `[clipboard] osc52 = true`,
      regardless of named buffer behaviour.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] No regression in existing copy-mode tests (display-width
      highlighting, search jumping, etc).

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| `regex` adds binary-size weight users don't want | Use `default-features = false` + minimal feature set; document in CHANGELOG. Substring remains the default. |
| Regex search on a 100k-line scrollback locks the main thread | v0.10 only searches the *visible* `pane_rows` (same scope as today's substring search). Scrollback search is a future SPEC. |
| Named buffer registry leaks memory if user sets many huge buffers | LRU eviction at 50 entries; `max_buffers` config knob. Persistence file capped at 4 MiB (oversize buffers dropped from disk, retained in memory until evicted). |
| Emacs/vi confusion: user thinks they're in vi but config says emacs | Status-bar mode label includes the table: `COPY[vi]` or `COPY[em]`. |
| Concurrent yank from two clients (shared mode) clobbers buffer | Registry mutations go through the daemon main loop (single-threaded); no races. |
| Persistence file becomes corrupted (truncated write, disk full) | Write to `buffers.json.tmp` then atomic rename. On load failure, log warning and start with empty registry (don't crash). |
| OSC 52 mirror on `set-buffer` surprises users who expect quiet ops | Defaulted to `mirror_set_buffer = false`; only triggers on explicit user opt-in. |

---

## 11. Open questions

1. **Smart-case for substring search?** Currently substring is always
   case-insensitive (`lower_query` at `src/copy_mode.rs:506`). Should we
   add smart-case for substring too, for consistency with the new
   regex path? *Default proposal:* yes, in a follow-up — out of scope
   for this SPEC to avoid scope creep.
2. **Vim-style register addressing in palette?** `:reg foo` to inspect,
   `"+y` for system clipboard register? *Default proposal:* defer.
   Named buffers + palette commands cover the use cases.
3. **Should `paste-buffer` from CLI block until the pane echoes it?**
   *Default proposal:* no — fire-and-forget, like `send-keys` (SPEC 06).
   Caller can poll the pane content if needed.
4. **Buffer expiry?** tmux buffers live forever (until evicted by LRU
   or explicit delete). Should we add `[clipboard] buffer_ttl = "7d"`
   for auto-pruning? *Default proposal:* no for v0.10. Revisit if
   the persist file grows unboundedly in real usage.
5. **What about regex with line-anchors when search is line-scoped?**
   `^foo` and `foo$` work as expected (each row is its own subject
   string). Document this clearly in the help overlay.
