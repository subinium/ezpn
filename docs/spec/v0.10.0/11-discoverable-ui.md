# SPEC 11 — Discoverable UI: Contextual Key Hints + Declarative Status Bar

**Status:** Draft
**Related issue:** TBD
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** C. UX & Discoverability

## 1. Background

Two related UX gaps that share a render path:

### (a) Contextual key hints

When a user enters Prefix mode they currently get **only** a `PREFIX`
badge on the status bar (`src/daemon/render.rs:42-55`). The only way
to recall what's bound is to either (i) press `?` to summon the help
overlay, which steals the entire screen, or (ii) memorise the
`src/render.rs:980-1023` hint table baked into `draw_status_bar_full`.

Today's hints in `src/render.rs:979-1023` are already context-aware
(different list for PREFIX vs RESIZE vs COPY) but they are buried
**inside** the status bar's right edge and elide silently when the
terminal is narrow. Zellij — the modern competitor the PRD calls out —
solves this with a dedicated mode-line strip that updates the moment
mode changes.

### (b) Declarative status bar segments

`draw_status_bar_full` (`src/render.rs:896-1092`) hard-codes its
layout: pane index left, mode badge after, hints in the middle, clock
on the right. Users with a 200-line `tmux.conf` are used to
`status-left`/`status-right` with `#{?cond,a,b}` style format strings.
We don't have to ship a full DSL — the PRD explicitly defers `#()`
shell substitution to v0.11 — but we do need to let users:

- toggle the bar position (top vs bottom)
- pick which segments appear and in what order
- choose which built-ins to show (session, tabs, mode, broadcast,
  clock, pane_count, layout, cwd)

This SPEC delivers both (a) and (b) because they rebuild the same
render code path; coupling them avoids two passes over `render.rs`.

PRD release-gate quote (§6):

> Contextual key hints render < 5 ms after mode change, can be
> toggled off without restart.

## 2. Goal

1. **Key-hint mode-line.** When `InputMode` changes from `Normal` to
   `Prefix` / `CopyMode` / `ResizeMode` / `PaneSelect`, render a
   one-line strip showing the most relevant chord → action pairs for
   that mode. Toggleable via `[ui] key_hints = "auto" | "always" | "off"`
   and at runtime via `:toggle hints` (or rebound key per SPEC 09).
2. **Declarative status bar.** Replace the fixed layout in
   `draw_status_bar_full` with `[[status_bar.segment]]` array entries
   evaluated per frame. Built-ins for v0.10: `session`, `tabs`, `mode`,
   `broadcast`, `clock`, `pane_count`, `layout`, `cwd`.
3. **Cached rebuild.** Segments rebuild only on config reload or
   relevant state change; the rendered string is cached per frame so
   the per-frame cost stays in the same envelope as today's
   hard-coded path.

## 3. Non-goals

- **`#(shell-cmd)` substitution.** PRD §3 defers this to v0.11. Only
  declarative built-ins ship in v0.10. State the deferral in
  `docs/status-bar.md` and in the in-binary `--help` for the `[ui]`
  table.
- **`#{?cond,a,b}` format DSL.** Same reason.
- **Per-pane status bars.** Single global bar.
- **Multi-row status bars.** Single row + optional key-hint strip
  above it. (Tab bar already eats one row when needed; that path is
  unchanged.)
- **Re-styling the help overlay.** That stays as-is for v0.10; the
  hint strip is a *complement*, not a replacement.
- **Bundled themes for the segment palette.** Segments use the
  existing `AdaptedTheme` colour slots; no new theme fields.

## 4. Design

### 4.1 ASCII mockups

#### Today (status bar only, narrow term elides hints)

```
┌──────────────────────────────────────────────────────────────────────┐
│ pane content                                                         │
└──────────────────────────────────────────────────────────────────────┘
 Pane 1/3  PREFIX     c new-tab  n/p next/prev-tab  %/" split H/V  …  14:02
```

#### After SPEC 11 — PREFIX mode with key-hint strip enabled

```
┌──────────────────────────────────────────────────────────────────────┐
│ pane content                                                         │
└──────────────────────────────────────────────────────────────────────┘
 % split-h │ " split-v │ c new-tab │ x kill-pane │ p palette │ ? help     ← hint strip
 ezpn-default · 1/3 · PREFIX                                       14:02   ← status bar
```

#### After SPEC 11 — CopyMode (visual)

