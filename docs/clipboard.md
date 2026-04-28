# Clipboard — OSC 52, fallback chain, SSH gotcha

ezpn's clipboard story is built around **OSC 52**, the terminal
protocol for copy/paste over the wire. This file covers the policy
chain, the fallbacks ezpn falls back to when OSC 52 is blocked, and the
SSH gotcha that bites everyone the first time.

## 1. What OSC 52 actually is

OSC 52 is a terminal escape sequence the running program emits to set
or read the user's system clipboard:

```
ESC ] 52 ; c ; <base64-encoded text> ESC \
```

The host emulator (the terminal app the user is staring at) is the only
party that can actually touch the OS clipboard. OSC 52 lets a program
running on a remote host send the bytes through the multiplexer and
the SSH session and have them land on the user's local clipboard. No
SSH agent forwarding, no `pbcopy`, no `xclip` — it just works as long
as everyone on the chain forwards the sequence.

## 2. ezpn's policy chain (#79)

Programs you run sit on the **trust** side of the boundary. Output you
display from elsewhere (`cat hostile.log`, `curl …`, `tail` on a network
log) sits on the **untrust** side. ezpn applies a per-pane policy chain
to every OSC 52 set sequence:

1. **Hard byte cap** — `clipboard.osc52_max_bytes` (default 1 MiB,
   absolute ceiling 16 MiB). Larger payloads are dropped with a `warn`
   log line. Defends against memory-exhaustion via crafted output.
2. **Per-pane decision cache** — once the user answers the prompt for
   a given pane, the answer (`Allowed` or `Denied`) is cached for the
   pane's lifetime.
3. **Configured policy** (`clipboard.osc52_set`):
    * `allow` — pass through unchanged. **Documented as insecure.**
    * `confirm` (default) — park the sequence, raise a status-bar
      prompt. The first OSC 52 set per pane prompts; subsequent writes
      use the cached decision.
    * `deny` — drop silently, log at warn level.

Reads (`OSC 52 ; c ; ?`) default to `deny`. Read is the dominant
attack vector — apps that legitimately read clipboard contents are
vanishingly rare, while malware exfiltrating recent clipboard contents
is not.

### 2.1 Tuning

```toml
# ~/.config/ezpn/config.toml
[clipboard]
osc52_set = "allow"           # disable confirm prompt — see warning below
osc52_get = "deny"            # leave as default unless you really need read
osc52_max_bytes = 524288      # 512 KiB
```

**`osc52_set = "allow"` reintroduces the v0.5 vulnerability** — anything
you `cat` to the terminal can silently overwrite your clipboard. Use it
only when you fully trust every byte your panes display, e.g. on a
dedicated dev workstation that never `tail`s untrusted logs.

### 2.2 Auditing

```sh
ezpn-ctl config show | grep -A4 '\[clipboard\]'
```

If `osc52_set = "allow"`, the v0.5 vulnerability is back; treat anything
that prints to your terminal as trusted-equivalent.

## 3. Fallback chain

OSC 52 only works if every link in the chain forwards the sequence:

```
program → ezpn (multiplexer) → host emulator → OS clipboard
```

