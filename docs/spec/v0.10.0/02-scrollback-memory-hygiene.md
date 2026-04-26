# SPEC 02 — Scrollback Memory Hygiene

**Status:** Draft
**Related issue:** TBD (v0.10.0 milestone)
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** A. Stability & Resource Hygiene
**Severity origin:** Audit P2 #3 (no `clear-history`, no per-pane bound, no runtime adjustment)

---

## 1. Background

Every pane in ezpn allocates a `vt100::Parser` with one global
scrollback line count. There is no way to:

1. Override scrollback per-pane (e.g., a high-volume `tail -F` pane
   wants more, an htop pane wants none).
2. Clear an existing pane's scrollback without restarting the pane
   (tmux's `clear-history` has been table-stakes for a decade).
3. Adjust scrollback at runtime — a long-running daemon must be killed
   and rebound just to lower the cap.

`src/pane.rs:196` allocates the parser:

```rust
let parser = vt100::Parser::new(rows, cols, scrollback);
```

The `scrollback` parameter is the single value chosen at *daemon
startup* from the config file:

`src/config.rs:24-37`:

```rust
impl Default for EzpnConfig {
    fn default() -> Self {
        Self {
            border: BorderStyle::Rounded,
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            scrollback: 10_000,
            // ...
        }
    }
}
```

`src/config.rs:81-85` parses and caps:

```rust
"scrollback" => {
    if let Ok(n) = value.parse::<usize>() {
        config.scrollback = n.min(100_000);
    }
}
```

### Worst-case memory math

`vt100::Screen::cell()` returns a `Cell` with foreground/background
colour, attrs, and contents. The library does not expose precise
sizing, but a conservative per-cell heuristic — already used in the
audit and in `src/pane.rs:483-488` — is **32 bytes/cell**:

```rust
pub fn estimated_scrollback_bytes(&self) -> usize {
    let screen = self.parser.screen();
    let (rows, cols) = screen.size();
    let scrollback = screen.scrollback() + rows as usize;
    scrollback.saturating_mul(cols as usize).saturating_mul(32)
}
```

For `100_000 × 200 cols × 32 bytes ≈ 640 MB` *per pane*. A 4-pane
session at the cap can balloon to ~2.5 GB of vt100 ringbuffer alone.
The PRD's 7-day soak test (4 panes, repeated `yes`) is unsurvivable
without bounded-history controls.

### Missing surface

`src/ipc.rs:15-43` — no `ClearHistory` or `SetHistoryLimit` variant in
`IpcRequest`. `src/bin/ezpn-ctl.rs:116-148` — no `clear-history` or
`set-scrollback` subcommand. `.ezpn.toml` has no `[[pane]]` block.

---

## 2. Goal

Give users **three** independent levers, all addressable without a
daemon restart:

1. **Per-pane override** at workspace bring-up via `[[pane]]
   scrollback_lines = N` in `.ezpn.toml`.
2. **Runtime trim** via `ezpn-ctl clear-history --pane N` (drops the
   ringbuffer; visible screen remains).
3. **Runtime resize** via `ezpn-ctl set-scrollback --pane N --lines L`.

Enforce a workspace-wide line-count cap (default 100 000) and emit one
warning log line per pane that crosses a coarse byte budget so users
know they are approaching trouble.

PRD release criterion: `clear-history` reduces per-pane vt100 RSS by
≥ 95 % within 100 ms.

---

## 3. Non-goals

- **Byte-budgeted scrollback** (`--scrollback-mem 256MB`) — explicitly
  deferred to v0.11. The byte heuristic in this SPEC is a *warning*
  trigger only, not a hard cap; vt100 0.15 doesn't expose enough to do
  precise budgeting safely.
- Persisting per-pane overrides into auto-saved snapshots. v0.10
  honours `[[pane]] scrollback_lines` only at workspace load; runtime
  `set-scrollback` is *transient* (lost on detach/reattach round-trip
  unless `persist_scrollback` also saves it — tracked as a follow-up).
- Replacing `vt100` with a bounded-buffer alternative. The dependency
  stays.
- Per-line search-back limits, OSC 1337 image scrollback, semantic
  grouping. All v0.11+.

---

## 4. Design

### 4.1 Config schema

Add a `[scrollback]` table to `~/.config/ezpn/config.toml`:

```toml
[scrollback]
default_lines = 10000   # applied to every pane unless overridden
max_lines     = 100000  # hard ceiling enforced at parse + IPC time
warn_bytes    = 50_000_000  # log a warning when est. usage crosses this
```

The existing flat key `scrollback = N` in the same file remains valid
and maps to `scrollback.default_lines` for back-compat
(`src/config.rs:81-85` keeps its current parse path; the section
parser is additive, not replacing).

`EzpnConfig` (in `src/config.rs:6-22`) gains:

```rust
pub struct EzpnConfig {
    // …existing fields…
    pub scrollback: usize,           // = scrollback_default_lines for back-compat
    pub scrollback_max_lines: usize,
    pub scrollback_warn_bytes: usize,
}
```

Defaults: `scrollback = 10_000`, `scrollback_max_lines = 100_000`,
`scrollback_warn_bytes = 50 * 1024 * 1024` (50 MB).

### 4.2 Per-pane override in `.ezpn.toml`

Project-level overrides live next to existing `[workspace]` /
`[[pane]]` sections. Example:

```toml
[workspace]
persist_scrollback = false

[[pane]]
id = 0
scrollback_lines = 50000   # log-tailing pane wants more

[[pane]]
id = 1
scrollback_lines = 0       # htop pane wants none
```

`project::ResolvedProject` already carries per-pane data
(`shells: HashMap<usize, String>`, `cwds`, `envs`, `names`). Add:

```rust
pub struct ResolvedProject {
    // …existing…
    pub scrollback_overrides: HashMap<usize, usize>,
}
```

`src/app/lifecycle.rs:196-231` (`spawn_project_panes`) consults the
override before passing to `Pane::with_full_config`:

```rust
let scrollback_for_pane = proj
    .scrollback_overrides
    .get(&pid)
    .copied()
    .unwrap_or(scrollback)
    .min(max_scrollback);
```

`max_scrollback` is plumbed through from `EzpnConfig::scrollback_max_lines`.

### 4.3 New IPC commands

Add to `IpcRequest` in `src/ipc.rs:15-43`:

```rust
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    // …existing…
    ClearHistory {
        pane: usize,
    },
    SetHistoryLimit {
        pane: usize,
        lines: usize,
    },
}
```

These dispatch through the existing `handle_ipc_command` switch in
`src/app/input_dispatch.rs:45-`. New arms:

```rust
ipc::IpcRequest::ClearHistory { pane } => {
    if let Some(p) = panes.get_mut(&pane) {
        clear_pane_history(p);
        (IpcResponse::success(format!("cleared history for pane {pane}")), update)
    } else {
        (IpcResponse::error(format!("no pane {pane}")), update)
    }
}
ipc::IpcRequest::SetHistoryLimit { pane, lines } => {
    let lines = lines.min(settings_max_scrollback);
    if let Some(p) = panes.get_mut(&pane) {
        resize_pane_scrollback(p, lines);
        (IpcResponse::success(format!("scrollback for pane {pane} set to {lines}")), update)
    } else {
        (IpcResponse::error(format!("no pane {pane}")), update)
    }
}
```

### 4.4 New `Pane` API

vt100 0.15 does **not** expose a public "drop scrollback rows"
operation. To work around this, two new helpers in `src/pane.rs`:

```rust
impl Pane {
    /// Drop all scrollback above the visible screen by reconstructing
    /// the vt100 parser from the current visible cells. The visible
    /// screen survives; user is snapped to bottom (live view).
    ///
    /// Implementation: reuse `crate::snapshot_blob::encode_scrollback`
    /// to capture the visible rows, then build a fresh
    /// `vt100::Parser::new(rows, cols, current_scrollback_cap)` and
    /// replay via `decode_scrollback`. Total cost ~5–20 ms per pane
    /// for a typical 80×24 screen; well inside the PRD's 100 ms.
    pub fn clear_history(&mut self) -> anyhow::Result<()>;

    /// Resize the scrollback ringbuffer to `new_lines`. If the new cap
    /// is smaller than current usage, oldest rows are dropped via the
    /// same encode/replay rebuild path as `clear_history`. If larger,
    /// a fresh parser is allocated with the higher cap and the
    /// existing visible screen is replayed.
    pub fn set_scrollback_lines(&mut self, new_lines: usize) -> anyhow::Result<()>;

    /// Current scrollback line cap (the value passed to vt100::Parser::new
    /// or set_scrollback_lines, NOT the live `scrollback()` offset).
    pub fn scrollback_cap(&self) -> usize;
}
```

The `scrollback_cap` is tracked as a new `Pane` field
(`scrollback_cap: usize`) since vt100 doesn't expose it.

### 4.5 Memory budget warning

A new helper in `src/daemon/event_loop.rs`, called once per detach and
once per `SetHistoryLimit` request:

```rust
fn warn_if_over_budget(
    panes: &HashMap<usize, Pane>,
    warn_bytes: usize,
    already_warned: &mut HashSet<usize>,
) {
    for (&pid, pane) in panes {
        let est = pane.estimated_scrollback_bytes();
        if est > warn_bytes && already_warned.insert(pid) {
            eprintln!(
                "ezpn: pane {pid} scrollback ~{} MB (cap {} lines × {} cols × ~32 B/cell); \
                 use `ezpn-ctl set-scrollback --pane {pid} --lines N` to lower",
                est / 1_048_576,
                pane.scrollback_cap(),
                pane.screen().size().1,
            );
        }
    }
}
```

`already_warned` is reset when a pane is closed or its scrollback
cap drops; deferred to keep the v0.10 surface minimal.

### 4.6 ezpn-ctl CLI

`src/bin/ezpn-ctl.rs:108-148` parser gets two new arms:

```rust
Some("clear-history") => {
    let pane = parse_required_pane(&args)?;
    Ok(ipc::IpcRequest::ClearHistory { pane })
}
Some("set-scrollback") => {
    let pane = parse_required_pane(&args)?;
    let lines = parse_required_lines(&args)?;
    Ok(ipc::IpcRequest::SetHistoryLimit { pane, lines })
}
```

`--help` text gains:

```
clear-history --pane N           Drop scrollback above the visible screen.
set-scrollback --pane N --lines L
                                 Resize a pane's scrollback ring (max from
                                 [scrollback] max_lines, default 100000).
```

---

## 5. Surface changes

### IPC / wire protocol

The `IpcRequest` enum is `#[serde(tag = "cmd")]` (see
`src/ipc.rs:15-17`), which means new variants are additive on the
wire. Old daemons receiving `clear_history` deserialize-fail and
return `IpcResponse::error("invalid request: …")`, which is the
correct behaviour. No `PROTOCOL_VERSION` bump required (this is
distinct from the binary client/server protocol in
`src/protocol.rs`).

Wire format examples:

```json
{"cmd":"clear_history","pane":2}
{"cmd":"set_history_limit","pane":2,"lines":5000}
```

### CLI (ezpn-ctl)

```
ezpn-ctl clear-history --pane 2
ezpn-ctl set-scrollback --pane 2 --lines 5000
```

Exit codes follow the existing `ezpn-ctl` convention: 0 on
`IpcResponse.ok = true`, non-zero with the error printed to stderr
otherwise.

### Config (TOML)

Global `~/.config/ezpn/config.toml`:

```toml
# Either flat (back-compat):
scrollback = 10000

# Or sectioned (new in v0.10):
[scrollback]
default_lines = 10000
max_lines     = 100000
warn_bytes    = 50000000
```

Project `.ezpn.toml`:

```toml
[[pane]]
id = 0
shell = "/bin/zsh"
scrollback_lines = 50000   # new in v0.10
```

---

## 6. Touchpoints

| File | Lines (approx) | Change |
|---|---|---|
| `src/config.rs` | 6–22, 49–106 | Add `scrollback_max_lines`, `scrollback_warn_bytes`; parse `[scrollback]` section. |
| `src/pane.rs` | 38–68 | Add `scrollback_cap: usize` field. |
| `src/pane.rs` | 196–217 | Initialise `scrollback_cap` on construction. |
| `src/pane.rs` | new ~80 LoC | `clear_history`, `set_scrollback_lines`, `scrollback_cap` impls. |
| `src/project.rs` | `ResolvedProject` | Add `scrollback_overrides: HashMap<usize, usize>`. |
| `src/project.rs` | `.ezpn.toml` parser | Parse `[[pane]] scrollback_lines`. |
| `src/app/lifecycle.rs` | 196–231 | Honour per-pane override in `spawn_project_panes`. |
| `src/ipc.rs` | 15–43 | Add `ClearHistory` and `SetHistoryLimit` variants. |
| `src/app/input_dispatch.rs` | dispatch switch | Two new arms. |
| `src/bin/ezpn-ctl.rs` | 108–148 | Two new subcommands. |
| `src/bin/ezpn-ctl.rs` | help text | Document new commands. |
| `src/daemon/event_loop.rs` | new helper + 2 call sites | `warn_if_over_budget` invoked on detach + on `SetHistoryLimit`. |
| `tests/property_snapshot.rs` or new `tests/scrollback.rs` | + ~150 LoC | Unit + integration tests; see §8. |
| `docs/configuration.md` | (if present) | Document `[scrollback]` table and `scrollback_lines` override. |

Approximate net: +500 / −10 LoC.

---

## 7. Migration / backwards-compat

- **Wire protocol**: unchanged (`PROTOCOL_VERSION = 1`).
- **JSON IPC**: additive variants; old daemons reject the new commands
  with the existing `invalid request: …` error path, so a new
  `ezpn-ctl` against an old daemon fails cleanly.
- **Config**: both flat and sectioned syntax accepted. The flat
  `scrollback = N` overrides `[scrollback] default_lines = M` if both
  are present (last-write-wins, matching existing parser semantics in
  `parse_config_into`).
- **Snapshots**: per-pane `scrollback_lines` is a `.ezpn.toml` concern,
  not a workspace snapshot concern. Existing v3 snapshot files load
  unchanged.
- **Daemon restart**: not required to apply config changes. New panes
  pick up `default_lines` at spawn; existing panes keep their cap until
  `set-scrollback` is invoked.

---

## 8. Test plan

### Unit tests — `src/pane.rs` / `tests/scrollback.rs`

- `clear_history_drops_scrollback_keeps_visible`: spawn a pane, write
  10 000 lines into the parser, call `clear_history()`, assert
  `screen().scrollback()` returns 0 and the *visible* rows are
  preserved.
- `set_scrollback_lines_grows_cap`: start at 1 000, raise to 50 000,
  fill, assert ringbuffer holds ~50 000.
- `set_scrollback_lines_shrinks_cap_drops_oldest`: start at 10 000,
  fill, lower to 1 000, assert visible screen survives and the new cap
  holds 1 000.
- `clear_history_within_100ms_for_typical_pane`: 80×24 with 10 000
  lines of scrollback, assert `Instant::now() - t0 < 100ms`.
- `estimated_scrollback_bytes_drops_after_clear_history`: PRD release
  criterion — assert ≥ 95 % reduction.

### Unit tests — `src/config.rs`

- `parse_scrollback_section`: assert `[scrollback]` table fills
  `default_lines`, `max_lines`, `warn_bytes`.
- `flat_scrollback_back_compat`: assert flat `scrollback = N` still
  populates `EzpnConfig::scrollback`.
- `max_lines_caps_default`: if `default_lines > max_lines`, capped on
  load.

### Unit tests — `src/project.rs`

- `pane_override_parses`: `.ezpn.toml` with `[[pane]] scrollback_lines = 50000`
  populates `ResolvedProject::scrollback_overrides`.
- `pane_override_capped_at_max`: override of 1_000_000 with global max
  100_000 ends up capped.

### Integration tests — `tests/daemon_lifecycle.rs`

- `ipc_clear_history_round_trip`: spawn daemon, drive a pane to fill
  scrollback, send `ClearHistory` over IPC, verify response, then
  `IpcRequest::List` shows the pane still alive with the visible
  screen intact.
- `ipc_set_history_limit_round_trip`: same shape; verify cap actually
  enforced after subsequent writes (write 5 000 lines into a pane
  capped at 1 000, assert ringbuffer holds ~1 000).
- `ezpn_ctl_clear_history_smoke`: shell out to `ezpn-ctl clear-history
  --pane 0`, assert exit 0 and `success` message.

### Soak / perf

- Add a microbench in `benches/scrollback.rs` (or extend
  `render_hotpaths.rs`) for `clear_history` on 80×24, 200×60, 500×120
  panes with full ringbuffers. Goal: ≤ 50 ms even at the largest size.
- 7-day soak (PRD): 4 panes running `yes | head -c $((1<<30))` plus a
  cron `ezpn-ctl clear-history --pane 0` every 10 min — assert RSS
  stays bounded.

### Manual smoke

1. Add `[scrollback] default_lines = 5000` to `~/.config/ezpn/config.toml`.
2. Start a session, run `seq 1 100000` in a pane.
3. `ezpn-ctl clear-history --pane 0`, scroll up, confirm scrollback is
   empty and the prompt is at the bottom.
4. `ezpn-ctl set-scrollback --pane 0 --lines 50000`; `seq 1 100000`
   again; scroll up, confirm exactly ~50 000 lines available.
5. Set `default_lines = 200000` in `[scrollback]`, restart, fill a
   pane, confirm a `~MB scrollback` warning is emitted on stderr once.

---

## 9. Acceptance criteria

- [ ] `EzpnConfig` has `scrollback_max_lines` and `scrollback_warn_bytes`
      fields; `[scrollback]` table parsed.
- [ ] Flat `scrollback = N` continues to work and populates
      `EzpnConfig::scrollback` (= `default_lines`).
- [ ] `Pane::clear_history()` and `Pane::set_scrollback_lines(N)`
      implemented; visible screen survives both.
- [ ] PRD: `clear-history` reduces `estimated_scrollback_bytes` by
      ≥ 95 % within 100 ms.
- [ ] `IpcRequest::ClearHistory` and `IpcRequest::SetHistoryLimit`
      added with `#[serde(rename_all = "snake_case")]`.
- [ ] `ezpn-ctl clear-history --pane N` and
      `ezpn-ctl set-scrollback --pane N --lines L` parse and dispatch
      correctly.
- [ ] `.ezpn.toml`'s `[[pane]] scrollback_lines = N` honoured at
      spawn, capped at `scrollback_max_lines`.
- [ ] Memory-budget warning fires once per pane when estimated bytes
      exceed `scrollback_warn_bytes`.
- [ ] All new code paths covered by unit tests; integration tests
      exercise the full `ezpn-ctl → daemon → pane` round trip.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] No regression in existing `cargo test`.
