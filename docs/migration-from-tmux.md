# Migrating from tmux to ezpn

This is a key-by-key, command-by-command guide for tmux users. Read it
end-to-end once and you should be productive in ezpn within an hour.

ezpn is **not** a drop-in replacement. The default keybindings overlap
heavily, but tmux's plugin ecosystem, complex hooks, and a few rare
features (`pipe-pane`, `command-alias`) are not implemented and will
not be in v1.0.

If you want a 30-second TL;DR: defaults match tmux. Drop your
`~/.tmux.conf`. Drop tmuxinator. Read §3 if you depend on `send-keys`.

## 1. Key-for-key cheat sheet

The default prefix is `Ctrl+B`, same as tmux. Everything below assumes
default keybindings — overrides go in `[keymap.prefix]` (see
[configuration.md](./configuration.md#15-keymaptable-frozen-vocabulary--issue-84)).

### 1.1 Pane lifecycle

| Action            | tmux              | ezpn              |
|-------------------|-------------------|-------------------|
| Split horizontal  | `C-b "`           | `C-b "`           |
| Split vertical    | `C-b %`           | `C-b %`           |
| Close pane        | `C-b x`           | `C-b x`           |
| Zoom toggle       | `C-b z`           | `C-b z`           |
| Equalize          | `C-b Space`       | `C-b =` or `C-b Space` |
| Swap with prev/next | `C-b {` / `}`   | `C-b {` / `}`     |
| Pane numbers (jump) | `C-b q`         | `C-b q`           |

### 1.2 Pane navigation

| Action     | tmux                        | ezpn                          |
|------------|-----------------------------|-------------------------------|
| Up/down/left/right | `C-b ↑/↓/←/→`        | `C-b ↑/↓/←/→`                 |
| No prefix  | (requires custom binding)   | `M-↑/↓/←/→` built in          |
| Cycle      | `C-b o`                     | `C-b o`                       |

### 1.3 Window/tab lifecycle

tmux calls them "windows", ezpn calls them "tabs". Same idea.

| Action          | tmux                | ezpn                |
|-----------------|---------------------|---------------------|
| New             | `C-b c`             | `C-b c`             |
| Next / previous | `C-b n` / `p`       | `C-b n` / `p`       |
| Jump 0–9        | `C-b 0`–`9`         | `C-b 0`–`9`         |
| Rename          | `C-b ,`             | `C-b ,`             |
| Close           | `C-b &`             | `C-b &`             |

### 1.4 Sessions

| Action          | tmux                | ezpn                |
|-----------------|---------------------|---------------------|
| Detach          | `C-b d`             | `C-b d`             |
| Attach last     | `tmux a`            | `ezpn a`            |
| Attach by name  | `tmux a -t NAME`    | `ezpn a NAME`       |
| List            | `tmux ls`           | `ezpn ls`           |
| Kill session    | `tmux kill-session -t NAME` | `ezpn kill NAME` |
| New named       | `tmux new -s NAME`  | `ezpn -S NAME`      |

### 1.5 Copy mode

`C-b [` enters copy mode in both. Once inside:

| Action              | tmux (vi-mode)   | ezpn               |
|---------------------|------------------|--------------------|
| Move                | `h j k l`        | `h j k l`          |
| Word                | `w` / `b`        | `w` / `b`          |
| Line start/end      | `0` / `$`        | `0` / `$`          |
| First non-blank     | `^`              | `^`                |
| Top / bottom        | `g` / `G`        | `g` / `G`          |
| Half-page           | `C-u` / `C-d`    | `C-u` / `C-d`      |
| Begin char select   | `Space`          | `v`                |
| Begin line select   | `V`              | `V`                |
| Yank                | `y` / `Enter`    | `y` / `Enter`      |
| Search forward      | `/`              | `/`                |
| Search backward     | `?`              | `?`                |
| Next / prev match   | `n` / `N`        | `n` / `N`          |
| Cancel              | `q` / `Esc`      | `q` / `Esc`        |

**ezpn's copy mode is vi-mode by default.** tmux defaults to emacs-mode
unless you set `set-window-option -g mode-keys vi`. If you've been
running emacs-mode in tmux, expect a one-day adjustment period.

### 1.6 Command palette

`C-b :` opens the command palette in both. ezpn accepts every tmux
command alias for the verbs it supports:

| tmux command          | ezpn equivalent      |
|-----------------------|----------------------|
| `split-window`        | `split-window` / `split` |
| `split-window -v`     | `split -v`           |
| `new-window`          | `new-window` / `new-tab` |
| `next-window`         | `next-window` / `next-tab` |
| `previous-window`     | `previous-window` / `prev-tab` |
| `kill-pane`           | `kill-pane` / `close-pane` |
| `kill-window`         | `kill-window` / `close-tab` |
| `rename-window NAME`  | `rename-window NAME` / `rename-tab NAME` |
| `select-layout SPEC`  | `layout SPEC`        |
| `setw synchronize-panes` | `broadcast` (toggle) |

## 2. `.tmux.conf` → `~/.config/ezpn/config.toml`

The 30 most-common tmux settings, mapped:

| `~/.tmux.conf`                                | `~/.config/ezpn/config.toml`        |
|-----------------------------------------------|-------------------------------------|
| `set -g prefix C-a`                           | `[keys]\nprefix = "a"`              |
| `set -g default-terminal "tmux-256color"`     | n/a — ezpn always reports `xterm-256color` to children |
| `set -g default-shell /bin/zsh`               | `[global]\nshell = "/bin/zsh"`      |
| `set -g history-limit 50000`                  | `[global]\nscrollback = 50000`      |
| `set -g status-position top`                  | n/a — ezpn's status bar is bottom-only in v1 |
| `set -g status on/off`                        | `[global]\nstatus_bar = true` / `false` |
| `set -g mouse on`                             | n/a — mouse is always on            |
| `set -g escape-time 0`                        | n/a — ezpn doesn't impose an escape delay |
| `setw -g mode-keys vi`                        | n/a — ezpn copy mode is vi-mode     |
| `set -g set-clipboard on`                     | `[clipboard]\nosc52_set = "allow"`  |
| `set -g pane-border-style "fg=…"`             | `[global]\nborder = "rounded"` (style only; no per-side colour) |
| `set -g pane-border-status off`               | n/a — ezpn does not draw per-pane border titles |
| `bind | split-window -h`                      | `[keymap.prefix]\n"|" = "split-window-h"` |
| `bind - split-window -v`                      | `[keymap.prefix]\n"-" = "split-window-v"` |
| `bind r source-file ~/.tmux.conf`             | `[keymap.prefix]\n"r" = "reload-config"` (built-in default) |
| `bind h select-pane -L` (and j/k/l)           | `[keymap.prefix]\n"h" = "select-pane left"` etc. |
| `bind -n M-h select-pane -L` (no-prefix nav)  | Built-in: `M-↑/↓/←/→` and the `[keymap.normal]` table |
| `set -g status-left "#S"`                     | n/a — status bar layout is fixed in v1 |
| `set-option -g renumber-windows on`           | n/a — ezpn does this automatically  |
| `bind C-b last-window`                        | n/a in v1; on the v0.13 roadmap     |
| `set -g focus-events on`                      | n/a — focus events are always forwarded for clients that opt in via DECSET ?1004 |
| `set -g allow-rename off`                     | n/a — ezpn never rewrites tab titles from OSC 0/2 |

### 2.1 What's intentionally not portable

* **Plugins (`tmux-plugins/*`)** — no plugin system in v1.
* **`pipe-pane`** — pipe-pane to a file is not implemented; use `tee`
  in the spawned command (`ezpn -e 'cmd | tee log'`) or hooks.
* **`command-alias`** — no user-defined command aliases. The verbs are
  fixed; rebind them via `[keymap.*]` instead.
* **Complex `if-shell` config logic** — TOML is declarative; conditional
  config requires editing `~/.config/ezpn/config.toml` per host.
* **`run-shell`** — use `[[hooks]]` (see [configuration.md](./configuration.md#14-hooks-frozen--issue-83)).

## 3. tmuxinator → `.ezpn.toml`

tmuxinator's YAML is one project workspace per file. ezpn's `.ezpn.toml`
is one project workspace per repo, dropped at the project root. No CLI
to remember.

### 3.1 Side-by-side

```yaml
# ~/.tmuxinator/myapp.yml
name: myapp
root: ~/projects/myapp
windows:
  - editor:
      layout: main-vertical
      panes:
        - vim
        - guard
  - server: bundle exec rails s
  - logs: tail -f log/development.log
```

```toml
# ~/projects/myapp/.ezpn.toml
[workspace]
layout = "7:3/1:1"

[[pane]]
name    = "editor"
command = "vim"

[[pane]]
name    = "guard"
command = "guard"

[[pane]]
name    = "server"
command = "bundle exec rails s"

[[pane]]
name    = "logs"
command = "tail -f log/development.log"
restart = "on_failure"
```

Run `ezpn` in `~/projects/myapp` — the `.ezpn.toml` is loaded
automatically. No `tmuxinator start myapp`, no `mux start`.

### 3.2 Differences worth flagging

| tmuxinator                                      | ezpn                                            |
|-------------------------------------------------|-------------------------------------------------|
| Per-window layouts (`main-vertical`, etc.)       | Single workspace layout (no multi-tab in `.ezpn.toml` v1) |
| `pre`, `pre_window`, `post` hooks               | Use `[[hooks]]` events (`after_session_create`) |
| `attach: false`                                 | `ezpn -d` (start daemon, don't attach)          |
| `socket_name` / `socket_path`                    | `ezpn -S NAME` for a named socket               |
| Inheriting from `~/.tmuxinator/default.yml`     | No inheritance; copy snippets per project       |

### 3.3 Importing a Procfile

```sh
ezpn from Procfile
```

generates a `.ezpn.toml` from a Heroku-style `Procfile`. One pane per
process type, `restart = "on_failure"` by default.

## 4. Behaviour differences worth knowing

### 4.1 `send-keys` ack model (#81)

tmux's `tmux send-keys -t pane:0 'cargo test' Enter` returns
immediately. The keys are queued; you have no idea when the command
finishes.

ezpn supports the same fire-and-forget mode (`ezpn-ctl exec <pane>
'cargo test'`) **and** an `--await-prompt` mode that blocks until the
pane emits OSC 133 D:

```sh
ezpn-ctl send-keys --pane 0 --await-prompt --timeout 60s -- 'cargo test\n'
echo "tests done"
```

This requires your shell to emit OSC 133 D (snippets in
[shell-integration.md](./shell-integration.md)). If your shell doesn't,
fall back to fire-and-forget + a sentinel write.

### 4.2 OSC 52 confirm prompt (#79)

tmux 3.4 + flips its `set-clipboard` default to `external` and prompts
on the first OSC 52 set per pane. ezpn does the same by default
(`clipboard.osc52_set = "confirm"`). If you have `set -g set-clipboard
on` muscle memory in your tmux config, mirror it explicitly:

```toml
[clipboard]
osc52_set = "allow"
```

See [clipboard.md](./clipboard.md) for the full policy chain.

### 4.3 Status bar customization

tmux's `status-left` / `status-right` mini-language doesn't port. ezpn
v1 ships a fixed status-bar layout. If you depend on a custom status
bar, build it externally with `ezpn-ctl events`
(see [scripting.md](./scripting.md)) and pipe to your existing status
bar tool (`tmux-line-rs`, `polybar`, `i3blocks`).

### 4.4 No nested-prefix doubling

In tmux, `C-b C-b` sends a literal `C-b` to the inner shell. ezpn does
**not** double-tap. If you nest ezpn under another multiplexer, set the
inner ezpn's prefix to a different letter:

```toml
# inner ezpn
[keys]
prefix = "a"   # use Ctrl+A so outer Ctrl+B reaches the right multiplexer
```

### 4.5 Memory and reattach speed

ezpn's resident set size is dramatically lower than tmux's at the same
pane count and scrollback budget. Empirically (Linux 6.6, 16-pane
session, 50 MB total scrollback): tmux 3.4 sits around 180 MB RSS;
ezpn v0.12 sits around 28 MB. Reattach is single-digit milliseconds in
both. Don't read these as benchmarks against your workload — measure
your own.

## 5. Diagnostic helpers

### 5.1 "Why doesn't my keybinding work?"

```sh
ezpn-ctl --json config show | jq '.keymap'
```

shows the merged keymap (defaults ⊕ your config) — useful for
diagnosing override conflicts.

### 5.2 "Did my `.ezpn.toml` parse?"

```sh
ezpn 2>&1 | head -20
```

Parse errors print to stderr with line/column. Soft warnings ("unknown
key") print but don't abort.

### 5.3 "Is the daemon listening?"

```sh
ezpn ls       # lists running daemons
ezpn-ctl list # lists panes in the most-recent daemon
```

If `ezpn ls` shows nothing, no daemon is running; `ezpn` will spawn
one.

## 6. Round-trip checklist

Before you delete `~/.tmux.conf`, run through this:

- [ ] Default keybindings look right (`C-b "`, `C-b %`, `C-b d`).
- [ ] `~/.config/ezpn/config.toml` has the prefix you want.
- [ ] If you used tmuxinator, every project gets a `.ezpn.toml`
      (or you've decided to launch them ad-hoc).
- [ ] Copy mode feels right — vi-mode by default; `Space` is bound to
      cancel (not begin-selection like tmux); use `v`/`V`.
- [ ] OSC 52 clipboard works end-to-end (see
      [clipboard.md](./clipboard.md#41-diagnosing-a-broken-chain)).
- [ ] If you script tmux with `send-keys`, switch to `ezpn-ctl
      send-keys --await-prompt` where blocking semantics matter.
