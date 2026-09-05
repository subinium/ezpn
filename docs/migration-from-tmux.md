# Moving from tmux

ezpn shares common prefix bindings, not the entire tmux command language.
It emphasizes quick layouts and mouse interaction. No comparative performance
advantage is claimed without a reproducible workload.

## Familiar actions

| tmux-style action | ezpn |
| --- | --- |
| Prefix | Ctrl+B by default |
| Split columns / rows | Prefix % / " |
| Create / next / previous window | Prefix c / n / p |
| Select window | Prefix 0..9, zero-based |
| Detach | Prefix d (only this attached client) |
| Close pane/window | Prefix x / & with confirmation |
| Zoom pane | Prefix z |
| Command prompt | Prefix : |
| Copy mode | Prefix [, v/V select, y copy, q exit |
| Reload config | Prefix r |

Standard shell control keys pass through unless explicitly rebound.
The previous direct Ctrl+D/Ctrl+E split behavior is not the default anymore.

## Live sessions and remote hosts

```sh
ezpn -S work
ezpn a work --shared
ezpn a work --readonly
ssh -t host 'ezpn -S work'
```

Install ezpn on the remote host first. Jobs remain on that host after detach.
Disk restore starts new processes; it does not checkpoint process memory.

## Workspaces and layouts

Review .ezpn.toml/Procfile, then run `ezpn --trust-project`.
`select-layout` changes geometry without restarting processes and requires the
same pane count. Use explicit split/close before changing counts.

Global configuration is TOML, not .tmux.conf. No automatic tmux configuration or
plugin migration is performed.

## Differences and limits

- Native Unix PTYs on macOS/Linux; no native Windows support.
- Mouse-aware applications receive their requested mouse format. Shift-drag uses
  ezpn selection rather than application mouse handling.
- Per-pane clipboard confirmation differs from tmux policy; read
  [clipboard](clipboard.md) before changing it.
- No tmux plugin/API compatibility or arbitrary command aliases.
- Reserved send-keys/semantic completion and event-subscription CLI functionality
  are not operational; see [scripting](scripting.md).
- Session locator rename does not rewrite the daemon's internal session identity.
- Inline terminal graphics and persistent OSC 8 metadata are unsupported.
- A smaller feature surface does not prove lower latency or memory usage.

The [README](../README.md), [configuration](configuration.md) and
[release audit](audits/v0.14.0.md) describe the tested contract.
