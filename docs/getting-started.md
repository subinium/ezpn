# Getting Started

Install the executable with Rust 1.88 or newer:

```sh
cargo install ezpn --locked
ezpn -S work
```

Or download a matching binary from [Releases](https://github.com/subinium/ezpn/releases)
and verify its accompanying checksum.

Click pane content to focus, drag separators to resize, and use title-bar buttons
to split or request close. Standard shell control keys remain shell input.
The prefix is Ctrl+B:

- `%` / `"`: split into columns / rows.
- `c`: new tab; `n` / `p`: next / previous tab.
- `z`: zoom; `x`: confirm pane close.
- `[`: copy mode; `v` select; `y` copy; `q` exit.
- `d`: detach this client without terminating the session.

```sh
ezpn a work
ezpn a work --shared
ezpn a work --readonly
ezpn ls
ezpn kill work
```

For SSH, install ezpn on the remote host's PATH and allocate a PTY:

```sh
ssh -t host 'ezpn -S work'
ssh -t host 'ezpn a work'
```

The server stays on that remote host. A dropped client does not migrate processes
to another machine or preserve them through a host reboot.

Project startup requires an explicit trust decision:

```sh
ezpn init
ezpn doctor
ezpn --trust-project
```

Review .ezpn.toml/Procfile first. Doctor does not execute or expand secrets.
Use `ezpn 1 2` for plain shells without project startup.

Disk `--restore FILE` starts new processes; it is not live session reattachment.
See [configuration](configuration.md), [terminal compatibility](terminal-protocol.md)
and [security](security.md) before enabling persistent history or application clipboard writes.
