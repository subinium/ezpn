# SPEC 07 — Event subscription stream

**Status:** Draft
**Related issue:** TBD
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** B. Automation & Scripting

## 1. Background

tmux's `-CC` ("control mode") flag turns the multiplexer into a JSON-ish
event source: every pane spawn, exit, resize, window change, and client
attach surfaces as a `%output` / `%window-add` / `%session-changed`
notification. iTerm2's tmux integration, neovim's `vim-tmux-runner`,
Claude Code's terminal driver, and `tmuxinator`'s health-check loop all
depend on it.

ezpn currently has **no equivalent**:

- `S_OUTPUT` (`src/protocol.rs:30`) is a one-way render-frame push to the
  attached client. It carries pre-rendered terminal bytes, not structured
  events. An external tool that wanted to know "did pane 3 die?" would have
  to parse rendered ANSI, which is brittle and lossy.
- `IpcResponse` (`src/ipc.rs:56-65`) is request-reply over a one-shot
  socket connection. There is no notification channel.

PRD §6 ("API / scriptability gates") explicitly requires:

> `ezpn-ctl events --subscribe pane,client,layout` emits NDJSON events to
> stdout, schema documented.

This SPEC defines that surface end-to-end: a new `S_EVENT` server tag, a
`Subscribe` IPC variant, per-subscriber bounded backpressure that cites
SPEC 01's model, and a JSON schema for every topic in v0.10.

## 2. Goal

A long-lived `ezpn-ctl events --subscribe pane,client,layout` invocation
emits one well-formed JSON object per line for every state change in the
daemon, with bounded memory growth even when the consumer pauses for
minutes, and never breaks the main render loop.

## 3. Non-goals

- **Replay / event log.** Subscribers receive events that fire *after* they
  subscribe. There is no historical buffer, no "give me everything since
  T0". A future `--since` flag is tracked for v0.11.
- **Authenticated/multi-user fan-out.** All subscribers see all events for
  the session they connected to. Filtering is client-side except for the
  `--filter` flag (§5 CLI).
- **Bidirectional commands on the event channel.** Commands still go through
  the request-reply IPC (`SendKeys`, etc.). The event channel is one-way
  push from the daemon.
- **Cross-session aggregation.** One subscription = one daemon = one session.
  Multi-session aggregation is a userspace concern.
- **Wire-level deduplication / coalescing.** A burst of 100 `pane.resized`
  in 10 ms ships as 100 events (subject to backpressure overflow). Coalescing
  is the consumer's job.

## 4. Design

### 4.1 Topology

```
                ┌─────────────────┐
client A        │   ezpn daemon   │        client B (attached terminal)
(ezpn-ctl       │                 │        (renders frames)
 events)        │  main loop      │
   ▲            │   ├─ pane I/O   │           ▲
   │ S_EVENT    │   ├─ tab switch │           │ S_OUTPUT
   │ ndjson     │   ├─ IPC        │           │
   └── socket ──┤   └─ event_bus  ├── socket ─┘
                │      │           │
                │      ├── per-subscriber
                │      │     mpsc::sync_channel(256)
                │      │       ↓
                │      │     drop-oldest + S_EVENT_OVERFLOW
                │      └── …
                └─────────────────┘
```

`event_bus` is a new module `src/daemon/events.rs`. It owns a
`Vec<Subscriber>`. Each subscriber holds:

```rust
pub(crate) struct Subscriber {
    id: u64,
    topics: TopicMask,
    filter: Option<EventFilter>,
    tx: mpsc::SyncSender<EventEnvelope>,
    overflowed: bool,         // true once we've dropped at least one event
                              // since the last successful send
}
```

A worker thread per subscriber drains its `tx` half and writes
length-prefixed `S_EVENT` frames to the socket. Drops happen when
`tx.try_send` returns `Full`; on the next successful send the worker
prepends a single `S_EVENT_OVERFLOW { dropped: N }` notice so the consumer
knows it lost events.

### 4.2 Backpressure model (cites SPEC 01)

Per the SPEC 01 backpressure rules: every cross-thread channel out of the
main loop is **bounded** with `mpsc::sync_channel(N)`. For events:

- `N = 256` per subscriber. Sized for ~250 ms of bursty activity at 1 kHz.
- Drop policy: **drop-oldest** is implemented as drop-at-source — when
  `try_send` returns `Full`, we drain *one* slot and retry once. If still
  full, we drop the new event and set `overflowed = true`. This prevents
  the main loop from ever blocking on a stuck consumer.
