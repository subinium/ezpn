# ezpn Plan

## Status

| Version | Scope | Status |
|--------|-------|--------|
| v0.1 | Core pane splitter, mouse resize, settings, close | DONE |
| v0.2 | Layout DSL, per-pane command launch | DONE |
| v0.3 | Dirty render path, border cache, targeted redraw | DONE |
| v0.4 | JSON IPC, `ezpn-ctl`, remote pane control | DONE |
| v0.5 | Workspace snapshot save/load and startup restore | DONE |

---

## v0.1 — Foundation

Binary split tree layout, mouse interaction, settings modal, drag-to-resize, pane close, dead-pane respawn.

### Delivered

- Binary tree layout with split, remove, equalize, directional navigation
- PTY-backed panes with VT100 parsing
- Click-to-focus, drag-to-resize, close button hit-test
- Settings modal for border style, split actions, status bar toggle

### Notes

- Foundation stayed intentionally small and direct.
- The layout tree remains the core state model for later IPC and snapshot work.

---

## v0.2 — Layout DSL & Per-Pane Commands

### Goal

Users can declare exact startup layouts and assign a separate launch command to each pane.

### Supported CLI

```bash
ezpn --layout "7:3"
ezpn --layout "1:1:1"
ezpn --layout "7:3/5:5"
ezpn --layout "1/1:1" -e "htop" -e "npm run dev" -e "tail -f app.log"
```

### What Changed

- `Layout::from_spec()` parses `/` rows and `:` columns.
- Weights are validated as `1-9`.
- `--layout` and positional grid args are mutually exclusive.
- `-e/--exec` commands are launched through `$SHELL -lc <cmd>`.
- Excess panes fall back to an interactive shell.

### Methodology Correction

The original rough pass treated `-e "cargo test"` like a direct executable path, which is wrong for shell commands with spaces, pipes, or redirects. The implemented version fixes that by making command panes explicitly shell-driven while leaving plain shell panes interactive.

### Remaining Improvement Ideas

- Optional nested DSL for asymmetric trees beyond row/column shorthand
- Named startup presets instead of only inline CLI strings

---

## v0.3 — Render Efficiency

### Goal

Stop redrawing the full terminal when only one pane received PTY output.

### Delivered

- Pane-level dirty tracking from PTY polling
- Border geometry cache reused across frames
- Full redraw only on structural changes:
  - resize
  - split / close
  - equalize
  - border style change
  - status bar toggle
  - focus/drag visual changes
- Partial redraw path for pane content/title only
- Cursor hide/show cleanup tied to actual active-pane state

### Methodology

- Keep dirty tracking at pane granularity, not line granularity
- Cache expensive layout-to-border geometry, but still redraw chrome when the visual state changes
- Prefer deterministic invalidation over speculative micro-optimization

### Improvement Ideas

- Line-diff rendering for very large panes
- Scrollback-aware redraw throttling under extremely noisy PTY output
- Explicit benchmark harness for allocations/frame and idle CPU

---

## v0.4 — External Control (IPC)

### Goal

Control ezpn from another terminal for scripting and automation.

### Protocol

JSON over Unix domain socket, newline-delimited.

```json
{"cmd":"split","direction":"horizontal","pane":0}
{"cmd":"exec","pane":1,"command":"cargo test"}
{"cmd":"list"}
```

### Supported Control Surface

```bash
ezpn-ctl list
ezpn-ctl split horizontal
ezpn-ctl exec 1 "cargo test"
ezpn-ctl layout "7:3/1:1"
ezpn-ctl focus 2
ezpn-ctl close 3
ezpn-ctl equalize
```

### Delivered

- Dedicated `ipc.rs` request/response types
- Real JSON request parsing and JSON response encoding
- `ezpn-ctl` client with:
  - latest-socket discovery
  - `--pid <PID>`
  - `--socket <PATH>`
  - `--json`
- Structured `list` output with pane size, active flag, alive state, and launch command

### Methodology Correction

The earlier text-based IPC sketch was useful for proving the thread/channel split, but it diverged from the documented spec. The current implementation closes that gap so automation uses a typed protocol instead of ad-hoc string parsing.

### Improvement Ideas

- Optional event subscription / watch mode
- Stronger socket lifecycle handling on crash recovery
- Batch command transactions for multi-step layout mutations

---

## v0.5 — Workspace Snapshots & Restore

### Goal

Persist reproducible workspace state: layout tree, active pane, commands, and UI settings.

### Supported UX

```bash
ezpn --restore .ezpn-session.json

ezpn-ctl save .ezpn-session.json
ezpn-ctl load .ezpn-session.json
```

### Snapshot Contents

- Layout tree with current ratios
- Next pane ID
- Active pane ID
- Default shell path
- Border style
- Status bar visibility
- Per-pane launch definition:
  - interactive shell
  - shell command

### Explicit Non-Goals

- PTY memory/state is not persisted
- Running process state is not detached and resumed
- Scrollback contents are not serialized

### Design Decisions

- Snapshots serialize the binary layout tree directly, not the limited layout DSL
- Restore is reproducible workspace boot, not tmux-style session reattach
- Dead pane respawn now uses the pane's remembered launch definition

### Implementation

| File | Change |
|------|--------|
| `src/workspace.rs` | Snapshot model, JSON save/load, validation |
| `src/main.rs` | `--restore`, IPC `save/load`, snapshot apply flow |
| `src/pane.rs` | `PaneLaunch` metadata for command persistence |

### Improvement Ideas

- Autosave on clean exit
- Named workspaces under `~/.config/ezpn/`
- Import/export of a more human-friendly TOML format alongside JSON

---

## Methodology Applied

### 1. Correctness Before Surface Feature Count

- Fixed shell-command launch semantics before calling `-e` complete
- Matched IPC behavior to the documented JSON contract
- Made snapshot restore validate pane/layout consistency

### 2. Structural State First

- Layout tree stays the single source of truth
- Pane launch metadata is stored with the pane, so respawn, list, save, and restore share one representation

### 3. Deterministic Invalidation

- Border cache is rebuilt only when geometry or chrome shape changes
- Pane output drives pane-only redraws
- Modal visibility still forces a full redraw to keep overlay rendering simple and predictable

---

## Next Candidate Roadmap

### v0.6 Candidates

- Session autosave / named workspaces
- Layout preset files
- IPC watch mode
- Better scrollback story
- Performance benchmark suite
