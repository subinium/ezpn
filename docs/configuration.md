# Configuration

Global settings live at `$XDG_CONFIG_HOME/ezpn/config.toml`, falling back to
`~/.config/ezpn/config.toml`. Project commands live in `./.ezpn.toml` and
require `ezpn --trust-project` for automatic execution.

Configuration versioning is separate from the client/server wire protocol.
A reserved schema field is not evidence that its runtime feature is implemented.

## Global settings

```toml
[global]
border = "rounded"
shell = "/bin/sh"
scrollback = 10000
scrollback_bytes = "32M"
status_bar = true
tab_bar = true
persist_scrollback = false

[keys]
prefix = "b"

[theme]
name = "ezpn-dark"

[clipboard]
osc52_set = "confirm"
osc52_get = "deny"
osc52_max_bytes = 1048576
```

Borders: single, rounded, heavy, double, none.
Themes: ezpn-dark, ezpn-light, nord, gruvbox-dark, solarized-dark.
Mouse coordinates, PTY sizes, zoom and borders use the same content geometry.

Scrollback is capped by configured lines and a conservative per-pane allocation
budget. This is not a process RSS limit or proof of zero memory growth.
`largest_line` remains a compatibility setting; arbitrary sparse-row eviction
is not implemented by the bundled vt100 grid. See the release audit.

## Reload and settings

`Ctrl+B r` or SIGHUP validates one complete file before applying supported
settings, hooks and keymaps. Invalid input leaves the running configuration
unchanged. Fields requiring process restart are reported instead of silently
changing existing processes. A failed settings write is visible.

## Keymaps

```toml
[keymap.normal]
"F1" = "toggle-settings"

[keymap.prefix]
"r" = "reload-config"

[keymap.copy_mode]
"v" = "begin-selection"
"y" = "copy-selection-and-cancel"
"q" = "cancel"
```

`clear = true` clears defaults in that table. Modifier prefixes are C-, M-, S-.
Normal shell control keys are not stolen by undocumented split shortcuts.
The full default map is [assets/default-keymap.toml](../assets/default-keymap.toml).

## Status bar

```toml
[status_bar]
left = ["{session}", "{mode}"]
right = ["{time}"]
```

Builtin fields include session, tab_count, mode and time. Literal/key-hint
segments are bounded to display width. Custom key-hint arrays are static
cheatsheets, not automatically generated descriptions of every remapped binding.
Confirmation prompts and text input remain visible above custom layouts.

## Project settings

```toml
[workspace]
layout = "7:3"

[[pane]]
name = "shell"
cwd = "."

[[pane]]
name = "worker"
command = "printf 'ready\\n'; exec sh"
restart = "on_failure"
env = { PROJECT_ROOT = "${PWD}" }
persist_scrollback = false
```

Per-pane fields: name, command, cwd, shell, env, restart and persist_scrollback.
An explicit CLI grid/layout bypasses automatic project loading.
`on_failure` does not restart a successful exit. Restart attempts have finite
retry/backoff budgets, including failed spawns.

## Environment interpolation

Supported forms are `$VAR`, `${VAR}`, `${VAR:-default}`,
`${VAR:?message}`, `$$` for a literal dollar and `${secret:KEY}`.
Environment precedence is per-pane values, .env.local, then process environment.
Secrets are consulted only by the explicit secret form.

These are interpolation rules, not a shell evaluator. External reads mark the
whole pane as sensitive for snapshot exclusion. See [security](security.md).

## Snapshots

Live detach keeps processes running. Disk restore starts new processes and may
restore opt-in text history; it does not resurrect process memory, terminal
graphics, hyperlink metadata or exact alternate-screen state.

Limits include 64 MiB JSON/payload, 128 MiB aggregate decoded history, 200,000 rows
per history blob, 100 tabs, 1,000 panes in a snapshot and 100 panes per tab.
The current writer uses private atomic files. Oversized/corrupt snapshots fail
before replacing the running workspace.

Per-pane external-value sensitivity takes precedence over history persistence.
Readonly diagnostics never print resolved secret values.

## Hooks

```toml
[[hooks]]
event = "after_pane_exit"
exec = ["notify-send", "ezpn", "pane ${pane.id} exited"]
```

Global hooks and explicitly trusted project hooks are combined. Hooks use four
workers and a queue of 64; excess work is rejected, not buffered without limit.
Use argv instead of constructing shell source from untrusted substitutions.
The release audit states which lifecycle producers are currently wired.