- On the next successful enqueue after `overflowed`, a synthetic
  `S_EVENT_OVERFLOW` envelope ships first, carrying the cumulative drop
  count since the last notice.
- A subscriber whose socket has been disconnected for > 5 s is reaped
  and removed from `subscribers`.

The drop-oldest policy is preferred over `bounded mpsc + block` because
events are **diagnostic / reactive**, not transactional. Losing a few
`pane.resized` is acceptable; freezing the whole UI is not.

### 4.3 Event envelope

Every event on the wire is a JSON object of shape:

```json
{
  "v": 1,
  "ts": 1714082400123,
  "topic": "pane",
  "type": "pane.created",
  "session": "default",
  "data": { "...topic-specific..." : "..." }
}
```

- `v`: schema version. Bumped on any breaking shape change. Consumers MUST
  ignore unknown `type` values inside a known topic.
- `ts`: ms since Unix epoch (host clock). Sole timestamp source.
- `session`: the daemon's session name (`session_name` parameter to
  `daemon::event_loop::run`, see `src/daemon/event_loop.rs:39`).
- `data`: defined per-topic in §4.5.

### 4.4 `S_EVENT_OVERFLOW`

```json
{
  "v": 1,
  "ts": 1714082400500,
  "topic": "_meta",
  "type": "overflow",
  "session": "default",
  "data": { "dropped": 17, "since_ts": 1714082400123 }
}
```

`_meta` is a reserved topic for protocol-level notices. Consumers cannot
subscribe to it directly — overflow notices ship inline whenever they apply.

### 4.5 Topic schemas

All v0.10 topics and their event types, with a Rust struct sketch and a
JSON example each. Field names are snake_case; pane / tab IDs are `usize`
matching the in-memory representation.

#### `pane`

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneEvent {
    PaneCreated  { pane_id: usize, tab_index: usize, command: String, cols: u16, rows: u16 },
    PaneExited   { pane_id: usize, tab_index: usize, exit_code: Option<i32> },
    PaneResized  { pane_id: usize, cols: u16, rows: u16 },
}
```

```json
// pane.created
{"v":1,"ts":1714082400000,"topic":"pane","type":"pane.created",
 "session":"default",
 "data":{"pane_id":3,"tab_index":0,"command":"/bin/zsh","cols":120,"rows":40}}

// pane.exited
{"v":1,"ts":1714082460000,"topic":"pane","type":"pane.exited",
 "session":"default",
 "data":{"pane_id":3,"tab_index":0,"exit_code":0}}

// pane.resized
{"v":1,"ts":1714082405000,"topic":"pane","type":"pane.resized",
 "session":"default","data":{"pane_id":3,"cols":100,"rows":30}}
```

Emit sites in `src/daemon/event_loop.rs` and `src/app/lifecycle.rs`:

- `pane.created` after `lifecycle::do_split` (`src/app/lifecycle.rs:344-358`)
  and after `lifecycle::spawn_pane` paths in `event_loop` (e.g. `:780-783`).
- `pane.exited` when `pane.update_alive()` returns `Some` in the SIGCHLD
  handler (`src/daemon/event_loop.rs:1162-1167`) — exit code via
  `pane.exit_status()`.
- `pane.resized` from `lifecycle::resize_all` (`src/app/lifecycle.rs:377-391`).

#### `client`

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    ClientAttached { client_id: u64, mode: AttachMode, cols: u16, rows: u16 },
    ClientDetached { client_id: u64, reason: String },
}
```

```json
{"v":1,"ts":1714082400100,"topic":"client","type":"client.attached",
 "session":"default",
 "data":{"client_id":42,"mode":"shared","cols":120,"rows":40}}

{"v":1,"ts":1714082460100,"topic":"client","type":"client.detached",
 "session":"default",
 "data":{"client_id":42,"reason":"socket_closed"}}
```

`reason` ∈ `{"detach_request","socket_closed","kicked","panic"}`, derived
from `ClientMsg::{Detach,Disconnected,Panicked}` in
`src/daemon/event_loop.rs:611-627`.

#### `layout`

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutEvent {
    LayoutChanged { tab_index: usize, spec: String, pane_count: usize },
}
```

```json
{"v":1,"ts":1714082401000,"topic":"layout","type":"layout.changed",
 "session":"default",
 "data":{"tab_index":0,"spec":"main-vertical","pane_count":3}}
