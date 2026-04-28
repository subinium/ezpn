# Configuration reference

ezpn reads two TOML files, in order of increasing specificity:

1. **`~/.config/ezpn/config.toml`** (or `$XDG_CONFIG_HOME/ezpn/config.toml`)
   — global per-user settings.
2. **`./.ezpn.toml`** — per-project workspace file. Loaded automatically
   when `ezpn` is run with no layout arguments and the file exists in
   the current directory.

Both schemas are **frozen at v1.0**. Adding a new key is a `proto_minor`
bump; renaming or removing one is a major bump.

* Sections marked _frozen_ below cannot change in v1.x.
* Sections marked _additive_ may gain new keys in v1.x, but existing
  keys keep their semantics.

## 1. Global config — `~/.config/ezpn/config.toml`

### 1.1 `[global]` (additive)

```toml
[global]
border = "rounded"           # single | rounded | heavy | double | none
shell = "/bin/zsh"           # default $SHELL when spawning panes
scrollback = 10000           # per-pane line cap (max 100_000)
scrollback_bytes = "32M"     # byte budget; 0 disables. integer or "32M"/"512K"/"2G"
scrollback_eviction = "oldest_line"  # oldest_line | largest_line
status_bar = true
tab_bar = true
```

| Key                   | Type                | Default          | Notes |
|-----------------------|---------------------|------------------|-------|
| `border`              | string              | `"rounded"`      | Border style. |
| `shell`               | string              | `$SHELL` or `/bin/sh` | Default shell for new panes. |
| `scrollback`          | integer ≥ 0         | `10000`          | Per-pane line cap. Clamped to 100 000. |
| `scrollback_bytes`    | integer or string   | `"32M"` (32 MiB) | Byte cap. Suffixes: `K`/`KB`/`KiB` and `M`/`MB`/`MiB` and `G`/`GB`/`GiB` (all map to powers of 1024). `0` disables. Hard cap `4 GiB`. |
| `scrollback_eviction` | string              | `"oldest_line"`  | Policy when `scrollback_bytes` is exceeded. |
| `status_bar`          | bool                | `true`           | Show bottom status bar. |
| `tab_bar`             | bool                | `true`           | Show top tab bar. |

### 1.2 `[keys]` (frozen)

```toml
[keys]
prefix = "b"   # ASCII letter; the prefix is Ctrl+<letter>. Default: b.
```

| Key      | Type   | Default | Notes |
|----------|--------|---------|-------|
| `prefix` | string | `"b"`   | First character (lowercased) is the prefix letter. |

### 1.3 `[clipboard]` (frozen)

```toml
[clipboard]
osc52_set = "confirm"        # allow | confirm | deny
osc52_get = "deny"           # allow | deny
osc52_max_bytes = 1048576    # hard cap on clipboard payload. Capped at 16 MiB.
```

| Key                | Type    | Default     | Notes |
|--------------------|---------|-------------|-------|
| `osc52_set`        | string  | `"confirm"` | Per-pane confirm prompt for OSC 52 set. See [`clipboard.md`](./clipboard.md). |
| `osc52_get`        | string  | `"deny"`    | Read is the dominant attack vector. |
| `osc52_max_bytes`  | integer | `1048576`   | Drop sequences whose payload exceeds this. Hard ceiling at 16 MiB. |

### 1.4 `[[hooks]]` (frozen — issue #83)

Declarative shell-out on lifecycle events. **`exec` is always an array**;
no shell-string form. Variable substitution is single-shot (no
re-tokenisation), so payload values that contain whitespace or shell
metacharacters cannot break out of the argv element they appear in.

```toml
[[hooks]]
event = "after_pane_exit"
exec  = ["sh", "-c", "echo '${pane.command} exited code=${pane.exit_code}' >> ~/.ezpn-pane.log"]

[[hooks]]
event = "on_cwd_change"
exec  = ["notify-send", "ezpn", "cwd → ${pane.cwd}"]
when  = "session.name == 'work'"
```

| Field   | Type     | Required | Notes |
|---------|----------|----------|-------|
| `event` | string   | yes      | One of the names below. |
| `exec`  | string[] | yes      | argv. `exec[0]` is the program; `${var.path}` placeholders are substituted at fire time without re-tokenising. |
| `when`  | string   | no       | Predicate evaluated against the payload. Drops the hook when false. |

**Frozen event vocabulary** (see [`src/hooks.rs`](../src/hooks.rs)):

* `after_session_create`
* `before_attach` / `after_attach`
* `before_detach` / `after_detach`
* `after_pane_spawn` / `after_pane_exit`
* `on_cwd_change`
* `on_focus_change`
* `on_config_reload`
* `before_session_destroy`

**Operational notes**:

* 5 s wall-clock timeout per hook child; overruns get `SIGKILL`.
* Output captured under `$XDG_STATE_HOME/ezpn/hooks/<event>-<unix>.log`,
  rotated FIFO at 1 MB per file.
* Hooks are best-effort and cannot abort the triggering action.
* The daemon's env is inherited unchanged; hooks cannot inject env vars.

### 1.5 `[keymap.<table>]` (frozen vocabulary — issue #84)

Three tables in v1: `prefix`, `normal`, `copy_mode`. Defaults ship in
[`assets/default-keymap.toml`](../assets/default-keymap.toml). User
tables **merge** on top of the defaults; setting `clear = true` in a
table drops every default first.