```
┌──────────────────────────────────────────────────────────────────────┐
│ pane content (selection highlighted)                                 │
└──────────────────────────────────────────────────────────────────────┘
 v select │ y copy │ / search │ n/N next/prev │ q exit                     ← hint strip
 ezpn-default · 1/3 · COPY · 42 chars                              14:02   ← status bar
```

#### After SPEC 11 — Normal mode, hint strip auto-hidden

```
┌──────────────────────────────────────────────────────────────────────┐
│ pane content                                                         │
└──────────────────────────────────────────────────────────────────────┘
 ezpn-default · 1/3 · ●broadcast                                   14:02
```

(`auto`: visible only in non-Normal modes. `always`: visible in Normal too.
`off`: never.)

### 4.2 Key-hint strip data model

The strip is a one-line, single-row render. Per-mode hints are derived
from the active `Keymaps` (SPEC 09) by selecting the **top-N most
relevant** bindings annotated with a `prio` field:

```rust
// src/keymap/action.rs (added by SPEC 09; extended here)
pub struct Action {
    pub kind: ActionKind,
    pub args: Vec<String>,
    pub hint: Option<HintMeta>,    // ← added by THIS SPEC
}

pub struct HintMeta {
    pub label: &'static str,       // e.g. "split-h", "kill-pane"
    pub prio:  u8,                 // 0 = always show; 255 = never show
}
```

Ranking: visible hints are picked by `prio` ascending until the
available row is full (same elision policy as today's
`src/render.rs:1024-1042` "fitted" loop), then sorted alphabetically
by `label` for visual stability.

```
fn build_hint_strip(mode: &InputMode, keymaps: &Keymaps, term_w: u16) -> String
```

Rebuilt only when `mode` changes (cached on the daemon-wide state
between frames). Render cost: one MoveTo + one Print per hint with
themed key/desc styling, total ~150 µs at 12 hints, well inside the
5 ms PRD gate.

### 4.3 Status-bar segment model

```rust
// src/render/status_bar.rs (new module — split out of src/render.rs)

#[derive(Clone, Debug, Deserialize)]
pub struct StatusBarConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub position: Position,                // top | bottom (default: bottom)
    #[serde(default = "default_segments")]
    pub segment: Vec<SegmentConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SegmentConfig {
    pub align: Align,                      // left | right
    pub content: ContentKind,              // session | tabs | mode | …
    #[serde(default)]
    pub style: SegmentStyle,
    #[serde(default)]
    pub format: Option<String>,            // strftime for clock, "{idx}/{total}" for pane_count
    #[serde(default)]
    pub separator: Option<String>,         // override the global "·" between segments
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Session, Tabs, Mode, Broadcast, Clock, PaneCount, Layout, Cwd,
}

#[derive(Clone, Copy, Debug, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Align { #[default] Left, Right }

#[derive(Clone, Copy, Debug, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Position { #[default] Bottom, Top }

#[derive(Clone, Debug, Deserialize, Default)]
pub struct SegmentStyle {
    pub fg: Option<ThemeRef>,    // accent | lbl_fg | muted_fg | warn_fg | ...
    pub bg: Option<ThemeRef>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}
```

`ThemeRef` is a string enum that names a slot from `AdaptedTheme`
(`src/theme.rs:226-249`): `accent`, `panel`, `lbl_fg`, `muted_fg`,
`warn_fg`, `status_bg`, `status_fg`, `focus_bg`, etc. We deliberately
**do not** allow raw RGB in segment style — themes are the single
source of colour, per the existing theme architecture.

### 4.4 Built-in `ContentKind` semantics

| Kind        | Renders                                                          | Updates when                              |
|-------------|------------------------------------------------------------------|-------------------------------------------|
| `session`   | session name (e.g. `ezpn-default`)                               | session attach / rename                   |
| `tabs`      | inline mini tab list `[1·dev] 2·logs 3·db` (active highlighted)  | tab add/remove/switch                     |
| `mode`      | mode badge (PREFIX, COPY, RESIZE, BROADCAST, …) — current behaviour | mode change                            |
| `broadcast` | `●broadcast` indicator only when broadcast is on                 | broadcast toggle                          |
| `clock`     | strftime format (default `%H:%M`)                                | once per minute                           |
| `pane_count`| `{idx}/{total}` (e.g. `1/3`) of active pane                       | active pane change / pane open/close      |
| `layout`    | layout name or DSL spec (e.g. `7:3/5:5` or `tiled`)              | layout change                             |
| `cwd`       | `~`-collapsed cwd of the active pane's foreground process        | every 1 s, polled                         |

