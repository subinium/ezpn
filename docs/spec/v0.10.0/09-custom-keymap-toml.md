# SPEC 09 — Custom Keymap in TOML

**Status:** Draft
**Related issue:** TBD
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** C. UX & Discoverability

## 1. Background

Today every keybinding ezpn responds to is hard-coded inside one ~625-line
`process_key` dispatch tree (`src/daemon/keys.rs:27-655`). The function
opens with this comment, which the maintainer left in place:

> `process_key` is a single ~625-line dispatch tree because every mode
> (Normal / Prefix / Resize / CopyMode / RenameTab / CommandPalette / etc.)
> lives behind one entry point.

There are six places (Normal, Prefix, ResizeMode, CopyMode, PaneSelect,
plus the three confirmation/text-input modes) where the user's chord
choice is baked in. A power user with a 200-line `tmux.conf` cannot
move ezpn off `Ctrl-B %` for horizontal split, cannot bind `prefix h/j/k/l`
for vi-style navigation, and cannot replace the `prefix [` copy-mode
trigger with `prefix Esc`. The PRD lists this as the headline customise
gap and ranks it second only to discoverability:

> **Power user with a 200-line tmux.conf** → Custom keymap in TOML;
> hooks for workflow automation. (PRD §4)

tmux's own keymap is rebindable (`bind-key`, `unbind-key`); the absence
of an equivalent in ezpn forces every user to monkey-patch the binary.
This SPEC adds a TOML-driven keymap that preserves the existing
defaults bit-for-bit while letting users override any chord without
recompiling.

The schema is shared with [SPEC 06 — `send-keys`](./06-send-keys.md):
both need a single canonical "key spec" string format. Keep parsers in
one module so a change to `C-l` parsing flows through to both.

## 2. Goal

A first-class `[keymap.*]` section in `~/.config/ezpn/config.toml`
that:

1. Replaces the hard-coded match arms in `src/daemon/keys.rs` with a
   `HashMap<KeySpec, Action>` lookup per `InputMode`.
2. Uses the **same action vocabulary** as the existing `:` command
   palette (see `src/daemon/dispatch.rs:147-258`), so a single
   "command grammar" covers both palette execution and key dispatch.
3. Validates at startup and on hot-reload (`prefix r`); on parse
   error, the previous keymap is kept and the user is shown a one-line
   error overlay rather than a daemon crash.
4. Ships a default keymap as a TOML asset (`assets/keymaps/default.toml`)
   so the binary's behaviour is identical for users who never write a
   `[keymap.*]` section.
5. Adds `ezpn config check` so users can validate without restarting
   the daemon.

## 3. Non-goals

- **Not** a full plugin DSL — actions are strings drawn from a closed
  vocabulary, not arbitrary shell. (Hooks are SPEC 08.)
- **Not** mode creation — users can rebind keys inside the existing
  `InputMode` set; they cannot define new modes. (Custom modes deferred
  to v0.11.)
- **Not** chord sequences longer than `prefix + key` — `prefix x x`
  style two-key sequences are out of scope. The `Prefix` table allows
  exactly one follow-up keystroke, matching today's behaviour.
- **Not** per-pane or per-tab keymaps — global only.
- **Not** mouse rebinding — handled by SPEC 11 / mouse settings.

## 4. Design

### 4.1 KeySpec parser (shared with SPEC 06)

A `KeySpec` is the canonical string form of a key event. Grammar:

```
keyspec := modifier* key
modifier := "C-" | "M-" | "S-"            // Ctrl / Meta(Alt) / Shift
key := named | char
named := "Enter" | "Esc" | "Tab" | "Space" | "Backspace"
       | "Up" | "Down" | "Left" | "Right"
       | "PageUp" | "PageDown" | "Home" | "End"
       | "F1" .. "F12"
char := <single Unicode scalar value, e.g. "%", '"', "a">
```

Examples:

| String   | Meaning                              |
|----------|--------------------------------------|
| `"%"`    | bare `%`                             |
| `"\""`   | bare `"` (escape inside TOML string) |
| `"c"`    | bare `c`                             |
| `"C-l"`  | Ctrl-l                               |
| `"M-Left"` | Alt-Left                            |
| `"S-Tab"`| Shift-Tab                            |
| `"C-S-p"`| Ctrl-Shift-p                         |

