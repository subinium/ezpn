# ezpn Roadmap

## v0.1 — Foundation (DONE)

Binary split tree layout, mouse interaction, settings panel, drag-to-resize.

**Delivered**: 2,551 LOC, 13 tests, 4 deps, 952KB binary.

---

## v0.2 — Preset Layouts & Per-Pane Commands

### Goal
Users can define exact layouts with ratios and run different commands in each pane.

### Spec

```bash
# Ratio-based layouts
ezpn --layout "7:3"              # 70/30 horizontal split
ezpn --layout "1:1:1"            # 3 equal horizontal panes
ezpn --layout "7:3/5:5"          # 2 rows: top 70:30, bottom 50:50
ezpn --layout "1/1:1"            # top full-width, bottom 2 panes

# Per-pane commands
ezpn -e "make watch" -e "npm run dev" -e "tail -f app.log"
ezpn --layout "7:3" -e "vim" -e "cargo watch"

# Combined
ezpn --layout "1/1:1" -e "htop" -e "bash" -e "tail -f /var/log/syslog"
```

### Layout DSL

```
LAYOUT  := ROW ( "/" ROW )*           # rows separated by /
ROW     := RATIO ( ":" RATIO )*      # panes separated by :
RATIO   := integer                    # relative weight (1-9)

"7:3"       → H-split, 70% left, 30% right
"1:1:1"     → H-split, 33% each
"7:3/5:5"   → V-split top/bottom; top H-split 70:30; bottom H-split 50:50
"1/1:1"     → V-split; top 1 pane; bottom H-split 50:50
```

### Implementation

| File | Change | Lines |
|------|--------|-------|
| `layout.rs` | Add `Layout::from_spec(spec: &str)` parser | +60 |
| `main.rs` | Add `-e`/`--exec` flag (repeatable), `--layout` flag | +30 |
| `main.rs` | Spawn loop: map pane ID → command from `-e` list | +15 |
| `pane.rs` | `Pane::new` already accepts any `&str` — no change | 0 |

### Key Design Decisions

- Layout DSL uses `/` for rows, `:` for columns within a row. Simple, no nesting syntax needed — the binary tree handles it internally.
- `-e` flags map to panes in tree order (left-to-right, top-to-bottom). Excess panes get the default `$SHELL`.
- `--layout` and positional args (`ezpn 2 3`) are mutually exclusive.

---

## v0.3 — Performance

### Goal
Match tmux-level render performance. No unnecessary re-renders. Bounded memory under fast PTY output.

### Spec

| Metric | v0.1 | v0.3 Target |
|--------|------|-------------|
| Allocations/frame | ~15,000 | ~500 |
| Full screen clear | Every frame | Only on resize/split |
| BorderMap rebuild | Every frame | Cached, invalidate on layout change |
| `yes` memory | Unbounded | Capped at ~128KB/pane |
| Idle CPU | ~0.5% | ~0.1% |

### Implementation

| File | Change | Lines |
|------|--------|-------|
| `render.rs` | Dirty-pane tracking: only redraw panes with new PTY output | +40 |
| `render.rs` | Cache BorderMap in caller, pass as param, rebuild on layout change | +20 |
| `render.rs` | Replace `Clear(All)` with per-pane region clear | +15 |
| `main.rs` | Track `dirty_panes: HashSet<usize>`, pass to render | +20 |
| `main.rs` | `border_dirty` flag: true on resize/split/close/style change | +10 |

### Key Design Decisions

- Dirty tracking is pane-level, not line-level. Simpler than tmux's per-line tracking, but still a 5-10x improvement for typical use.
- BorderMap becomes `Option<Vec<(u16, u16, [bool;4])>>` stored alongside layout. `None` = needs rebuild.
- `sync_channel(32)` already applied in v0.1 perf fix.

---

## v0.4 — External Control (IPC)

### Goal
Control ezpn from outside: split panes, run commands, query state — enabling scripting and automation.

### Spec

```bash
# Start ezpn with IPC enabled (creates socket at /tmp/ezpn-{pid}.sock)
ezpn 2 2

# From another terminal:
ezpn-ctl split horizontal          # split active pane
ezpn-ctl split vertical 2          # split pane 2
ezpn-ctl exec 1 "cargo test"       # run command in pane 1
ezpn-ctl close 3                   # close pane 3
ezpn-ctl layout "7:3/1:1:1"       # reset to new layout
ezpn-ctl equalize                  # equalize sizes
ezpn-ctl list                      # list panes + status
ezpn-ctl focus 2                   # focus pane 2
```

### Architecture

```
ezpn (main process)
  ├── Event loop (terminal events)
  ├── IPC listener thread (Unix socket)
  │     └── Parses commands → sends AppCommand via mpsc channel
  ├── AppCommand receiver (checked each loop iteration)
  └── Panes + Layout + Render (unchanged)

ezpn-ctl (separate binary)
  └── Connects to /tmp/ezpn-{pid}.sock, sends command, receives response
```

### Protocol

JSON over Unix domain socket, newline-delimited:

```json
// Request
{"cmd": "split", "direction": "horizontal", "pane": null}
{"cmd": "exec", "pane": 1, "command": "cargo test"}
{"cmd": "list"}

// Response
{"ok": true}
{"ok": true, "panes": [{"id": 0, "alive": true, "rows": 24, "cols": 80}]}
{"ok": false, "error": "pane not found"}
```

### Implementation

| File | Change | Lines |
|------|--------|-------|
| `ipc.rs` (new) | Unix socket listener, command parser, response builder | +150 |
| `main.rs` | Add `mpsc::Receiver<AppCommand>`, check in event loop | +40 |
| `main.rs` | `handle_app_command()` dispatcher | +60 |
| `bin/ezpn-ctl.rs` (new) | CLI client binary | +80 |
| `Cargo.toml` | Add `[[bin]]` for ezpn-ctl, add `serde_json` dep | +10 |

### Key Design Decisions

- Unix socket (not TCP) — no network exposure, file-based auth via permissions.
- Socket path includes PID for multi-instance support: `/tmp/ezpn-{pid}.sock`.
- IPC listener runs in a separate thread, sends parsed commands via bounded `mpsc` channel.
- JSON protocol is human-readable and scriptable. No binary protocol needed at this scale.
- `ezpn-ctl` is a second binary in the same crate (`[[bin]]` in Cargo.toml).
- Adds 1 dependency: `serde_json` for protocol parsing.

---

## Summary

| Version | Features | New Deps | Est. Lines |
|---------|----------|----------|------------|
| v0.1 | Core multiplexer | — | 2,551 (done) |
| v0.2 | Layout DSL, per-pane commands | — | +105 |
| v0.3 | Dirty render, BorderMap cache | — | +105 |
| v0.4 | IPC control, ezpn-ctl binary | `serde_json` | +340 |
| **Total** | | **5 deps** | **~3,100** |
