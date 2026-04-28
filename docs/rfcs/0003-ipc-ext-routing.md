# RFC 0003 — IPC `Ext` channel unification

| | |
|---|---|
| **Status** | Accepted (retroactive — shipped in v0.13.0) |
| **Tracks issue** | #103 |
| **Supersedes** | The bi-modal `IpcRequest` / `IpcRequestExt` dispatch documented in v0.12.x |
| **Implemented in** | v0.13.0 (commit `0d0bd80`) |
| **Owner** | @subinium |

## Summary

Through v0.12.x, IPC commands used two parallel dispatch paths. Legacy `IpcRequest` (Split / Close / Save / Load / …) flowed through an `mpsc` channel into the daemon main loop, which holds the live state (`panes`, `tab_mgr`, `clients`). The newer `IpcRequestExt` (`ls_tree`, `dump`, `send_keys`) was handled inline in the per-connection thread (`src/ipc.rs::handle_ext_request`), which had no access to that state — so every Ext arm returned `IpcResponse::error("server-side handler not yet wired")`.

v0.13.0 collapsed both paths into a single `enum IpcCommand { Legacy(IpcRequest), Ext(IpcRequestExt) }` carried over one channel. This RFC documents the shipped design, captures why the alternatives were rejected, and locks the policy that prevents a future Ext-style split.

## Motivation

### What was broken in v0.12.x

`src/ipc.rs::handle_ext_request` (now removed; see `src/ipc.rs:539-542` placeholder comment) executed in the per-connection thread spawned by `start_listener`'s accept loop:

```text
listener accept → handle_client(stream, tx) thread
                  ├── parse JSON
                  ├── if IpcRequest:    tx.send((cmd, resp_tx)) → main loop
                  └── if IpcRequestExt: handle_ext_request(...) inline
                                        └── no panes / no tab_mgr / no clients
                                        └── always returns IpcResponse::error
```

The `IpcRequestExt` branch was a stub from day one. #88 (`ezpn-ctl dump`) and #89 (`ezpn-ctl ls --json`) were filed because the wire schema landed without a usable server side.

### What two channels cost

Two costs, both real:

1. **Two surfaces to keep correct under refactor.** Every change to per-pane state had to be audited against both paths to avoid stale snapshots in the inline handler. With a single channel, the state-access contract is "if you can read it, you got it from the main loop."
2. **No path to features that need write access.** `send-keys` (#81) writes to a pane's PTY. The inline handler had no `&mut Pane`. Adding `Arc<Mutex<HashMap<usize, Pane>>>` to bridge the gap was on the table; see the rejected option (b) below.

## Design

### Shipped: option (a) — unified `enum IpcCommand`

```rust
// src/ipc.rs:145-154
pub enum IpcCommand {
    Legacy(IpcRequest),
    Ext(IpcRequestExt),
}

// src/ipc.rs:412
pub fn start_listener() -> anyhow::Result<mpsc::Receiver<(IpcCommand, ResponseSender)>>
```

`handle_client` (`src/ipc.rs:475-525`) peeks the JSON `cmd` tag via `is_ext_command` (line 529) — a cheap `serde_json::Value` parse that reads only the top-level `cmd` field — and deserialises into the matching variant:

```rust
// src/ipc.rs:496-504
let parsed: Result<IpcCommand, String> = if is_ext_command(&line) {
    serde_json::from_str::<IpcRequestExt>(&line)
        .map(IpcCommand::Ext)
        .map_err(|e| format!("invalid request: {}", e))
} else {
    serde_json::from_str::<IpcRequest>(&line)
        .map(IpcCommand::Legacy)
        .map_err(|e| format!("invalid request: {}", e))
};
```

Both variants ride one `mpsc::Sender<(IpcCommand, ResponseSender)>` into the main loop. The loop drains via `try_recv` (`src/server/mod.rs:1364-1388`) and dispatches Ext commands through `crate::server::ext_handlers::dispatch_ext_mut` before falling through to the legacy match arm.

### `EXT_CMD_TAGS` registry

`src/ipc.rs:134` carries the canonical list:

```rust
const EXT_CMD_TAGS: &[&str] = &["ls_tree", "dump", "send_keys"];
```

Adding a new Ext command is a two-line change: append the tag here, append the variant to `IpcRequestExt`. The dispatcher in `ext_handlers.rs` grows one match arm. No churn in `handle_client` or the main loop.

### Why option (b) — `Arc<Mutex<ServerState>>` — was rejected

The inline handler could have been kept by passing it a shared-state handle. Three reasons against:

1. **Lock contention.** Every IPC command would acquire the same mutex. The main loop already holds short-but-frequent borrows (per-frame render, per-tick housekeeping). Read-side `try_lock` failures would surface as IPC `internal error` responses indistinguishable from real bugs.
2. **Borrow-discipline regression.** ezpn's main loop currently holds `panes`, `tab_mgr`, `clients`, `layout` as separate locals so the borrow checker can split them (`src/server/ext_handlers.rs:33-46` accepts each as an independent reference). Wrapping them in one mutex would force a single coarse-grained lock.
3. **Two correctness surfaces remain.** The inline path and the main-loop path would need to agree on every piece of state. Lock acquisition order, panic safety, dropped-event semantics — all duplicated.

### Why option (c) — actor pattern — was rejected for now

A per-subsystem actor model (PaneManager, TabManager, ClientManager each with a mailbox) is the right v1.0 answer if/when the main loop is provably the bottleneck. Today it is not: ezpn's bench data shows < 1ms IPC round-trip at 100 concurrent clients on a single-loop dispatcher. The cost of restructuring main loop into a coordination shell now is paid against zero current pain. Captured as future work below.

### Wire format compatibility

`IpcRequest` and `IpcRequestExt` are both `#[serde(tag = "cmd")]`. JSON shape is unchanged; existing `ezpn-ctl` builds against the v0.12.x wire format keep working unmodified. The internal `IpcCommand` enum exists only on the daemon side.

### Read-only vs `&mut Pane` Ext handlers

`Dump` requires `&mut Pane` because the scrollback walk in `src/pane.rs:521-563` mutates `parser.set_scrollback(...)`. `ext_handlers.rs:35-72` exposes two entry points:

- `dispatch_ext(&Pane, &TabManager, …)` — read-only handlers (`LsTree`).
- `dispatch_ext_mut(&mut Pane, …)` — mutable handlers (`Dump`, eventually `SendKeys`).

The main loop calls the `_mut` variant; the read-only variant exists so future short-lived helpers (e.g., a status-bar IPC poll) can avoid unnecessary `&mut` borrows. See `src/server/ext_handlers.rs:25-34` for the docstring on the borrow split.

## Open Questions

- **`send-keys --await-prompt`** (#81) blocks until OSC 133 D arrives. Blocking inside the main loop is unacceptable. The stub at `ext_handlers.rs:65-69` returns `not yet wired`; the real implementation must spawn a helper thread, take a per-pane prompt-arrival sender, and reply asynchronously. Owner: #81; design lives there, not here.
- **`events` subscription endpoint** (#82) is a long-lived stream, not a request/response. The unified `IpcCommand` envelope assumes one response per command. The likely answer is a fourth Ext variant `Subscribe` whose response is a stream end-marker plus an out-of-band channel that piggybacks on the same socket. Defer to the #82 implementation.
- **Per-command priority queue** — if IPC traffic ever swamps frame rendering, dispatch order may need to favour render over IPC (or vice versa). Not needed today; flag for revisit at the v1.0-rc soak test.

## Decision Path / Recommendation

Already shipped. The retroactive policy this RFC locks:

1. **One channel, one routing rule.** Every new IPC vocabulary goes through `IpcCommand`. No new inline `handle_*` paths in `handle_client`.
2. **Tag registration is cheap; do it.** New Ext commands append to `EXT_CMD_TAGS` and `IpcRequestExt`; no exceptions.
3. **`Arc<Mutex<...>>` for IPC state is forbidden** without a follow-up RFC documenting why the unified channel is insufficient.
4. **Actor restructure (option c) is the next escalation** — captured as a follow-up RFC if main-loop p99 IPC latency exceeds 5 ms at 100 concurrent clients (current bench: < 1 ms).

### Numbers (current, post-shipping)

- **Channel depth**: unbounded `mpsc` (see `start_listener` line 432). Each command carries one `SyncSender<IpcResponse>` of capacity 1, so backpressure is per-client, not global.
- **Dispatch overhead**: one `serde_json::from_str::<Value>` for the `cmd`-tag peek + one full deserialise. For typical 50–200-byte command lines this is dominated by the second parse; the peek cost is < 5 µs measured.
- **Lock count crossing thread boundaries**: zero. The Ext path now reads daemon state via the main loop's `&mut`, not via `Arc<Mutex<...>>`.

## References

- Issue #103 — this RFC's tracking issue
- Issue #88 — `ezpn-ctl dump` (server side enabled by this unification)
- Issue #89 — `ezpn-ctl ls --json` (same)
- Issue #81 — `send-keys --await-prompt` (next consumer; needs async response handling)
- Issue #82 — events subscription (open design question above)
- `src/ipc.rs:17-43` — `IpcRequest` enum (legacy vocabulary)
- `src/ipc.rs:63-129` — `IpcRequestExt` enum (extended vocabulary)
- `src/ipc.rs:134` — `EXT_CMD_TAGS` registry
- `src/ipc.rs:145-154` — `IpcCommand` envelope
- `src/ipc.rs:412-468` — `start_listener`
- `src/ipc.rs:475-525` — `handle_client` (single-channel dispatch)
- `src/ipc.rs:539-542` — placeholder where `handle_ext_request` used to live
- `src/server/mod.rs:1364-1388` — main-loop drain
- `src/server/ext_handlers.rs` — daemon-side Ext dispatcher
- CHANGELOG `[0.13.0]` § Wiring — `ls --json` and `dump` server handlers

Closes #103