Modifier order is normalised (`C` < `M` < `S`) on parse, so `S-C-p`
and `C-S-p` resolve to the same `KeySpec`. Comparison is case-sensitive
on `key`: `"a"` and `"A"` are different bindings (the latter is `S-a`
in compact form and gets normalised to `S-a` on load).

```rust
// src/keymap/spec.rs (new)
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KeySpec {
    pub mods: KeyMods,   // bitfield: ctrl|alt|shift
    pub code: KeyCode,   // re-uses crossterm::event::KeyCode
}

impl KeySpec {
    pub fn parse(s: &str) -> Result<Self, KeySpecError> { … }
    pub fn from_event(e: &KeyEvent) -> Self { … }
    pub fn to_string(&self) -> String { … }   // canonical form
}
```

`KeySpec::from_event` is the lookup-side glue: every keystroke at
runtime is converted to a `KeySpec` and looked up in the active table.

### 4.2 Action grammar (shared with `:` command palette)

Actions are written as strings using the same vocabulary the
command palette already accepts (`src/daemon/dispatch.rs:163-253`).
The full v0.10 vocabulary, with first-token aliases:

| Action string                  | What it does                      | Source today                |
|--------------------------------|-----------------------------------|-----------------------------|
| `split-window` / `split` / `split horizontal` | Split active pane horizontally | dispatch.rs:164-183 |
| `split-window -v` / `split vertical` | Split vertically            | dispatch.rs:165-169         |
| `new-window` / `new-tab`       | Create a new tab                  | dispatch.rs:184             |
| `next-window` / `next-tab`     | Cycle to next tab                 | dispatch.rs:187             |
| `prev-window` / `prev-tab` / `previous-window` | Cycle to prev tab | dispatch.rs:190         |
| `kill-pane` / `close-pane`     | Close active pane                 | dispatch.rs:193             |
| `kill-window` / `close-tab`    | Close active tab                  | dispatch.rs:200             |
| `rename-window NAME` / `rename-tab NAME` | Rename tab to NAME      | dispatch.rs:203             |
| `select-layout SPEC` / `layout SPEC` | Apply a layout DSL spec     | dispatch.rs:210             |
| `equalize` / `even`            | Equalise pane sizes               | dispatch.rs:232             |
| `zoom`                         | Toggle zoom on active pane        | dispatch.rs:238             |
| `broadcast`                    | Toggle broadcast mode             | dispatch.rs:249             |

New action strings introduced by this SPEC and SPEC 06 / 10 / 11:

| Action string                  | Added by      | What it does                      |
|--------------------------------|---------------|-----------------------------------|
| `select-pane DIR`              | this SPEC     | DIR ∈ `up|down|left|right|next|prev|last` |
| `select-pane -t N`             | this SPEC     | Jump to pane index `N`            |
| `enter-mode MODE`              | this SPEC     | MODE ∈ `prefix|copy|resize|pane-select|help` |
| `leave-mode`                   | this SPEC     | Exit any non-Normal mode          |
| `swap-pane DIR`                | this SPEC     | DIR ∈ `prev|next`                 |
| `detach`                       | this SPEC     | Detach client                     |
| `kill-session`                 | this SPEC     | Kill the whole session            |
| `reload-config`                | this SPEC     | Re-read config.toml               |
| `toggle status-bar`            | this SPEC     | Toggle the status bar             |
| `send-keys KEYS…`              | SPEC 06       | Inject KEYS into active pane      |
| `command-palette`              | SPEC 10       | Open the fuzzy command palette    |
| `toggle hints`                 | SPEC 11       | Toggle key-hint strip             |

Unknown actions are a hard error at config-load time:

```text
$ ezpn config check
error: keymap.prefix["%"] = "split-werindow"
       unknown action `split-werindow`; did you mean `split-window`?
       (run `ezpn config check --list-actions` for the full list)
```

### 4.3 Tables match `InputMode`

A separate `[keymap.NAME]` table per active mode, where `NAME` matches
the `InputMode` variants in `src/daemon/state.rs:19-39`:

| TOML table          | InputMode               | Today's hard-coded source       |
|---------------------|-------------------------|---------------------------------|
| `[keymap.root]`     | `Normal`                | keys.rs:512-654                 |
| `[keymap.prefix]`   | `Prefix { .. }`         | keys.rs:296-510                 |
| `[keymap.copy_mode]`| `CopyMode(_)`           | keys.rs:231-294 + copy_mode.rs  |
| `[keymap.resize_mode]` | `ResizeMode`         | keys.rs:193-229                 |
| `[keymap.pane_select]` | `PaneSelect`         | keys.rs:106-124                 |

The text-input modes (`RenameTab`, `CommandPalette`, `QuitConfirm`,
`CloseConfirm`, `CloseTabConfirm`, `HelpOverlay`) keep their current
bespoke handling. Letting users rebind `Enter`-confirmation in
`QuitConfirm` is a foot-gun with no upside.

### 4.4 Resolution and layering

```
defaults  →  user TOML  →  active keymap
(ship in    (~/.config/   (HashMap loaded into
 binary)    ezpn/         daemon at startup)
            config.toml)
```

For each `[keymap.NAME]` table:

1. Start from the default table (loaded from
   `assets/keymaps/default.toml` via `include_str!`).
2. For each `key = action` line in the user TOML, **replace** the
   default. (No "extend"; no partial merge.)
3. To **unbind** a key, set it to the empty string: `"x" = ""`.
4. To **wipe** a whole table and start from scratch, set
   `[keymap.prefix]` and `clear = true` at the top of that section.
   (Optional, deferred unless requested.)

Built-in defaults live in `assets/keymaps/default.toml` shipped via
`include_str!("../assets/keymaps/default.toml")`, mirroring the way
themes already work in `src/theme.rs:288-292`.

### 4.5 Loading and hot-reload

Today `prefix r` calls `config::load_config()` and applies the result
(`src/daemon/keys.rs:396-408`). We extend that path:

```rust
// src/daemon/state.rs (new fields on the daemon-wide State)
pub struct Keymaps {
    pub root: HashMap<KeySpec, Action>,
    pub prefix: HashMap<KeySpec, Action>,
    pub copy_mode: HashMap<KeySpec, Action>,
    pub resize_mode: HashMap<KeySpec, Action>,
    pub pane_select: HashMap<KeySpec, Action>,
}

impl Keymaps {
    pub fn defaults() -> Self { … }                    // from baked-in TOML
    pub fn try_load(path: &Path) -> Result<Self, …> { … }
    pub fn lookup(&self, mode: &InputMode, ks: KeySpec) -> Option<&Action> { … }
}
```

Hot-reload semantics on `prefix r`:

```text
load user TOML
  ├─ ok  → swap in new Keymaps; toast "keymap reloaded (N bindings)"
  └─ err → keep old Keymaps; toast "keymap parse error at line N: …"
```

The toast is rendered via the same overlay path as the help screen
(`render::draw_text_input` style); duration ~3 s.

### 4.6 Replacing the `process_key` dispatch tree

The 625-line match becomes a much smaller "convert event → KeySpec →
lookup → execute action" loop. The mode-specific handlers (text input,
copy mode's vi state machine, etc.) stay; only the chord-to-behaviour
arms move out.

Sketch:

```rust
// src/daemon/keys.rs (post-refactor, illustrative)
let ks = KeySpec::from_event(&key);

match mode {
    InputMode::Normal => {
        if let Some(action) = keymaps.root.get(&ks) {
            execute_action(action, ctx);
            return;
        }
        // fall through: pane stdin
        forward_to_pane(key, ctx);
    }
    InputMode::Prefix { .. } => {
        if let Some(action) = keymaps.prefix.get(&ks) {
            execute_action(action, ctx);
        }
        *mode = InputMode::Normal;
    }
    InputMode::CopyMode(_) | InputMode::ResizeMode | InputMode::PaneSelect => { … }
    // text-input / confirm modes unchanged
    _ => { … }
}
```

`execute_action` is the same function the command palette already
uses, renamed and lifted out of `dispatch.rs`. SPEC 10's palette calls
it too. One function, one truth.

### 4.7 TOML parsing strategy