```

Emit after `update.border_dirty = true` paths that mutate `Layout`
structure (split / close / equalize / select-layout). Coalesced per render
frame: at most one `layout.changed` per tab per loop iteration.

#### `tab`

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TabEvent {
    TabCreated  { tab_index: usize, name: String },
    TabClosed   { tab_index: usize, name: String },
    TabSwitched { from_index: usize, to_index: usize, name: String },
    TabRenamed  { tab_index: usize, old_name: String, new_name: String },
}
```

```json
{"v":1,"ts":1714082402000,"topic":"tab","type":"tab.switched",
 "session":"default",
 "data":{"from_index":0,"to_index":1,"name":"build"}}
```

Emit sites: `TabAction` arms in `src/daemon/event_loop.rs:752-931`.

#### `mode`

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModeEvent {
    ModeEntered { mode: String, pane_id: Option<usize> },
}
```

```json
{"v":1,"ts":1714082403000,"topic":"mode","type":"mode.entered",
 "session":"default","data":{"mode":"copy_mode","pane_id":3}}
```

Modes mirror `InputMode` discriminants (`src/daemon/state.rs`). `mode` is
the snake_case discriminant name. `pane_id` is set only for
`copy_mode` and `pane_select`.

### 4.6 Subscriber lifecycle

```
ezpn-ctl events --subscribe pane,client
   │
   ▼ open binary protocol socket (same path as attached clients)
C_HELLO (v=1, caps=0)        → S_HELLO_OK
C_SUBSCRIBE {topics:[...]}   → S_SUBSCRIBE_OK {subscriber_id, topics}
   ◀ S_EVENT … (NDJSON-encoded JSON object inside framed payload)
   ◀ S_EVENT …
   ◀ S_EVENT_OVERFLOW (inline if any drops)
   ▼ socket close = unsubscribe (server reaps within ~50 ms via main loop)
```

Subscribers do **not** receive `S_OUTPUT`. The daemon's
`accept_client` path branches on the first post-hello message: `C_ATTACH`
(existing) vs `C_SUBSCRIBE` (new). A subscriber connection holds no `tw/th`
state and is excluded from the smallest-client-wins resize calculation
(`router::effective_size`).

## 5. Surface changes

### IPC / wire protocol

Add three constants to `src/protocol.rs`. Slot allocation: the highest
client tag in use is `C_HELLO = 0x07`; the highest server tag is
`S_HELLO_ERR = 0x86`. We take the next free slots in each range.

```rust
// src/protocol.rs additions

/// Subscribe to one or more event topics. Payload = JSON `SubscribeRequest`.
/// Sent after C_HELLO. The connection becomes a subscriber, not a render
/// client — no S_OUTPUT will be sent on it.
pub const C_SUBSCRIBE: u8 = 0x08;

/// Server confirms subscription. Payload = JSON `SubscribeOk`.
pub const S_SUBSCRIBE_OK: u8 = 0x87;

/// Single event push. Payload = JSON-encoded `EventEnvelope` (one object,
/// no trailing newline — the framing is done by [tag][len] not by line).
pub const S_EVENT: u8 = 0x88;

/// Inline notice that the per-subscriber backlog overflowed and at least
/// one event was dropped since the last `S_EVENT_OVERFLOW`. Payload = JSON
/// `EventEnvelope` with `topic = "_meta"` and `type = "overflow"`.
pub const S_EVENT_OVERFLOW: u8 = 0x89;
```

Tag-collision guard test in `src/protocol.rs:253-275` updated to include
the new tags.

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SubscribeRequest {
    pub topics: Vec<EventTopic>,
    /// Optional server-side filter. v0.10 supports `session = "<name>"`
    /// only; future fields ignored on the server (warned in logs).
    #[serde(default)]
    pub filter: Option<EventFilter>,
}

#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum EventTopic { Pane, Client, Layout, Tab, Mode }

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct EventFilter {
    pub session: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SubscribeOk {
    pub subscriber_id: u64,
    pub topics: Vec<EventTopic>,
}
```

### CLI (`ezpn-ctl`)