For `cwd` we read `/proc/<pid>/cwd` (Linux) or use
`proc_pidpath`/`proc_pidfdinfo` (macOS) on the foreground process of
the active pane's PTY. This is the only segment that does I/O per
poll; we cap the poll rate at 1 s and skip it entirely when the
segment is not configured. Errors are silent — the segment renders
empty if cwd cannot be resolved.

### 4.5 Render path

`draw_status_bar_full` becomes a thin shim:

```rust
// src/render.rs — old hard-coded fn becomes:
pub fn draw_status_bar_full(stdout: &mut impl Write, ctx: &StatusCtx, cfg: &StatusBarConfig)
    -> anyhow::Result<()>
{
    let cached = ctx.frame_cache.borrow();
    if cached.signature == ctx.signature() {
        return write_cached(stdout, &cached.bytes);
    }
    drop(cached);
    let bytes = render_segments(ctx, cfg)?;
    *ctx.frame_cache.borrow_mut() = FrameCache {
        signature: ctx.signature(),
        bytes: bytes.clone(),
    };
    write_cached(stdout, &bytes)
}
```

`signature()` is a tiny tuple of `(active_pane_id, mode_label,
selection_chars, broadcast, term_w, current_minute_for_clock,
config_version)`. When nothing visible changed we replay the cached
bytes verbatim — same throughput as today.

The key-hint strip lives one row above the status bar (when bar is
at bottom) or one row below (when at top). When `tab_bar` is also
visible, the order from bottom is:

```
[ panes / content ]
[ key-hint strip ]                ← optional, this SPEC
[ tab bar      ]                  ← when > 1 tab and show_tab_bar
[ status bar   ]
```

The inner-rect calculation in
`crate::app::render_ctl::make_inner` (referenced from
`src/daemon/keys.rs:337`) and in
`render::build_border_cache_with_style` (`src/render.rs:255-322`) needs
to subtract the extra row when the strip is visible. Implementation
note: existing helper `make_inner(tw, th, settings.show_status_bar)`
becomes `make_inner(tw, th, &chrome)` where `chrome` is a small struct
holding `{show_status_bar, show_tab_bar, show_hint_strip}`.

### 4.6 Toggle command

A new action string `toggle hints` is added to SPEC 09's vocabulary.
Default binding: none. Users can bind it from `[keymap.prefix]`:

```toml
[keymap.prefix]
"H" = "toggle hints"
```

It cycles through `auto → always → off → auto`. Current state surfaced
in the settings panel (SPEC tracking / `Settings` struct).

### 4.7 Caching and per-frame cost

| Step                          | Cost (warm) | Cost (cold)            | When                         |
|-------------------------------|-------------|------------------------|------------------------------|
| `build_hint_strip`            | 0 (cached)  | ~150 µs                | mode change                  |
| `render_segments`             | 0 (cached)  | ~100–300 µs (8 segs)   | signature change             |
| `write_cached` to byte buffer | ~5 µs       | n/a                    | every frame                  |
| `cwd` syscall (when enabled)  | 0           | ~50 µs (one read)      | once per second              |

Total cold-path budget for both strips: < 500 µs. Well inside the PRD
5 ms gate.

### 4.8 Why bundle (a) and (b) into one SPEC

- They both rewrite `src/render.rs:882-1092` (`draw_status_bar` /
  `draw_status_bar_full`).
- They both depend on the new chrome row-height calculation.
- The hint strip's mode-aware data is naturally derived from SPEC 09's
  `Keymaps` map, so introducing both at once avoids carrying two
  partially-keymap-aware code paths.
- Splitting them adds a merge-conflict surface in the same 200 lines
  of code with no UX or release benefit.

## 5. Surface changes

### Config (TOML)

```toml
[ui]
# auto = visible only in non-Normal modes
# always = visible in every mode (including Normal)
# off = never render the strip
key_hints = "auto"

# Position of the key-hint strip relative to the status bar.
# Default: above_status (i.e. just above the status bar).
key_hints_position = "above_status"

[status_bar]
enabled = true
position = "bottom"        # or "top"
# Single character used between segments unless a segment overrides it.
separator = " · "

[[status_bar.segment]]
align = "left"
content = "session"
style = { fg = "accent", bg = "status_bg", bold = true }

[[status_bar.segment]]
align = "left"
content = "pane_count"
format = "{idx}/{total}"   # default
style = { fg = "lbl_fg", bg = "status_bg" }

[[status_bar.segment]]
align = "left"
content = "mode"
style = { fg = "warn_fg", bg = "focus_bg", bold = true }

[[status_bar.segment]]
align = "left"
content = "broadcast"      # only renders when broadcast = on
style = { fg = "broadcast_color", bold = true }

[[status_bar.segment]]
align = "right"
content = "clock"
format = "%H:%M"           # strftime
style = { fg = "hint_fg", bg = "status_bg" }
```

