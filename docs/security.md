# Security model

This document covers the threat model for multiplexer-controlled escape
sequences and the default policies ezpn ships.

## Threat model

ezpn sits between user-controlled input (your keystrokes) and
program-controlled output (PTY bytes from child processes). Two distinct
trust boundaries matter:

1. **Programs you run** — your shell, editors, build tools. These are
   expected to emit OSC 52 (clipboard), OSC 7 (cwd), and similar
   sequences as part of normal operation.
2. **Output you display from elsewhere** — `cat hostile.log`, `curl …`,
   `tail` on a network log. The bytes here have not been audited and
   may try to inject sequences that affect host-emulator state, the
   user's clipboard, or the user's filesystem.

The dangerous case is #2: a hostile byte stream printed to the terminal.
The user is expecting to read text. They are not expecting their
clipboard contents to silently change, or to be exfiltrated.

## Default policies

| Policy                       | Default value | Rationale |
|------------------------------|---------------|-----------|
| `clipboard.osc52_set`        | `confirm`     | Match tmux >= 3.4 default. First write per pane prompts the user; subsequent writes within the same pane lifetime use the cached decision. |
| `clipboard.osc52_get`        | `deny`        | Read is the dominant attack vector — apps that legitimately read clipboard contents are extremely rare. |
| `clipboard.osc52_max_bytes`  | `1048576` (1 MiB) | Defence against memory-exhaustion via crafted output. The 16 MiB hard cap prevents config typos from re-introducing the vulnerability. |

## What changed from v0.5

In v0.5 ezpn forwarded OSC 52 set sequences directly to the host
terminal. A program that read a file controlled by an attacker (e.g.
`cat malicious.txt`) could replace your clipboard contents silently.
This is fixed in v0.12 by intercepting OSC 52 in
[`Pane::read_output`](../src/pane.rs) and routing through the policy
chain in [`crate::terminal_state`](../src/terminal_state.rs).

## What is NOT in scope

- **Wayland/X11 clipboard fallback** — when the host emulator is itself
  inside `tmux` or another nested multiplexer, OSC 52 may not reach the
  Wayland/X11 clipboard at all. v0.16 will optionally bridge to
  `wl-copy` / `xclip`. (See issue #80 family.)
- **Per-pane theme overrides for OSC 4/10/11/12** — colour queries are
  answered with the session-wide theme. Per-tab themes are deferred to
  v0.15.
- **OSC 8 ID renumbering for multi-client** — see
  [multi-client-osc.md](./multi-client-osc.md#multi-client-osc-8-id-collisions).

## Auditing your config

```sh
ezpn-ctl config show | grep -A4 '\[clipboard\]'
```

If `osc52_set = "allow"`, the v0.5 vulnerability is back; treat anything
that prints to your terminal as trusted-equivalent.