```
ezpn-ctl events --subscribe <topics> [--format <fmt>] [--filter <key=val>]

OPTIONS
  --subscribe <topics>   Comma-separated: pane,client,layout,tab,mode (or "all")
  --format <fmt>         json | ndjson  (default: ndjson)
  --filter <key=val>     Server-side filter. v0.10 supports: session=<name>

OUTPUT
  ndjson: one JSON object per line on stdout
  json:   stream of `[obj, obj, ...]` — array opens on connect, comma-separated,
          closes on SIGINT/SIGTERM. Designed for `jq -s 'group_by(.topic)'`.

EXIT
  0 on graceful disconnect (Ctrl-C / SIGTERM). 1 on protocol or socket error.

EXAMPLES
  ezpn-ctl events --subscribe pane,client
  ezpn-ctl events --subscribe all --format json | jq '.[] | select(.type=="pane.exited")'
  ezpn-ctl events --subscribe layout --filter session=build
```

Implementation in `src/bin/ezpn-ctl.rs`: this is the first subcommand that
keeps the socket open and pumps to stdout, so it skips the existing
read-one-response path (`src/bin/ezpn-ctl.rs:66-105`) and uses a dedicated
`fn run_events(...)` that loops on `protocol::read_msg`, decodes the
JSON payload, and writes to stdout. SIGPIPE handling matters here:
short-write to stdout = graceful exit.

### Config (TOML)

None — runtime API. Filter syntax is in CLI flags only.

## 6. Touchpoints

| File | Lines | Change |
|---|---|---|
| `src/protocol.rs` | 25-41, 253-275 | Add `C_SUBSCRIBE`, `S_SUBSCRIBE_OK`, `S_EVENT`, `S_EVENT_OVERFLOW` constants; extend tag-collision test |
| `src/protocol.rs` | (new section) | Add `SubscribeRequest`, `SubscribeOk`, `EventTopic`, `EventFilter`, `EventEnvelope` types |
| `src/daemon/events.rs` | new (~250 LOC) | Event bus, `Subscriber` struct, drop-oldest backpressure, overflow notice logic |
| `src/daemon/router.rs` | (existing `accept_client`) | Branch on `C_SUBSCRIBE`: register a subscriber instead of a render client |
| `src/daemon/event_loop.rs` | 280-360, 752-931, 1162-1167 | Emit `pane.*`, `tab.*`, `client.*` envelopes at the documented sites |
| `src/app/lifecycle.rs` | 344-391 | Emit `pane.created`/`pane.resized` at split/spawn/resize sites |
| `src/daemon/state.rs` | (InputMode definition) | `mode.entered` emit hook on every `*mode = ...` |
| `src/bin/ezpn-ctl.rs` | new arm + ~60 LOC | `events` subcommand: open socket, hello, subscribe, NDJSON pump loop |
| `tests/events.rs` | new | Integration: subscribe, drive splits, assert NDJSON shape |

## 7. Migration / backwards-compat

- **Protocol bump?** No. Adding new tag constants is backwards-compatible:
  old clients never send `C_SUBSCRIBE`, never receive `S_EVENT`. We do
  **not** bump `PROTOCOL_VERSION` (`src/protocol.rs:47`) — the major
  version covers framing semantics, not the tag enum.
- **Capability bit.** Add `CAP_EVENT_STREAM = 0x0010` to
  `SERVER_CAPABILITIES` so future clients can probe support without
  blindly attempting `C_SUBSCRIBE`. This **is** a header-bit change but
  follows the existing pattern in `src/protocol.rs:54-62`.
- **Old `ezpn-ctl` against new daemon.** Unchanged; the new `events`
  subcommand simply isn't there. No regression.
- **New `ezpn-ctl` against old daemon.** `C_HELLO` returns `S_HELLO_OK`
  with `capabilities & CAP_EVENT_STREAM == 0`; the new `events` subcommand
  prints `events: server does not support event subscriptions (upgrade ezpn)`
  and exits 1.

## 8. Test plan

1. **Unit — envelope serialization**: round-trip every `*Event` enum variant
   through serde; assert JSON shape matches the §4.5 examples (golden files
   under `tests/fixtures/events/`).
2. **Unit — backpressure**: spawn a Subscriber whose `tx` is full; emit 300
   events; assert exactly 256 are queued + 1 `S_EVENT_OVERFLOW` with
   `dropped >= 44` is enqueued on next successful send.
3. **Unit — disconnect reaping**: drop the subscriber socket; loop iteration
   detects a dead `tx` (or 5 s grace expires); subscriber removed from list.