If the user omits `[status_bar]` entirely, the v0.10 defaults
reproduce the v0.9 layout segment-for-segment so behaviour is
unchanged.

### CLI

No new CLI subcommands. New action available via the `:` palette and
SPEC 10 fuzzy palette:

```
:toggle hints
```

### Keybindings (default)

| Chord               | Action                       |
|---------------------|------------------------------|
| (none by default)   | `toggle hints`               |

Suggested binding for users:

```toml
[keymap.prefix]
"H" = "toggle hints"
```

## 6. Touchpoints

| File                              | Lines       | Change |
|-----------------------------------|-------------|--------|
| `src/render.rs`                   | 882-1092    | Split `draw_status_bar_full` into a thin shim + new `render_segments`; move hint table out. |
| `src/render.rs`                   | 979-1023    | Delete the static per-mode hint match (now derived from `Keymaps` per SPEC 09). |
| `src/render/status_bar.rs`        | new         | Segment model, `render_segments`, frame cache. |
| `src/render/hint_strip.rs`        | new         | `draw_hint_strip` + `build_hint_strip(mode, keymaps, w)`. |
| `src/keymap/action.rs`            | added by SPEC 09 | Add `HintMeta { label, prio }` to each `Action`. |
| `src/daemon/render.rs`            | 117-145     | Insert hint-strip render between pane render and status bar; route through `render_segments`. |
| `src/app/render_ctl.rs` (referenced from `src/daemon/keys.rs:337`) | — | `make_inner` takes a `Chrome` struct so it can subtract the optional hint-strip row. |
| `src/config.rs`                   | post-SPEC 09 toml migration | Add `[ui] key_hints*` and `[status_bar]` sections to the deserialise struct. |
| `src/settings.rs`                 | —           | Toggle field for `key_hints` exposed in the settings panel. |
| `assets/keymaps/default.toml`     | —           | (No new default binding for `toggle hints`; ships as a documented opt-in.) |
| `docs/status-bar.md`              | new         | Reference: every `ContentKind`, every theme slot, deferral notice for `#()`. |

## 7. Migration / backwards-compat

- Default config has identical visual output to v0.9: `key_hints =
  "auto"` shows the strip on mode change (new behaviour, but
  additive — no removal of existing chrome). Users who dislike it set
  `key_hints = "off"`.
- Today's hard-coded right-side hints inside the status bar
  (`src/render.rs:979-1042`) are removed in favour of the strip. To
  keep the bar uncluttered we explicitly **do not** also render
  hints inside the status bar; the strip subsumes them. Document
  this in `docs/status-bar.md`.
- The `[status_bar]` section is fully optional. Without it, defaults
  reconstruct v0.9 layout (session-equivalent left, mode badge after,
  clock right).
- Per-segment `format` only honours `%`-strftime tokens for the
  `clock` segment and `{idx}`/`{total}` for `pane_count`. Other
  segments ignore `format` (validated by `ezpn config check` from
  SPEC 09 — unknown placeholders → error).

## 8. Test plan

Unit tests:

- `build_hint_strip(InputMode::Prefix, default_keymaps, 80)` returns the top-N hints in priority order.
- `build_hint_strip` with a 30-col-wide terminal elides low-priority hints first.
- `render_segments` with the default segment set renders a row whose visible bytes match a snapshot of the v0.9 layout (regression guard).
- `signature` collisions: changing only `selection_chars` produces a different signature and busts the cache.
- `cwd` segment with a missing PID returns empty string, no panic.
- Toggling `key_hints` mid-frame updates the chrome calculation and the next frame doesn't render a stray row.

Integration tests:

- Open a session with `key_hints = "auto"`, observe no strip in Normal, then `prefix` and observe the strip appears within one frame.
- `:toggle hints` cycles `auto → always → off → auto` and the next render reflects each state.
- Custom `[status_bar]` with all 8 `ContentKind`s renders without truncation at 120 cols and degrades by dropping right-aligned segments first at 60 cols.
- Setting `position = "top"` flips the chrome: status bar at row 0, panes start at row 1 (or 2 if hint strip enabled at top).

