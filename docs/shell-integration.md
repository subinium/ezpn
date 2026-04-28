# Shell integration

ezpn relies on a few escape sequences emitted by your shell to stay
consistent with what you actually see. None of these are required —
ezpn falls back to procfs polling and reasonable defaults — but turning
them on makes the multiplexer faster and works correctly over SSH.

## OSC 7 — current working directory (#75)

When a shell emits `OSC 7` on every directory change, ezpn skips procfs
polling entirely. This is the only way the multiplexer can know the cwd
of a remote shell over SSH (where `/proc/<pid>/cwd` lives on the remote
host).

### bash

```bash
# Append to ~/.bashrc
prompt_command_pwd() {
    local hostname
    hostname=${HOSTNAME:-$(hostname)}
    printf '\e]7;file://%s%s\e\\' "$hostname" "$PWD"
}
PROMPT_COMMAND="prompt_command_pwd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
```

### zsh

```zsh
# Append to ~/.zshrc
prompt_pwd() {
    local hostname=${HOST:-${HOSTNAME:-$(hostname)}}
    printf '\e]7;file://%s%s\e\\' "$hostname" "$PWD"
}
chpwd_functions+=(prompt_pwd)
prompt_pwd  # emit once for the initial cwd
```

### fish

`fish >= 3.0` emits OSC 7 automatically when the shell believes the
terminal supports it. If yours doesn't:

```fish
# ~/.config/fish/conf.d/osc7.fish
function osc7_pwd --on-variable PWD
    printf '\e]7;file://%s%s\e\\' (hostname) "$PWD"
end
osc7_pwd
```

## OSC 52 — clipboard

OSC 52 is **not** something your shell normally emits. Apps like Vim and
Neovim emit it when you yank into the `+` register with `clipboard=unnamed`
plus the relevant terminal-clipboard provider plugin. ezpn applies a
[per-pane policy](./multi-client-osc.md#osc-52--clipboard) — by default
the first OSC 52 set request raises a status-bar prompt.

If your workflow relies on OSC 52 being instant (vim/neovim with the
`+` register), set:

```toml
[clipboard]
osc52_set = "allow"
```

in `~/.config/ezpn/config.toml`. Be aware this disables the multiplexer's
defence against malicious clipboard injection from `cat`-ing hostile
files.

## Verifying

Inside an ezpn pane:

```sh
printf '\e]7;file://%s/tmp\e\\' "$(hostname)"
```

Then in another pane, the status bar / new-pane-here action should treat
`/tmp` as the cwd of the first pane.

For OSC 52:

```sh
printf '\e]52;c;%s\e\\' "$(printf 'hello' | base64)"
```

Should trigger a status-bar prompt (default `confirm` policy). After
accepting once, subsequent OSC 52 set sequences from that pane forward
without prompting until the pane closes.
