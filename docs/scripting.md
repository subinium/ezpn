# Scripting — `ezpn-ctl`, events, and `ls --json`

External tooling drives ezpn through three surfaces:

1. **`ezpn-ctl`** — request/response over a UNIX domain socket.
2. **Event subscription stream** — push notifications when ezpn state
   changes.
3. **`ls --json` and `--json` on every command** — machine-readable
   output for status bars, editors, CI hooks.

All three are **frozen at v1.0**. Adding a new command, event type, or
JSON field is additive (`proto_minor` bump). Renaming or removing one
is a breaking change.

## 1. `ezpn-ctl` overview

```
ezpn-ctl [--pid <PID> | --socket <PATH>] [--json] <command> [args...]
```

When neither `--pid` nor `--socket` is set, `ezpn-ctl` finds the
**most-recently-modified** ezpn socket in `$XDG_RUNTIME_DIR` (falling
back to `/tmp`) that is still connectable, and uses that. Stale sockets
from dead processes are filtered out automatically.

### 1.1 Frozen v1 commands

| Command                              | Effect                                  |
|--------------------------------------|-----------------------------------------|
| `split horizontal [pane]`            | Split active (or named) pane left/right.|
| `split vertical [pane]`              | Split top/bottom.                       |
| `close <pane>`                       | Close a pane.                           |
| `focus <pane>`                       | Focus a pane.                           |
| `equalize`                           | Equalize all pane sizes.                |
| `list`                               | List panes (see §3 for `--json` form).  |
| `layout <spec>`                      | Reset to layout spec (e.g. `7:3/1:1`).  |
| `exec <pane> <command>`              | Run command in a pane.                  |
| `save <path>`                        | Save workspace snapshot.                |
| `load <path>`                        | Load workspace snapshot.                |

A few additional verbs are reserved in the wire schema for v1.1+:
`config show`, `send-keys`, `subscribe`, `dump --pane`. They are NOT
guaranteed in v1.0 and will land alongside their issues.

### 1.2 Wire format

`ezpn-ctl` opens a UNIX domain socket and writes one JSON object per
line. The daemon writes back a single JSON response object terminated
by `\n`. The schema is `IpcRequest` / `IpcResponse` in
[`src/ipc.rs`](../src/ipc.rs):

```json
// request
{"cmd": "split", "direction": "horizontal", "pane": null}

// response (success, no panes)
{"ok": true, "message": "split"}

// response (success, list)
{"ok": true, "panes": [
  {"index": 0, "id": 1, "cols": 80, "rows": 24, "alive": true, "active": true,  "command": "/bin/zsh"},
  {"index": 1, "id": 2, "cols": 80, "rows": 24, "alive": true, "active": false, "command": "tail -f /var/log/system.log"}
]}

// response (failure)
{"ok": false, "error": "no such pane: 9"}
```

`ok`, `message`, `error`, `panes` are all top-level. Adding a new
optional field is additive. Removing one is a major bump.

### 1.3 Exit codes

* `0` — success.
* `1` — request failed (`ok: false`); the error message is printed to
  stderr.
* `1` (with a different prefix) — connection error (no socket found,
  permission denied, etc.).

`ezpn-ctl --json` always exits `0` and prints the JSON response, even
on `ok: false`. This lets shell scripts inspect the JSON without
losing structured error information.

## 2. Event subscription stream (frozen — issue #82)

`ezpn-ctl events` opens an event subscription on the IPC socket. The
daemon pushes newline-delimited JSON events as ezpn state changes:

```
$ ezpn-ctl events --filter pane.spawned,pane.exited --session work
{"type":"pane.spawned","session":"work","pane":3,"command":"zsh","cwd":"/home/me/work","ts":1745800000.123}
{"type":"pane.exited","session":"work","pane":3,"exit_code":0,"ts":1745800012.456}
```

Filters:

* `--filter <type1,type2,...>` — only emit these `type` values.
* `--session <name>` — only events whose session matches.

Both filters are AND-combined.

### 2.1 Frozen event vocabulary (v1)

| `type`               | Required fields                                    | Notes |
|----------------------|----------------------------------------------------|-------|
| `session.created`    | `session`, `ts`                                    | Daemon binds its socket. |
| `session.detached`   | `session`, `ts`                                    | Last client disconnects. |
| `pane.spawned`       | `session`, `pane`, `command`, `ts`; `cwd?`        | A new pane is inserted. |
| `pane.exited`        | `session`, `pane`, `ts`; `exit_code?`              | Child process exits. |
| `pane.focused`      | `session`, `pane`, `ts`                            | Focus moves to a pane. |
| `pane.cwd_changed`   | `session`, `pane`, `cwd`, `ts`                     | OSC 7 or procfs poll detects new cwd. |
| `pane.prompt`        | `session`, `pane`, `ts`; `exit_code?`              | OSC 133 D semantic prompt. |
| `tab.added`          | `session`, `tab`, `ts`                             | New tab. |
| `tab.renamed`        | `session`, `tab`, `name`, `ts`                     | Tab renamed. |
| `config.reloaded`    | `ok`, `ts`                                         | After SIGHUP / `Ctrl+B r`. |
| `snapshot.saved`     | `session`, `path`, `ts`                            | After `workspace::save_snapshot` Ok. |
| `events.dropped`     | `count`, `ts`                                      | Synthetic; injected when the subscriber's queue overflowed. |