- [ ] PROTOCOL_VERSION unchanged.

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| `vt100::Parser` reconstruction loses ANSI state spanning the visible/scrollback boundary (e.g. an unterminated SGR run carried from scrollback into visible). | The `encode_scrollback` → `decode_scrollback` path used by snapshots already preserves visible-row formatting via `rows_formatted`. Reuse that machinery; existing snapshot tests cover the common cases (`tests/property_snapshot.rs`). |
| Per-cell heuristic (32 B) is wrong for vt100 internals; warning fires at the wrong threshold. | The warning is a *hint*, not a hard cap. Document the heuristic; users can override with `warn_bytes`. v0.11 byte-budget will replace this. |
| `clear_history` allocating + replaying a fresh parser briefly doubles RSS. | Bounded by visible screen size (`rows × cols × ~32 B` < 100 KB typical); negligible. The old parser is dropped immediately. |
| `set-scrollback --lines 0` confuses vt100 (zero-cap ringbuffer). | Validate `lines >= 0`; treat `0` as "no scrollback above visible" (same as `clear_history` + cap=0). Documented in CLI help. |
| `[[pane]]` section in `.ezpn.toml` already exists with other keys; collision with the new `scrollback_lines` key. | Additive; existing keys (`shell`, `cwd`, etc.) are unchanged. Parser test covers the multi-key case. |
| Old `ezpn-ctl` against a *new* daemon is unaffected (commands are additive). New `ezpn-ctl` against an *old* daemon fails with `invalid request: …`. | Document the version requirement in `--help` and in the v0.10 release notes. Ship binaries together. |
| `warn_if_over_budget` allocating a `HashSet` per call. | Hoist to a `&mut HashSet<usize>` owned by the event loop. Code shape shown in §4.5. |

---

## 11. Open questions

1. Should `clear-history` *also* drop the on-disk snapshot blob for the
   pane? **PRD §9 default proposal:** in-memory only; add an explicit
   `--with-snapshot` flag if a user requests it later. Confirmed: ship
   in-memory only for v0.10.
2. Should `set-scrollback --lines 0` be a hard error or a no-op
   "minimum scrollback"? **Default proposal:** treat as 0 — no
   scrollback above visible — and document.
3. Should the warning escalate to a hard cap if it fires N times?
   **Default proposal:** no; warning is the entire v0.10 surface. Hard
   cap waits for v0.11 byte-budget.
4. Should per-pane runtime overrides (`SetHistoryLimit`) persist into
   auto-saved snapshots? **Default proposal:** no for v0.10. Treating
   runtime tweaks as transient keeps snapshot schema stable; revisit
   when v0.11 adds memory budgeting.
