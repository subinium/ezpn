# v0.6 Spec — Feature Parity Push

Current: 3,914 LOC, 22 tests, 6 deps.

## Scope

All features below ship in a single release. No partial merges.

| # | Feature | New Module | Est. Lines |
|---|---------|-----------|------------|
| 1 | Prefix key mode (`Ctrl+B`) | — (main.rs) | +80 |
| 2 | Tabs / windows | `tab.rs` | +200 |
| 3 | Scrollback + scroll mode | pane.rs + render.rs | +120 |
| 4 | Title bar buttons (split ┃ ━ next to ×) | render.rs | +40 |
| 5 | Config file (`~/.config/ezpn/config.toml`) | `config.rs` | +150 |
| 6 | Quit confirmation | main.rs + render.rs | +30 |
| **Total** | | 2 new modules | **+620** |

Target: ~4,500 LOC.

---

## 1. Prefix Key Mode

### Spec

Press `Ctrl+B` → enters "prefix mode" for 1 second → next key is interpreted as a command.

| Sequence | Action | tmux equiv |
|----------|--------|------------|
| `Ctrl+B %` | Split left\|right | `Ctrl+B %` |
| `Ctrl+B "` | Split top/bottom | `Ctrl+B "` |
| `Ctrl+B o` | Next pane | `Ctrl+B o` |
| `Ctrl+B Arrow` | Navigate directionally | `Ctrl+B Arrow` |
| `Ctrl+B x` | Close active pane | `Ctrl+B x` |
| `Ctrl+B z` | Zoom/unzoom pane | `Ctrl+B z` |
| `Ctrl+B E` | Equalize | custom |
| `Ctrl+B c` | New tab | `Ctrl+B c` |
| `Ctrl+B n` | Next tab | `Ctrl+B n` |
| `Ctrl+B p` | Previous tab | `Ctrl+B p` |
| `Ctrl+B [` | Enter scroll mode | `Ctrl+B [` |
| `Ctrl+B d` | Quit (with confirm) | `Ctrl+B d` |

### Implementation

```rust
// In main.rs
enum InputMode {
    Normal,
    Prefix { entered_at: Instant },
}
```

In the event loop:
- `Normal` mode: all keys forwarded to active pane, except `Ctrl+B` which transitions to `Prefix`
- `Prefix` mode: next key is dispatched to the command table. If 1 second elapses with no key, return to `Normal`
- Existing direct shortcuts (`Ctrl+D`, `Ctrl+E`, etc.) remain as alternatives

Status bar shows `[PREFIX]` indicator when in prefix mode.

### Files Changed

| File | Change |
|------|--------|
| `main.rs` | Add `InputMode` enum, prefix timeout logic, command dispatch table |
| `render.rs` | Show `[PREFIX]` in status bar |

---

## 2. Tabs / Windows

### Spec

```
┌ Tab 1 ─────┬ Tab 2 ─────┬ Tab 3 ─────┐
│ ● dev      │   build    │   logs     │
╰────────────┴────────────┴────────────╯
╭──────────┬──────╮
│    1     │  2   │
│          ├──────┤
│          │  3   │
╰──────────┴──────╯
```

- Tab bar at the top (1 row)
- Each tab has its own Layout + panes
- Active tab has `●` indicator
- Ctrl+B c = new tab
- Ctrl+B n/p = next/prev tab
- Ctrl+B 1-9 = jump to tab
- Click tab name to switch

### Data Model

```rust
// tab.rs
pub struct Tab {
    pub name: String,
    pub layout: Layout,
    pub panes: HashMap<usize, Pane>,
    pub active_pane: usize,
}

pub struct TabManager {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
}
```

### Implementation

| File | Change |
|------|--------|
| `tab.rs` (new) | `Tab`, `TabManager`, new/close/switch/rename |
| `main.rs` | Replace `layout + panes + active` with `TabManager`. Event loop dispatches to active tab. |
| `render.rs` | Add `draw_tab_bar()`. Inner rect shifts down 1 row when tab bar visible. |
| `workspace.rs` | Snapshot includes all tabs |

### Tab bar rendering

```
 ● dev │ build │ logs                              +
```

- `+` button at the right end to create new tab (clickable)
- Click tab name to switch
- Active tab: bold + highlight color
- Tab bar takes 1 row from the top

---

## 3. Scrollback + Scroll Mode

### Spec