The Rust source-of-truth is [`src/events.rs`](../src/events.rs). All
fields above are **frozen**. Optional fields (`cwd?`, `exit_code?`)
remain optional in v1.x.

`ts` is a `f64` of seconds since UNIX epoch.

### 2.2 Backpressure

Each subscriber has a bounded queue of `MAX_QUEUE_DEPTH = 1000` events.
When the queue is full, the **oldest** event is dropped and a
per-subscriber counter increments. The next successful publish for that
subscriber injects a synthetic `events.dropped` event ahead of the new
payload and resets the counter:

```json
{"type":"events.dropped","count":42,"ts":1745800123.0}
{"type":"pane.spawned",...}
```

Subscribers MUST tolerate `events.dropped` events. Treat them as a
re-sync signal (re-poll `list` if you need exact state).

### 2.3 send-keys --await-prompt (#81)

`ezpn-ctl send-keys --pane <p> --await-prompt -- <keys>` writes the
key sequence into the pane and blocks until a `pane.prompt` event
fires for that pane. This is how scripts wait for `cargo test` to
finish without polling.

```sh
# wait for the prompt that follows our cargo command
ezpn-ctl send-keys --pane 0 --await-prompt --timeout 60s -- 'cargo test\n'
echo "tests done"
```

Without `--await-prompt`, `send-keys` returns immediately after the
keys are queued.

## 3. `ls --json` schema (frozen v1)

```sh
ezpn-ctl --json list
```

Emits the response object from §1.2 verbatim. The `panes` array element
schema:

```json
{
  "index":   0,            // u32, 0-based position in the active tab
  "id":      1,            // u64, stable for the pane's lifetime
  "cols":    80,           // u16
  "rows":    24,           // u16
  "alive":   true,         // bool — false once the child exits
  "active":  true,         // bool — currently focused
  "command": "/bin/zsh"    // string — argv[0] when spawned
}
```

| Field    | Type   | Required | Notes |
|----------|--------|----------|-------|
| `index`  | u32    | yes      | 0-based position in the active tab. |
| `id`     | u64    | yes      | Stable identifier for the pane's lifetime. |
| `cols`   | u16    | yes      | Current width. |
| `rows`   | u16    | yes      | Current height. |
| `alive`  | bool   | yes      | `false` after the child exits. |
| `active` | bool   | yes      | Whether the pane currently has focus. |
| `command`| string | yes      | Resolved `argv[0]` when spawned. |

### 3.1 Top-level `ezpn ls --json`

`ezpn ls` (no `-ctl`) lists **sessions**, not panes. Output:

```json
{
  "sessions": [
    {"name": "work",  "pid": 23456, "attached": true,  "panes": 4, "tabs": 2},
    {"name": "scratch","pid": 23789, "attached": false, "panes": 1, "tabs": 1}
  ]
}
```

| Field      | Type    | Required | Notes |
|------------|---------|----------|-------|
| `name`     | string  | yes      | Session label. |
| `pid`      | u32     | yes      | Daemon PID. |
| `attached` | bool    | yes      | Has at least one client. |
| `panes`    | u32     | yes      | Total live panes across all tabs. |
| `tabs`     | u32     | yes      | Tab count. |

## 4. Common scripting patterns

### 4.1 Wait until a pane finishes its current command

```sh
ezpn-ctl events --filter pane.prompt --session work | while read line; do
  pane=$(echo "$line" | jq -r '.pane')
  exit_code=$(echo "$line" | jq -r '.exit_code // 0')
  case $pane in
    3) echo "tests pane finished with code $exit_code"; break ;;
  esac
done
```

### 4.2 Status bar showing the active pane's cwd

```sh
ezpn-ctl events --filter pane.cwd_changed,pane.focused | jq -r '
  if .type == "pane.cwd_changed" then "cwd: \(.cwd)"
  elif .type == "pane.focused"   then "focus: pane \(.pane)"
  else empty end'
```

### 4.3 Pane snapshot for a CI dashboard

```sh
ezpn-ctl --json list \
  | jq '.panes[] | select(.alive) | {id, command, active}' \
  > /tmp/ezpn-state.json
```

### 4.4 Conditional spawn — only if the pane isn't already there

```sh
exists() {
  ezpn-ctl --json list \
    | jq -e --arg cmd "$1" '.panes[] | select(.command == $cmd)' >/dev/null
}

exists 'tail -f app.log' || ezpn-ctl exec 0 'tail -f app.log'
```

## 5. Stability guarantees

* The wire schema in §1.2, §2.1, and §3 is **frozen** at v1.0.
* Adding a new `IpcRequest` variant or `Event` type in v1.x is additive
  and does NOT require clients to update.
* Removing or renaming any of the above requires a v2.0 bump.
* The CLI surface (`ezpn-ctl <command>`) is frozen for the v1 commands
  in §1.1; new subcommands may be added.

For the underlying binary protocol (frame format, tag space, handshake)
see [`docs/protocol/v1.md`](./protocol/v1.md).