`src/config.rs:59-106` is currently a hand-written line parser with
no `[section]` support. Three options:

| Option | Effort | Risk | Notes |
|---|---|---|---|
| (a) Extend the hand-written parser to handle `[keymap.NAME]` sections and quoted keys | Medium | Medium — `"\""`, `'"'`, multiline strings get awkward fast | Keeps zero-dep promise but the `[keymap.*]` schema is genuinely a TOML use case (quoted keys, escape sequences). |
| (b) Switch the whole loader to the `toml` crate already in `Cargo.toml` | Low | Low — `toml = "0.8"` is **already a dep**, used by the theme loader (`src/theme.rs:331-352`). | Wins for free. Slightly more code in the existing-key path. |
| (c) Hybrid — keep the line parser for legacy globals, parse `[keymap.*]` sections separately with `toml` | High | High — two parsers diverge over time | Worst of both. |

**Decision: (b).** `toml = "0.8"` is already pulled in for theme
parsing; depending on it again is free, eliminates a class of
escape-handling bugs (every `[keymap.prefix]` value lives or dies on
quoting), and lets us delete `parse_config_into` in favour of a
`#[derive(Deserialize)]` struct. The legacy `key = value` lines stay
working because TOML is a superset of "bare key = value" — every
existing config keeps parsing without change.

State the deviation explicitly in the changelog: ezpn no longer parses
its config with hand-rolled regex; users who relied on quirks of the
old parser (e.g. `# inline comments after a value`, which TOML rejects)
get a one-line migration note.

### 4.8 CLI: `ezpn config check`

```text
USAGE: ezpn config check [PATH]

Validates the config file at PATH (default: $XDG_CONFIG_HOME/ezpn/config.toml)
and prints any errors. Exits 0 on success, non-zero on any validation error.
Does not start the daemon, does not connect to any running server.

OPTIONS:
  --list-actions    Print every valid action string and exit
  --json            Emit errors as JSON for tooling integration
```

Sample successful run:

```text
$ ezpn config check
ok  ~/.config/ezpn/config.toml (47 bindings: root=3, prefix=38, copy=4, resize=2)
```

Sample failure (matches behaviour of `cargo check`-style tooling):

```text
$ ezpn config check
~/.config/ezpn/config.toml:24: keymap.prefix["C-Foo"]
  error: unknown key `Foo` (after `C-`)
  help:  named keys are Enter|Esc|Tab|Space|Backspace|Up|Down|Left|Right|
                        PageUp|PageDown|Home|End|F1..F12
~/.config/ezpn/config.toml:31: keymap.prefix["%"] = "split-werindow"
  error: unknown action `split-werindow`
  help:  did you mean `split-window`?
2 errors
```

## 5. Surface changes

### Config (TOML)

```toml
# ~/.config/ezpn/config.toml — new sections; existing globals unchanged
border = "rounded"
shell = "/bin/zsh"
prefix = "b"

[keymap.prefix]
"%"   = "split-window"
'"'   = "split-window -v"
"c"   = "new-tab"
"n"   = "next-tab"
"p"   = "prev-tab"
"x"   = "kill-pane"
"&"   = "kill-window"
"d"   = "detach"
"z"   = "zoom"
"E"   = "equalize"
"R"   = "enter-mode resize"
"["   = "enter-mode copy"
"q"   = "enter-mode pane-select"
"?"   = "enter-mode help"
":"   = "enter-mode command-palette"   # legacy line-mode (kept)
"p"   = "command-palette"              # SPEC 10 fuzzy palette (default rebound)
"B"   = "broadcast"
"r"   = "reload-config"
"s"   = "toggle status-bar"
"C-l" = "select-pane right"
"C-h" = "select-pane left"
"C-k" = "select-pane up"
"C-j" = "select-pane down"

[keymap.root]
"C-d" = "split-window"
"C-e" = "split-window -v"
"F2"  = "equalize"
"M-Left"  = "select-pane left"
"M-Right" = "select-pane right"

[keymap.copy_mode]
"v" = "selection-start char"
"V" = "selection-start line"
"y" = "yank-and-exit"
"q" = "leave-mode"

[keymap.resize_mode]
"h" = "resize -L 5"
"j" = "resize -D 5"
"k" = "resize -U 5"
"l" = "resize -R 5"
"q" = "leave-mode"
```