- Scrollback buffer: 10,000 lines per pane (configurable)
- Mouse scroll wheel: scroll up/down in active pane's history
- `Ctrl+B [`: enter scroll mode
  - Arrow keys / PgUp / PgDown to navigate
  - `q` or `Esc` to exit scroll mode
- Scroll indicator in pane title: `[42/1000]` when scrolled up

### Implementation

```rust
// In pane.rs
vt100::Parser::new(rows, cols, 10_000) // was 0

// Scroll state per pane
pub struct ScrollState {
    pub offset: usize,      // lines scrolled up from bottom (0 = live)
    pub in_scroll_mode: bool,
}
```

When `offset > 0`:
- Render from `screen.scrollback(offset)` instead of live screen
- Pane title shows `[scrolled: {offset}]`
- New PTY output resets offset to 0 (snap to bottom)

| File | Change |
|------|--------|
| `pane.rs` | Change scrollback to 10000, add `ScrollState` |
| `render.rs` | Render scrollback view when offset > 0, draw indicator |
| `main.rs` | Handle scroll events, scroll mode key dispatch |

---

## 4. Title Bar Buttons

### Spec

Each pane's title bar:
```
─ 1 ────────────────────── ━ ┃ ×─
                            ^  ^ ^
                        split-h  | close
                           split-v
```

- `━` button: click to split horizontally (left|right)
- `┃` button: click to split vertically (top/bottom)
- `×` button: click to close (existing)
- All buttons: 1-cell each with 1-cell gap between

### Implementation

| File | Change |
|------|--------|
| `render.rs` | Update `draw_pane_title()` to render 3 buttons. Update `close_button_hit()` → `title_button_hit()` returning `TitleAction` enum. |
| `main.rs` | Handle `TitleAction::SplitH`, `TitleAction::SplitV`, `TitleAction::Close` from click |

---

## 5. Config File

### Spec

```toml
# ~/.config/ezpn/config.toml

border = "rounded"
shell = "/bin/zsh"
scrollback = 10000
show_status_bar = true
show_tab_bar = true

[keys]
prefix = "ctrl-b"    # prefix key (default: ctrl-b)

[tabs]
default_name = "shell"
```

### Implementation

```rust
// config.rs
#[derive(Deserialize)]
pub struct EzpnConfig {
    pub border: Option<BorderStyle>,
    pub shell: Option<String>,
    pub scrollback: Option<usize>,
    pub show_status_bar: Option<bool>,
    pub show_tab_bar: Option<bool>,
}
```

Priority: CLI args > config file > defaults.

| File | Change |
|------|--------|
| `config.rs` (new) | Config struct, load from `~/.config/ezpn/config.toml`, merge with CLI |
| `Cargo.toml` | Add `toml = "0.8"` dep |
| `main.rs` | Load config before parse_args, merge |

---

## 6. Quit Confirmation

### Spec

When quitting with live panes:
```
╭─────────────────────────────────╮
│  Quit ezpn?                     │
│  3 panes are still running.     │
│                                 │
│  [Quit]        [Cancel]         │
╰─────────────────────────────────╯
```

- Ctrl+B d or Ctrl+\ with live panes → show dialog
- If all panes dead → quit immediately
- Click [Quit] or press `y` → exit
- Click [Cancel] or press `n`/`Esc` → cancel

---

## Dependency Changes

| Dep | Version | Purpose |
|-----|---------|---------|
| `toml` | 0.8 | Config file parsing |

Total deps: 7 (was 6).

---

## Implementation Order

Dependencies between features:

```
1. Prefix key mode ← needed by tabs, scroll mode
2. Tabs            ← needs prefix key for Ctrl+B c/n/p
3. Scrollback      ← needs prefix key for Ctrl+B [
4. Title buttons   ← independent
5. Config file     ← independent (but needs to know about all features)
6. Quit confirm    ← independent
```

### Parallel Groups

| Group | Steps | Can run in parallel |
|-------|-------|-------------------|
| A | 1. Prefix key mode | No deps |
| B | 4. Title bar buttons | No deps |
| C | 2. Tabs (after A) | After A |
| D | 3. Scrollback (after A) | After A, parallel with C |
| E | 5. Config file (after C, D) | After all features exist |
| F | 6. Quit confirmation | After A |

### Execution Plan

```
Step 1: Prefix key + Title buttons        (A + B parallel)
Step 2: Tabs + Scrollback + Quit confirm   (C + D + F parallel, after Step 1)
Step 3: Config file                        (E, after Step 2)
Step 4: Tests + README + release           (verification)
```
