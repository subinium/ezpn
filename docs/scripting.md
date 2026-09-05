# Control API and Scripting Limits

```sh
ezpn-ctl --pid DAEMON_PID list
ezpn-ctl --pid DAEMON_PID ls --json
ezpn-ctl --pid DAEMON_PID split h
ezpn-ctl --pid DAEMON_PID layout '7:3'
ezpn-ctl --pid DAEMON_PID save /path/work.ezpn.json
ezpn-ctl --pid DAEMON_PID load /path/work.ezpn.json
```

Use an explicit PID or `--socket PATH` when automation must address one session.
The control socket uses newline-delimited JSON; the attach socket uses binary
framing. They are different endpoints.

## Implemented commands

| Command | Meaning |
| --- | --- |
| list / ls --json | Pane list / session-tab-pane tree |
| split h / split v | Add a shell if the content area can fit it |
| close ID / focus ID | Close/focus a pane in the active tab |
| equalize | Equalize existing panes |
| layout SPEC | Reflow existing processes; same pane count required |
| exec ID COMMAND | Replace that pane's process with the explicit command |
| dump --pane ID | Capture plain output text |
| save PATH | Write a private bounded snapshot |
| load PATH | Prepare replacement before terminating the old workspace |

`load` and `exec` execute commands. They are not read-only operations.

## Reserved or incomplete surfaces

`send-keys` has a parsed CLI/wire shape, but server execution and semantic
`--await-prompt` acknowledgement are not implemented. It returns an explicit
error. Do not rely on it to drive editors or detect shell completion.

The event bus is internal; `ezpn-ctl events`, subscriptions and
`ezpn-ctl config show` are not implemented CLI commands.
OSC 133 semantic prompt completion is not guaranteed.

Pane IDs are scoped to the active tab on mutation/dump requests. The ls tree
includes tab indices; do not assume an ID is globally unique across tabs.

## Responses and failure handling

Requests and replies use the types in [src/ipc.rs](../src/ipc.rs).
Check the JSON `ok` field. Legacy `--json` output retains its existing exit-code
convention (structured errors can still exit zero); textual commands return an
error status. Transport timeouts, peer UID checks and response limits apply.

Dump is text-only and bounded. `--strip-ansi` does not recover original terminal
bytes: the parser has already converted them into cells. Scrollback capture does
not move the user's scroll view.

## Compatibility

The v1 framing is preserved. That does not imply full tmux command/flag parity
or a stable implementation of every reserved schema entry. Use the real PTY and
control integration tests as executable evidence, and see the
[release audit](audits/v0.14.0.md).