If a user only wants to override one binding, they write only that
binding — everything else falls through to the baked-in default:

```toml
[keymap.prefix]
"|" = "split-window"        # add | as horizontal split (in addition to %)
"-" = "split-window -v"     # add - as vertical split
"%" = ""                    # unbind tmux's default %
```

### CLI

```text
ezpn config check [PATH] [--list-actions] [--json]
```

No other CLI surface changes. The runtime daemon CLI is unchanged.

### Keybindings (default)

Defaults are byte-for-byte identical to today's hard-coded behaviour.
The full default keymap is in `assets/keymaps/default.toml` (extracted
from `src/daemon/keys.rs:296-510` and `src/daemon/keys.rs:512-654`),
checked into git so users can read it as a reference.

## 6. Touchpoints

| File                              | Lines       | Change |
|-----------------------------------|-------------|--------|
| `src/daemon/keys.rs`              | 27-655      | Replace hard-coded match arms with `keymaps.lookup(mode, ks)` + `execute_action(action, ctx)`. Mode-specific state (copy mode vi machine, text-input modes) stays. Should drop file size below the 500-line target the existing comment laments. |
| `src/daemon/dispatch.rs`          | 145-258     | `execute_command` renamed `execute_action` and lifted to `src/keymap/action.rs`; both palette and key handler call it. |
| `src/keymap/mod.rs`               | new         | `Keymaps`, lookup, default loading. |
| `src/keymap/spec.rs`              | new         | `KeySpec` parser + `from_event` (shared with SPEC 06). |
| `src/keymap/action.rs`            | new         | `Action` enum + parser + `execute_action`. |
| `src/config.rs`                   | 39-118      | Migrate hand-written parser to `toml::from_str` against a `#[derive(Deserialize)]` `RawConfig` struct. Existing global keys keep parsing. |
| `src/config.rs`                   | new         | `pub fn load_keymaps() -> Result<Keymaps, …>` + `pub fn check_config(path: &Path) -> Result<Report, …>`. |
| `src/main.rs`                     | CLI parse   | Wire up `ezpn config check` subcommand. |
| `assets/keymaps/default.toml`     | new         | Baked-in default keymap; `include_str!` from `src/keymap/mod.rs`. |
| `Cargo.toml`                      | —           | No new deps. (`toml = "0.8"` already present.) |
| `docs/keymap.md`                  | new         | User-facing reference: every action string, every default binding, hot-reload semantics. |

## 7. Migration / backwards-compat

- Existing `~/.config/ezpn/config.toml` files keep working: every
  recognised global key (`border`, `shell`, `scrollback`, etc.) is
  still parsed. Users who never write a `[keymap.*]` section get
  identical behaviour to v0.9.
- The hand-written parser accepted `# inline comments` after values.
  TOML rejects them. Document this as a one-line breaking note in
  the v0.10 changelog and in `ezpn config check`'s "common errors"
  section.
- `prefix r` continues to work: it now reloads keymaps too. On parse
  error the **previous** keymap is kept; the toast surfaces the line
  number so users can `prefix : config check` (or run it externally)
  to fix.
- `assets/keymaps/default.toml` is checked into the repo so any user
  can `cat $(ezpn config path)/../keymaps/default.toml` (or read it
  on GitHub) to see what the defaults are.

## 8. Test plan

Unit tests:

- `KeySpec::parse` round-trips every default binding (`%`, `"`, `c`, `C-l`, `M-Left`, `S-Tab`, `F2`).
- Modifier-order normalisation: `parse("S-C-p") == parse("C-S-p")`.
- Unknown named key (`"C-Foo"`) returns `KeySpecError::UnknownKey`.
- `Action::parse` accepts every documented action string from §4.2; unknown action returns `ActionError::Unknown`.
- `Keymaps::defaults()` parses without error and contains exactly the bindings present in v0.9 keys.rs (snapshot test against a Vec<(KeySpec, Action)> baseline).
- User TOML overrides default: `[keymap.prefix]\n"%" = "kill-pane"` then lookup of `KeySpec("%")` returns `Action::KillPane`.
- Empty-string action removes the binding.
- Malformed TOML returns `Err`; daemon retains old Keymaps on hot-reload (integration test against a `Keymaps::try_load` failure).