When a link drops the sequence, ezpn does NOT try to bridge the
clipboard via `wl-copy` / `xclip` / `pbcopy` — that fallback is deferred
to v0.16 (#80 family). Until then:

| Failure mode                                 | Symptom                                  | Fix |
|----------------------------------------------|------------------------------------------|-----|
| `osc52_set = "deny"`                         | OSC 52 silently dropped at ezpn          | Re-enable `confirm` or `allow`. |
| Host emulator doesn't speak OSC 52           | OSC 52 reaches host, then dropped        | Use a terminal that supports OSC 52 (kitty, WezTerm, iTerm2 ≥ 3.4, foot, Alacritty ≥ 0.13, Terminal.app — see §6). |
| Nested multiplexer (tmux outside ezpn)       | Outer tmux drops OSC 52                  | Set `set-option -g set-clipboard on` in the outer tmux. |
| SSH client strips control sequences          | Rare; some terminal pagers do it         | Run `LESS=-R` for `less`; check `tmux`'s `escape-time`. |

ezpn's copy-mode `y` / `Enter` always emits OSC 52 — there is no
"silently degrade to internal buffer" path. If the chain breaks, the
user sees an empty clipboard, not stale data.

## 4. SSH gotcha (the one that bites everyone)

When you SSH into a remote host, run `ezpn` there, and copy from inside
ezpn, **the OSC 52 envelope must travel back through the SSH session**
to your local terminal. Three things have to be true:

1. **Your local emulator must accept OSC 52**. iTerm2, kitty, WezTerm,
   Terminal.app on macOS Ventura+, foot, and Alacritty 0.13+ do. macOS
   built-in Terminal.app on Big Sur and earlier does not.
2. **No outer multiplexer between your local emulator and ezpn must
   strip OSC 52**. If you have local tmux around your SSH session, set
   `set-option -g set-clipboard on` in `~/.tmux.conf`. (tmux 3.2+ has
   `external` as the default; older tmux defaults to `external` too,
   but check.) Zellij does not strip OSC 52 by default.
3. **ezpn's policy must allow the write**. Default is `confirm` — the
   first paste per pane raises a status-bar prompt. If you accept once,
   subsequent writes from that pane go through silently until the pane
   exits.

### 4.1 Diagnosing a broken chain

Run inside an ezpn pane on the remote host:

```sh
printf 'TEST\n' | base64 | xargs -I{} printf '\e]52;c;{}\e\\'
```

If your local clipboard now contains the literal string `TEST`, the
chain works. If not, walk the chain backwards:

* Run the same line **inside the SSH session but outside ezpn**. If
  this works, ezpn is the broken link — check `osc52_set`.
* Run the same line **outside the SSH session in your local terminal**.
  If this works, your SSH path drops the sequence — check for nested
  multiplexers or aggressive `escape-time` settings.
* If even local doesn't work, your terminal emulator does not implement
  OSC 52 set; nothing on the ezpn side can fix that.

## 5. OSC 52 from copy mode

`Ctrl+B [` enters copy mode. Default copy bindings (configurable in
`[keymap.copy_mode]`):

| Key             | Action                          |
|-----------------|---------------------------------|
| `v`             | Begin character selection.       |
| `V`             | Begin line selection.            |
| `y` / `Enter`   | Copy selection and exit copy mode (emits OSC 52). |
| `q` / `Esc`     | Exit copy mode without copying.  |

The OSC 52 emit goes through the **same policy chain** as program-emitted
OSC 52 — if you set `osc52_set = "deny"`, your own copy-mode yanks will
also be dropped. (This is intentional: the policy is a property of the
multiplexer, not of the source.)

## 6. Terminal emulator support matrix

| Emulator           | OSC 52 set | OSC 52 read | Notes |
|--------------------|------------|-------------|-------|
| kitty              | yes        | yes         | Reliable; the reference impl. |
| WezTerm            | yes        | yes         | Configurable per-window. |
| iTerm2 ≥ 3.4       | yes        | opt-in      | "Allow clipboard access" toggle. |
| Terminal.app (macOS Ventura+) | yes | no       | Read disabled. |
| foot               | yes        | yes         | Wayland. |
| Alacritty ≥ 0.13   | yes        | no          | Read intentionally absent. |
| GNOME Terminal     | partial    | no          | OSC 52 set behind a config flag. |
| xterm              | yes        | yes         | `XTerm*disallowedWindowOps` may block. |

Emulators not listed: assume they don't support OSC 52 until proven
otherwise.

## 7. Multi-client behaviour

When a clipboard set is allowed, the OSC 52 envelope is forwarded to
**every** attached client. If you're attached from laptop A and laptop
B simultaneously, both clipboards get the write. This matches user
expectation: detaching from A and attaching from B should still leave
the clipboard set on B's machine.

A pending OSC 52 confirm prompt is **per-pane, not per-client** — the
first client to answer the prompt makes the decision for everyone
attached to that pane.