```toml
[keymap.prefix]
"|" = "split-window-h"   # override default %
"_" = "split-window-v"
"k" = "kill-pane"

[keymap.normal]
"M-Tab" = "next-window"
clear = false            # default — keep built-ins

[keymap.copy_mode]
clear = true             # nuke every default for this table
"y"     = "copy-selection-and-cancel"
"Enter" = "copy-selection-and-cancel"
"q"     = "cancel"
```

**Key syntax**:

* Modifiers: `C-` (Ctrl), `M-` (Alt/Meta), `S-` (Shift). Combinable in
  any order (`C-M-Right`, `M-C-Right`).
* Named keys: `Enter`, `Esc` (alias `Escape`), `Tab`, `Backspace`,
  `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`,
  `Left`, `Right`, `Space`, `F1`..`F12`.
* Single character: literal key. `A` and `a` collapse to lower case
  unless paired with `S-`.

**Frozen action vocabulary** (see [`src/keymap.rs`](../src/keymap.rs)):

```
split-window-h, split-window-v, kill-pane,
new-window [-n NAME], rename-window, kill-window, select-window N,
next-window, previous-window,
select-pane (up|down|left|right),
resize-pane (up|down|left|right) N,
swap-pane (up|down),
equalize,
select-layout NAME,
detach-session, kill-session,
copy-mode, cancel, begin-selection, copy-selection-and-cancel,
reload-config, command-prompt, toggle-settings, toggle-broadcast,
display-message TEXT,
set-option KEY VALUE
```

Unknown actions are rejected at load time with a structured error
pointing into the offending TOML — the daemon refuses to start.

## 2. Project config — `./.ezpn.toml`

### 2.1 `[workspace]` (frozen)

Either ratio spec or grid spec; not both.

```toml
[workspace]
layout = "7:3/1:1"   # ratio spec: outer split | inner split
# rows = 2
# cols = 3            # grid spec: rows × cols of equal panes
```

### 2.2 `[[pane]]` (frozen)

One block per pane, in layout order.

```toml
[[pane]]
command = "cargo watch -x test"
cwd     = "./backend"
name    = "tests"
shell   = "/bin/zsh"
restart = "on_failure"      # never | on_failure | always
env     = { RUST_LOG = "debug", DATABASE_URL = "${secret:DEV_DB_URL}" }
```

| Field     | Type    | Required | Notes |
|-----------|---------|----------|-------|
| `command` | string  | no       | Command to spawn. Defaults to the resolved shell. |
| `cwd`     | string  | no       | Initial working directory. Resolved relative to the project root. |
| `name`    | string  | no       | Pane label (used in tab titles, status bar). |
| `shell`   | string  | no       | Override `[global].shell` for this pane. |
| `restart` | string  | no       | `never` (default), `on_failure`, `always`. |
| `env`     | table   | no       | Per-pane env. Values support `${VAR}` (process env) and `${secret:KEY}` (gated, see §2.3). |

### 2.3 Variable interpolation (frozen — `crate::env_interp`)

Pane-scope strings (`command`, `cwd`, `name`, `shell`, `env` values) go
through a layered interpolation context:

```
per-pane env > .env.local > secrets > process env
```

Two prefixes are recognised:

* `${VAR}` — resolved against the layered context. Missing keys are an
  error at load time.
* `${secret:KEY}` — resolved only against `~/.config/ezpn/secrets.toml`
  (or `$XDG_CONFIG_HOME/ezpn/secrets.toml`). Secret values are tagged
  `Redacted` internally — they never leak through `Debug` formatting,
  log lines, or the snapshot file.

`.env.local` (if present at the project root) is parsed as a flat
`KEY=VALUE` file and contributes to the layered context.

### 2.4 `[[hooks]]` (frozen)

Project-level hooks have the same schema as global hooks (§1.4) and are
**merged** with the global set at server boot. There is no override
semantic — both fire.

## 3. Hot-reload

* `Ctrl+B r` (default `prefix.r`) and `SIGHUP` both trigger
  `reload-config`.
* Reload re-reads `~/.config/ezpn/config.toml`. The keymap, hooks,
  clipboard policy, and border style apply immediately. `shell` and
  `scrollback*` apply only to newly spawned panes.
* `.ezpn.toml` is **not** re-read on hot-reload — restart `ezpn` for
  workspace changes.

## 4. Loading order and precedence

```
defaults
  ⊕ ~/.config/ezpn/config.toml [global]
  ⊕ ~/.config/ezpn/config.toml [keys] [clipboard] [keymap.*]
  ⊕ CLI flags (-b, -l, -e, -S, …)
```

CLI flags always win. Project `.ezpn.toml` only contributes
`[workspace]`, `[[pane]]`, and `[[hooks]]` — it does not override
`[global]` or `[keys]`.

## 5. Errors

* TOML syntax errors print `config: <path>: malformed TOML at line N,
  column M: <message>` and the daemon falls back to defaults for the
  affected section.
* Unknown keys print a warning with line/column and are ignored.
* Unknown actions in `[keymap.*]` are a hard error — the daemon refuses
  to start.
* Unknown events in `[[hooks]]` print a structured warning; the
  offending entry is dropped, others continue.

## 6. Inspecting the live config

```sh
ezpn-ctl config show               # entire effective config
ezpn-ctl config show --keys keymap # one section
ezpn-ctl --json config show        # JSON for scripting
```

(See [`scripting.md`](./scripting.md) for the `--json` schema.)