4. **Integration — splits emit pane.created**:
   ```
   start daemon
   spawn `ezpn-ctl events --subscribe pane > out.ndjson &`
   ezpn-ctl split horizontal
   ezpn-ctl split vertical
   sleep 100ms; kill subscriber
   assert out.ndjson contains exactly 2 `pane.created` lines + 0..N `pane.resized`
   ```
5. **Integration — client.attached on real attach**: attach a real `ezpn`
   client; assert one `client.attached` envelope fires with `mode=shared`
   (or whatever the test uses).
6. **Integration — multi-subscriber fan-out**: 3 subscribers all subscribe
   to `pane`; one split → all 3 see `pane.created`; one of the 3 stalls →
   the other 2 keep flowing without delay.
7. **Performance gate (PRD §6)**: drive 1000 rapid splits/closes; assert
   subscriber-1 (fast consumer) sees all events; subscriber-2 (sleep 1 s
   between reads) gets `S_EVENT_OVERFLOW` notices and never blocks the main
   loop (median input latency < 16 ms during the burst — same gate as PRD
   §6 Perf/stability).

## 9. Acceptance criteria

- [ ] `protocol.rs` defines `C_SUBSCRIBE`, `S_SUBSCRIBE_OK`, `S_EVENT`,
      `S_EVENT_OVERFLOW`, plus `SubscribeRequest`, `EventEnvelope`, and
      per-topic structs; tag-collision test extended.
- [ ] `CAP_EVENT_STREAM` advertised in `S_HELLO_OK`.
- [ ] `ezpn-ctl events --subscribe pane,client,layout` emits NDJSON and
      survives `Ctrl-C` cleanly.
- [ ] Bursty producer never blocks main loop (validated by latency probe
      under PRD §6 perf gate).
- [ ] `S_EVENT_OVERFLOW` shipped exactly once per overflow episode with
      cumulative drop count.
- [ ] All 7 test categories in §8 pass.
- [ ] Schema documented in `docs/spec/v0.10.0/07-event-subscription-stream.md`
      §4.5 with stable JSON examples.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.

## 10. Risks

| Risk | Mitigation |
|---|---|
| Slow subscriber stalls the main loop | Per-subscriber `sync_channel(256)` + drop-oldest; cite SPEC 01's bounded-channel model. Validated by §8 test 7. |
| Event burst from `resize_all` floods subscribers (one per pane) | Coalesce `pane.resized` per render frame: collect into a `Vec` during the loop iteration, dedupe by `pane_id`, emit at most one envelope per pane per iteration. |
| JSON serialization on the hot path | Serialize lazily — `serde_json::to_vec(envelope)` runs **inside** the per-subscriber worker thread, not the main loop. Main loop pushes a `Box<EventEnvelope>` into the bounded channel; worker serializes + writes. |
| Tag-number collision with future SPECs | Reserve `0x08–0x0F` (client) and `0x87–0x8F` (server) for v0.10's automation surface. Document in `protocol.rs` next to the constants. |
| Schema drift between SPEC and code | The §4.5 JSON examples become test fixtures (`tests/fixtures/events/*.json`). Tests assert the live serializer output equals the fixture, byte-for-byte. Drift = test failure. |
| Subscriber leaks if `ezpn-ctl events` is killed mid-handshake | Daemon-side: every accepted socket runs through the existing `accept_client` panic-catching shell; if the subscriber thread panics or the socket hits EPIPE, the subscriber is removed from `subscribers` on the next loop iteration. |

## 11. Open questions

1. Do we want a `C_UNSUBSCRIBE { topics: [...] }` for fine-grained
   adjustment, or is "close the socket and reconnect" enough?
   **Default proposal**: skip for v0.10 — close+reconnect is fine for
   shell-driven consumers. Add if a real consumer asks.
2. Should `mode.entered` also emit `mode.exited`?
   **Default proposal**: only `mode.entered`. Most consumers care about
   "is the user in copy-mode?" which they can derive from successive
   `mode.entered` events (transition to `normal` = exited the previous
   mode). Saves wire chatter.
3. Should `pane.created` carry the `cwd` and `env`?
   **Default proposal**: no by default; add an opt-in `--include cwd,env`
   flag in v0.11. Both can leak secrets and bloat the stream.
4. NDJSON line buffering: should the daemon flush after every event?
   **Default proposal**: yes — interactive consumers (`jq -c`) need
   line-by-line flushing. Cost is one extra `flush()` per event, dominated
   by the socket write itself.