Integration tests:

- `ezpn config check` exits 0 on the default config, non-zero with line numbers on a known-bad fixture.
- Spawn a daemon with a custom keymap that rebinds `prefix x` → `split-window`; `ezpn-ctl send-keys 'C-b' 'x'` (SPEC 06) results in a split, not a close.
- Hot-reload: write a bad config, send `prefix r`, assert the previous keymap is still active and the error toast text matches.

Manual:

- 30-line custom `[keymap.prefix]` exercising every action string; daemon survives a 1-hour soak with no panics.
- `ezpn config check --list-actions` output matches the action table in §4.2.

## 9. Acceptance criteria

- [ ] All five `[keymap.NAME]` tables (`root`, `prefix`, `copy_mode`, `resize_mode`, `pane_select`) parse from TOML and replace today's hard-coded dispatch.
- [ ] `assets/keymaps/default.toml` is shipped in-binary via `include_str!`.
- [ ] `prefix r` hot-reloads the keymap; on parse error the previous keymap is preserved and an error toast is shown.
- [ ] `ezpn config check` validates without starting the daemon, exits non-zero on error, prints structured line/column errors.
- [ ] `ezpn config check --list-actions` prints every valid action string.
- [ ] Default keymap is byte-for-byte equivalent to v0.9 behaviour (snapshot test).
- [ ] `KeySpec` parser is in `src/keymap/spec.rs` and re-used by SPEC 06's `send-keys`.
- [ ] `execute_action` is one function shared between `:` palette and key dispatch.
- [ ] No new crate dependencies (TOML parsing piggybacks on the existing `toml = "0.8"` dep).
- [ ] `src/daemon/keys.rs` drops below the 500-line target referenced in its leading comment.
- [ ] `docs/keymap.md` documents every action and default.

## 10. Risks

| Risk | Mitigation |
|---|---|
| `toml` crate migration accidentally changes behaviour for an existing global key. | Snapshot test: parse the current `~/.config/ezpn/config.toml.example` with both old and new parsers and compare resulting `EzpnConfig` byte-for-byte for the v0.9 corpus. |
| Users encode multi-byte characters as keys (`"가"`) that don't have crossterm `KeyCode` representation. | `KeySpec::parse` accepts any Unicode scalar but lookup-time conversion goes through `KeyEvent::code` — non-ASCII chars work the same way they do today (chars typed at the keyboard arrive as `KeyCode::Char`). Document UTF-8 support; add a non-ASCII binding to the test suite. |
| Action grammar diverges between palette and key handler over time. | One `execute_action` function in `src/keymap/action.rs`. Compile-time enforcement: palette and key handler import from the same module; no parallel match. |
| Schema gets locked in pre-1.0. | The PRD already calls this out (§7); ship under documented "experimental" disclaimer in `docs/keymap.md` for v0.10, promote to stable in 1.0. Per-section `version = 1` field reserved but not required in v0.10. |
| Hot-reload drops a keystroke between "old keymap removed" and "new keymap installed". | `Keymaps::try_load` builds the full new map first, then a single atomic swap (`std::mem::replace`). |

## 11. Open questions

1. **Two-key prefix sequences** — tmux supports `prefix x x` (kill window, double-x) and similar. Should v0.10 ship with single-keystroke prefix only, or extend to optional one-level chord? Default proposal: single-keystroke; track multi-key as v0.11.
2. **Mode-leak on rebinding** — if a user rebinds `Esc` inside `[keymap.copy_mode]` to a no-op, they lose the only escape hatch. Should we hard-code `Esc → leave-mode` as a non-overridable safety key? Default proposal: yes, with a warning printed on `ezpn config check` if the user tries.
3. **Action argument quoting** — `"C-r" = "rename-tab \"my tab\""` works in TOML but is ugly. Should we move to a list form `"C-r" = ["rename-tab", "my tab"]`? Default proposal: keep string form for v0.10 (matches palette syntax exactly); revisit when arity grows.