Performance gates (PRD §6: "Contextual key hints render < 5 ms after mode change"):

- Microbench: time from `mode = Prefix` mutation to frame ready in render buffer; assert median < 5 ms over 100 transitions.
- Microbench: cached-frame `write_cached` < 50 µs.

Manual:

- Start the daemon with the example config from §5; cycle through every mode and observe the strip text + status segments update correctly.
- Resize terminal from 200 → 60 cols; segments elide right-to-left; strip elides lowest-priority hints first.

## 9. Acceptance criteria

- [ ] `[ui] key_hints = "auto" | "always" | "off"` honoured at startup and on hot-reload.
- [ ] Key-hint strip renders the relevant chord → action pairs for `Prefix`, `CopyMode`, `ResizeMode`, `PaneSelect`.
- [ ] Hints derive from the active `Keymaps` (SPEC 09), not a hard-coded table.
- [ ] `toggle hints` action cycles through the three states without restart.
- [ ] `[status_bar]` table replaces the fixed layout; supports `enabled`, `position`, `separator`, plus `[[status_bar.segment]]` array.
- [ ] All eight built-in `ContentKind`s render: `session`, `tabs`, `mode`, `broadcast`, `clock`, `pane_count`, `layout`, `cwd`.
- [ ] Per-frame status-bar cache keyed on a stable `signature()`; cached path runs in < 50 µs.
- [ ] Hint strip first paint after mode change < 5 ms (instrumented).
- [ ] `cwd` segment never blocks the render loop; degrades silently if the kernel call fails.
- [ ] Theme references in `style` resolve against `AdaptedTheme` slots only — no raw RGB.
- [ ] Default config (no `[status_bar]`, no `[ui]`) reproduces v0.9 status-bar appearance segment-for-segment.
- [ ] `docs/status-bar.md` documents every `ContentKind`, every theme slot, and the v0.11 `#()` deferral.

## 10. Risks

| Risk | Mitigation |
|---|---|
| Adding a row to the chrome breaks pane geometry maths in `make_inner` and friends; off-by-one errors are easy. | Introduce a `Chrome` struct so all chrome maths goes through one constructor; unit-test inner-rect for every combination of `(status_bar, tab_bar, hint_strip, position)`. |
| `cwd` polling cost on macOS where `proc_pidpath` requires a syscall per poll. | Cap rate at 1 s, only poll when segment configured, skip if it errored last time (back-off). |
| Users add `[status_bar]` and the order/styles render unreadably on a 16-color terminal. | Theme adapter (`src/theme.rs:206-218`) already downgrades to 16-color; segment style references never store raw RGB so they always survive the downgrade. |
| Frame-cache bug shows stale clock when minute rolls over. | `signature()` includes `current_minute_for_clock` so a new minute mints a new cache entry. |
| Hint strip visually competes with broadcast warning colour. | Reserve `theme.warn_fg` for mode badges only; strip uses `theme.lbl_fg` for keys and `theme.muted_fg` for descriptions, mirroring today's left-of-status keystroke styling (`src/render.rs:1046-1075`). |
| Users write `[status_bar]` with 30 segments; right-aligned segments always elide first, leading to "I configured a clock but it never appears". | `ezpn config check` (SPEC 09) emits a warning when the sum of minimum widths of all segments exceeds 80 cols. |

## 11. Open questions

1. **`#()` shell substitution** — PRD §3 explicitly defers this. Should we ship a stub `Custom { shell: String }` `ContentKind` in v0.10 that renders as a static placeholder so configs are forward-compatible? Default proposal: no — adding the variant invites users to depend on it before semantics are designed; track properly in v0.11 SPEC.
2. **Hint strip placement for top-bar configurations** — when `position = "top"`, should the strip go above the bar (row 0) or below (row 1)? Default proposal: directly under the bar so the chord glance is closer to the active pane border.
3. **Per-mode hint priority overrides** — should users be able to write `[hints.prefix]` to customise which actions appear in the strip? Default proposal: defer to v0.11 — for v0.10 the strip always derives from `prio` metadata on actions.
4. **`tabs` segment vs the standalone tab bar** — overlap is mostly fine (one is a compact inline view, the other a full-width row), but defaults should not show both. Default proposal: when `[[status_bar.segment]]` includes `tabs`, the standalone tab bar (`src/render.rs:1180-1254`) auto-hides unless `[ui] tab_bar = "force"` is set.
