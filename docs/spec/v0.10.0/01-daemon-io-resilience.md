# SPEC 01 — Daemon I/O Resilience

**Status:** Draft
**Related issue:** TBD (v0.10.0 milestone)
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** A. Stability & Resource Hygiene
**Severity origin:** Audit P1 #1 (slow-client backpressure), P1 #2 (IPC handler thread leak)

---

## 1. Background

Two unrelated I/O paths in the daemon both share the same failure mode:
**a single misbehaving peer (a slow attached client, or a hostile `ezpn-ctl`
caller) can stall or exhaust the daemon.** Both are P1 stability issues for
the v0.10.0 release because a 7-day soak is one of the gating metrics.

### (a) Slow-client write backpressure

Every render frame is written synchronously from the main loop to every
attached client through a `BufWriter<UnixStream>`. There is no per-client
queue, no write timeout, and no eviction policy.

`src/daemon/router.rs:131-141` (current `accept_client` body):

```rust
clients.push(ConnectedClient {
    id: client_id,
    writer: std::io::BufWriter::new(conn),
    event_rx: msg_rx,
    mode,
    caps,
    tw: new_w,
    th: new_h,
});
```

`src/daemon/event_loop.rs:1138-1150` (broadcast loop):

```rust
if render_result.is_ok() && !render_buf.is_empty() {
    clients.retain_mut(|c| {
        if protocol::write_msg(&mut c.writer, protocol::S_OUTPUT, &render_buf)
            .is_err()
        {
            // Try to send detach ack before dropping
            let _ = protocol::write_msg(&mut c.writer, protocol::S_DETACHED, &[]);
            false
        } else {
            true
        }
    });
}
```

`protocol::write_msg` calls `w.write_all()` followed by `w.flush()`. If
the client is consuming bytes at, say, `pv -L 100` (100 B/s), each frame
(~10–80 KB) blocks the daemon main loop for many seconds. PTY reads stall,
keystrokes from *other* clients are not processed, snapshot writes never
fire — the daemon is functionally frozen until the slow client either
catches up or its TCP buffer overflows.

`ConnectedClient` is defined at `src/daemon/state.rs:124-136`:

```rust
pub(crate) struct ConnectedClient {
    pub(crate) id: u64,
    pub(crate) writer: std::io::BufWriter<UnixStream>,
    pub(crate) event_rx: mpsc::Receiver<ClientMsg>,
    pub(crate) mode: protocol::AttachMode,
    #[allow(dead_code)]
    pub(crate) caps: u32,
    pub(crate) tw: u16,
    pub(crate) th: u16,
}
```

The `Drop` impl already shuts the socket down for the reader thread; we
need the same kind of "evict cleanly" guarantee for the *writer* path.

### (b) IPC handler thread leak

`src/ipc.rs:122-145` spawns one OS thread per accepted `ezpn-ctl`
connection with no cap, no read timeout, and no join handle:

```rust
std::thread::spawn(move || {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        let result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                handle_client(stream, tx);
                            }));
                        if let Err(payload) = result {
                            let reason = panic_payload_to_string(&payload);
                            eprintln!("ezpn-ctl: handler thread panicked: {}", reason);
                        }
                    });
                }
                Err(_) => break,
            }
        }
    }));
});
```

Inside `handle_client` (`src/ipc.rs:165-205`) the per-client loop is a
plain `BufReader::lines()` against an unconfigured `UnixStream`:

```rust
fn handle_client(stream: UnixStream, tx: mpsc::Sender<(IpcRequest, ResponseSender)>) {
    let Ok(read_stream) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(read_stream);
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        // ...
    }
}
```

A `nc -U /tmp/ezpn-ctl.sock` that connects, sends a single byte, and
sleeps forever burns one thread *per connection*. `kill -USR1 $(pgrep
ezpn)` style shell loops (or a buggy `ezpn-ctl` that hangs after writing
its request) trivially leak hundreds of threads. The PRD's release
criterion `ps -o nlwp` constancy across 1000 invocations is failed today.

---

## 2. Goal

Make the daemon's two write/read paths to *external* processes
**bounded, timed, and self-evicting**. After this SPEC lands:

- A single slow attached client cannot stall the main loop for more than
  one bounded send timeout (proposed: **50 ms per write attempt**, **3
  consecutive `WouldBlock`s before eviction**) — measured by the
  PRD's `pv -L 100` test: median local-input latency stays ≤ 16 ms.
- 1000 sequential `ezpn-ctl list` invocations leave the daemon at the
  same `nlwp` count (±1 for transient handler) — PRD release criterion.
- Hostile `ezpn-ctl` connections that connect-and-sleep are reaped within
  **5 seconds** by a per-handler read timeout.
- The thread budget for IPC is a **fixed pool of 4** (with overflow
  request rejection), not unbounded spawn-per-connection.

---

## 3. Non-goals

- Replacing the synchronous render-broadcast with a fully async runtime
  (tokio / async-std). The daemon stays single-threaded with bounded
  worker channels — adding a runtime is a v0.11+ topic.
- Replacing `mpsc::sync_channel` with crossbeam everywhere. Only the IPC
  pool uses crossbeam-channel for `select`-style multi-receiver dispatch;
  the per-client outbound channel stays on `std::sync::mpsc`.
- Per-client *output rate limiting* (e.g. fewer frames for slow clients).
  Eviction is the v0.10 policy; rate-adaptive frame skipping is v0.11.
- Authentication or per-IPC-client identity. The Unix socket already
  enforces `0o600` ownership; that is the trust boundary.

---

## 4. Design

### 4.1 Per-client outbound queue + write timeout

Replace the inline `BufWriter<UnixStream>` in `ConnectedClient` with
a bounded outbound queue plus a dedicated **writer thread** per client:

```
                 main loop                writer thread        socket
   render_buf ──► outbound_tx ──(mpsc 64)─► outbound_rx ──► UnixStream
                                                            (50ms timeout)
```

Proposed type, in `src/daemon/state.rs`:

```rust
pub(crate) enum OutboundMsg {
    Frame(Vec<u8>),       // S_OUTPUT
    Detached,             // S_DETACHED
    Exit,                 // S_EXIT
    Output(Vec<u8>),      // raw passthrough (OSC 52)
    HelloOk(Vec<u8>),     // pre-encoded payload (kept for symmetry)
    Shutdown,             // sentinel — writer thread exits, drops socket
}

pub(crate) struct ConnectedClient {
    pub(crate) id: u64,
    pub(crate) outbound_tx: mpsc::SyncSender<OutboundMsg>,
    pub(crate) writer_handle: Option<std::thread::JoinHandle<()>>,
    pub(crate) event_rx: mpsc::Receiver<ClientMsg>,
    pub(crate) mode: protocol::AttachMode,
    pub(crate) caps: u32,
    pub(crate) tw: u16,
    pub(crate) th: u16,
}
```

The writer thread, in a new `src/daemon/writer.rs`:

```rust
pub(crate) fn spawn_writer(
    socket: UnixStream,
    rx: mpsc::Receiver<OutboundMsg>,
    client_id: u64,
    wake_on_drop: mpsc::Sender<ClientMsg>,
) -> std::thread::JoinHandle<()> {
    socket
        .set_write_timeout(Some(Duration::from_millis(50)))
        .ok();
    std::thread::spawn(move || {
        let mut bw = BufWriter::with_capacity(64 * 1024, socket);
        let mut consecutive_wouldblocks = 0u32;
        const MAX_WOULDBLOCKS: u32 = 3;
        for msg in rx {
            let result = match &msg {
                OutboundMsg::Shutdown => return,
                OutboundMsg::Frame(b) | OutboundMsg::Output(b) => {
                    protocol::write_msg(&mut bw, protocol::S_OUTPUT, b)
                }
                OutboundMsg::Detached => protocol::write_msg(&mut bw, protocol::S_DETACHED, &[]),
                OutboundMsg::Exit => protocol::write_msg(&mut bw, protocol::S_EXIT, &[]),
                OutboundMsg::HelloOk(payload) => {
                    protocol::write_msg(&mut bw, protocol::S_HELLO_OK, payload)
                }
            };
            match result {
                Ok(()) => consecutive_wouldblocks = 0,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    consecutive_wouldblocks += 1;
                    if consecutive_wouldblocks >= MAX_WOULDBLOCKS {
                        let _ = wake_on_drop.send(ClientMsg::Disconnected);
                        crate::pane::wake_main_loop();
                        return;
                    }
                }
                Err(_) => {
                    let _ = wake_on_drop.send(ClientMsg::Disconnected);
                    crate::pane::wake_main_loop();
                    return;
                }
            }
        }
    })
}
```

Channel sizing: `mpsc::sync_channel(64)`. With 60 fps render budget and
typical 8–32 KB frames the queue holds about 1 second of frames before
the main loop's `try_send` returns `Full`. On `Full`, the main loop
treats the client as dead (synthesises `ClientMsg::Disconnected`) and
removes it on the next iteration — same path as a write error today.

The main-loop broadcast at `event_loop.rs:1138-1150` becomes:

```rust
if render_result.is_ok() && !render_buf.is_empty() {
    clients.retain(|c| {
        // Cheap clone of Vec<u8>; the alternative (Arc<Vec<u8>>) is
        // tracked as a follow-up perf win, not a correctness concern.
        match c.outbound_tx.try_send(OutboundMsg::Frame(render_buf.clone())) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_))
            | Err(mpsc::TrySendError::Disconnected(_)) => false,
        }
    });
}
```

`Drop` for `ConnectedClient` becomes:

```rust
impl Drop for ConnectedClient {
    fn drop(&mut self) {
        let _ = self.outbound_tx.try_send(OutboundMsg::Shutdown);
        if let Some(h) = self.writer_handle.take() {
            // Join with bounded patience — the writer should drain Shutdown
            // within at most 50 ms (the write_timeout). If not, leak the
            // handle: the OS will reap it on process exit.
            let _ = h.join();
        }
    }
}
```

### 4.2 IPC handler thread pool + read timeout

Replace the unbounded `std::thread::spawn` per accept with a fixed-size
worker pool and a per-handler read timeout. The acceptor thread stays
(it must call `listener.incoming()`); only the per-connection handlers
move to a pool.

Proposed structure in `src/ipc.rs`:

```rust
const IPC_POOL_SIZE: usize = 4;
const IPC_QUEUE_CAPACITY: usize = 16; // pending accepted-but-not-handled connections
const IPC_READ_TIMEOUT: Duration = Duration::from_secs(5);
const IPC_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

struct IpcPool {
    work_tx: crossbeam_channel::Sender<UnixStream>,
    _workers: Vec<std::thread::JoinHandle<()>>,
}

fn spawn_pool(
    cmd_tx: mpsc::Sender<(IpcRequest, ResponseSender)>,
) -> IpcPool {
    let (work_tx, work_rx) = crossbeam_channel::bounded::<UnixStream>(IPC_QUEUE_CAPACITY);
    let mut workers = Vec::with_capacity(IPC_POOL_SIZE);
    for worker_id in 0..IPC_POOL_SIZE {
        let rx = work_rx.clone();
        let cmd_tx = cmd_tx.clone();
        workers.push(std::thread::Builder::new()
            .name(format!("ezpn-ipc-{worker_id}"))
            .spawn(move || {
                while let Ok(stream) = rx.recv() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _ = stream.set_read_timeout(Some(IPC_READ_TIMEOUT));
                        let _ = stream.set_write_timeout(Some(IPC_WRITE_TIMEOUT));
                        handle_client(stream, cmd_tx.clone());
                    }));
                }
            })
            .expect("ipc worker spawn"));
    }
    IpcPool { work_tx, _workers: workers }
}
```

The acceptor body becomes:

```rust
for stream in listener.incoming() {
    match stream {
        Ok(s) => {
            if let Err(crossbeam_channel::TrySendError::Full(s)) = pool.work_tx.try_send(s) {
                // Pool is saturated — refuse with a structured error and close.
                let mut s = s;
                let resp = IpcResponse::error("ezpn ipc pool saturated; retry");
                let _ = write_response(&mut s, &resp);
                drop(s);
            }
        }
        Err(_) => break,
    }
}
```

Read-timeout handling inside `handle_client`: `BufReader::lines()` will
surface `io::ErrorKind::WouldBlock` or `TimedOut` on idle. We convert
those into a clean disconnect:

```rust
for line in reader.lines() {
    let line = match line {
        Ok(line) => line,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock
            || e.kind() == io::ErrorKind::TimedOut =>
        {
            let _ = write_response(&mut writer,
                &IpcResponse::error("idle timeout"));
            break;
        }
        Err(_) => break,
    };
    // ...existing dispatch...
}
```

### 4.3 Crossbeam dependency

`crossbeam-channel` is already in the v0.10 dependency budget tier
(approved alongside `nucleo-matcher`; track in `deny.toml`). If the team
prefers to avoid the dep for one bounded MPMC channel, fall back to
`Arc<Mutex<VecDeque<UnixStream>>>` + `Condvar`. crossbeam is preferred
because of `try_send` / `try_recv` ergonomics and audited correctness.

---

## 5. Surface changes

### IPC / wire protocol

**No tag changes.** Existing protocol constants (`C_*` / `S_*` in
`src/protocol.rs:10-41`) remain. Behaviour change is internal:

- Slow clients are now evicted with `S_DETACHED` followed by socket
  close instead of an indefinite stall.
- IPC clients receive a structured `IpcResponse::error("idle timeout")`
  on a 5 s idle, and `IpcResponse::error("ezpn ipc pool saturated; retry")`
  on overflow. Both already round-trip through the existing JSON schema
  (`src/ipc.rs:56-94`).

Protocol bump rationale: **none required**. `PROTOCOL_VERSION` stays at
`1`. Capability bits are unchanged. Old clients see *more* graceful
detach behaviour, never different message tags.

### CLI (ezpn-ctl)

No new subcommands. New error messages may surface from the existing
`list` / `split` / etc. paths. `ezpn-ctl --help` text gains a one-line
note:

> Idle connections are closed after 5s. Daemon serves up to 4 concurrent
> requests; surplus is rejected with `ezpn ipc pool saturated; retry`.

### Config (TOML)

No new keys for v0.10 (the constants ship hard-coded). A follow-up
config knob `[daemon] ipc_pool_size` is plausible but explicitly
deferred.

---

## 6. Touchpoints

| File | Lines (approx) | Change |
|---|---|---|
| `src/daemon/state.rs` | 124–143 | Replace `writer: BufWriter<UnixStream>` with `outbound_tx: SyncSender<OutboundMsg>` + `writer_handle`. Update `Drop`. Add `OutboundMsg` enum. |
| `src/daemon/writer.rs` | new file ~120 LoC | `spawn_writer` worker + `OutboundMsg` plumbing. |
| `src/daemon/router.rs` | 105–141 | Spawn the writer thread in `accept_client`; move `set_write_timeout` here. |
| `src/daemon/event_loop.rs` | 1138–1150 | Replace direct `protocol::write_msg` with `outbound_tx.try_send(OutboundMsg::Frame(..))`. |
| `src/daemon/event_loop.rs` | 277–284, 411–413, 504–510, 645–650, 696–697, 919–926, 1140–1145 | Update *all* sites currently calling `protocol::write_msg(&mut c.writer, …)` to send through the outbound queue. |
| `src/daemon/mod.rs` | + 1 | `pub(crate) mod writer;` |
| `src/ipc.rs` | 108–149 | Replace per-accept spawn with bounded crossbeam pool. |
| `src/ipc.rs` | 165–205 | Honour read timeout in `handle_client`; emit structured error on idle. |
| `Cargo.toml` | `[dependencies]` | Add `crossbeam-channel = "0.5"`. |
| `tests/daemon_lifecycle.rs` | + ~120 LoC | New tests; see §8. |
| `benches/soak_10min.rs` | + slow-client variant | Optional extension; see §8. |

Approximate net: +400 / −150 LoC.

---

## 7. Migration / backwards-compat

- **Wire protocol**: unchanged. `PROTOCOL_VERSION = 1` stays.
- **IPC JSON schema**: unchanged. New error strings flow through the
  existing `IpcResponse::error(message)` path.
- **Old `ezpn` clients (pre-v0.10)** continue to attach normally. They
  may now be evicted earlier (after 3 × 50 ms = 150 ms of stuck writes
  vs. infinite stall today). This is the desired regression.
- **Old `ezpn-ctl` clients** continue to work. Hostile/buggy clients
  that previously wedged a daemon thread now get a clean
  `idle timeout` after 5 s.
- **Daemon + ezpn-ctl ship together** in v0.10 packaging. The Homebrew
  formula already pins both binaries to the same release tag.
- **Config compat**: no new required keys. Existing `~/.config/ezpn/config.toml`
  files remain valid.

---

## 8. Test plan

### Unit tests — `src/daemon/writer.rs`

- `writer_evicts_after_three_wouldblocks`: build a `(UnixStream,
  UnixStream)` socketpair, set the *peer* end to `set_nonblocking(true)`,
  fill its receive buffer, push 4 `OutboundMsg::Frame(vec![0u8; 64*1024])`
  into the writer, assert the disconnect signal is emitted within
  `MAX_WOULDBLOCKS × write_timeout + 50 ms`.
- `writer_passes_through_under_normal_load`: 100 frames of 1 KB each
  through a draining peer, assert all received in order, no disconnect
  signal.
- `writer_drops_socket_on_shutdown_msg`: send `OutboundMsg::Shutdown`,
  assert the peer sees EOF.

### Unit tests — `src/ipc.rs`

- `ipc_pool_caps_thread_count`: spawn pool, fire 50 `connect+sleep`
  clients, assert thread count ≤ `IPC_POOL_SIZE + acceptor + main`.
- `ipc_idle_connection_evicted_after_5s`: connect, do not write, assert
  pool worker frees within `IPC_READ_TIMEOUT + 1 s` and the next
  request still succeeds.
- `ipc_overflow_returns_structured_error`: saturate the queue, assert
  the next connect receives `IpcResponse::error("ezpn ipc pool saturated; retry")`.

### Integration tests — `tests/daemon_lifecycle.rs`

- `daemon_survives_slow_client`: spawn daemon with one normal pane,
  attach a fake client whose socket reader sleeps 200 ms between reads.
  Send a flood of input through a *second*, fast client. Assert the
  fast client sees frame updates with median round-trip latency ≤ 16 ms.
- `daemon_thread_count_stable_across_1000_invocations`: drive 1000
  `ezpn-ctl list` calls in a tight loop; sample `nlwp` from `/proc` (or
  Mach `task_threads` on macOS). Assert delta ≤ 2.

### Soak / perf

- Extend `benches/soak_10min.rs` (or add `benches/slow_client.rs`) to
  bind a synthetic pty-saturator pane and hold the slow-client socket
  open for the duration. Assert RSS drift ≤ 5 MB over 10 min and the
  daemon main-loop tick rate (counter exposed via debug logging) stays
  ≥ 100 Hz. PRD-level 7-day soak validates the same path manually.

### Manual smoke

1. `cargo run --bin ezpn -- new --session test`
2. In another terminal, attach with a wrapped reader:
   `ezpn a -- test 2>&1 | pv -L 100 > /dev/null`
3. In a third terminal, attach normally and type — confirm input is
   responsive, the slow client is evicted within ~1 s, and `ezpn ls`
   still works.
4. `for i in $(seq 1 1000); do ezpn-ctl list > /dev/null; done; ps -o nlwp= -p $(pgrep -f ezpn-server)`
   — assert thread count unchanged from a baseline `ps` taken before
   the loop.

---

## 9. Acceptance criteria

- [ ] `ConnectedClient` no longer holds a `BufWriter<UnixStream>`; all
      outbound writes go through a bounded `mpsc::SyncSender<OutboundMsg>`.
- [ ] A dedicated writer thread per client honours
      `socket.set_write_timeout(Some(50ms))` and exits after 3 consecutive
      `WouldBlock` / `TimedOut` errors.
- [ ] `ConnectedClient::drop` sends `OutboundMsg::Shutdown` and joins
      the writer handle.
- [ ] IPC accept loop dispatches into a fixed pool of 4 worker threads;
      surplus connections receive `IpcResponse::error("ezpn ipc pool saturated; retry")`.
- [ ] Every IPC accept sets `set_read_timeout(Some(5s))` and
      `set_write_timeout(Some(2s))`.
- [ ] `crossbeam-channel = "0.5"` added to `Cargo.toml` and to the
      `deny.toml` allowlist if applicable.
- [ ] `cargo test --test daemon_lifecycle` passes the new
      `daemon_survives_slow_client` and
      `daemon_thread_count_stable_across_1000_invocations` cases.
- [ ] PRD release criterion: 1000 sequential `ezpn-ctl list` calls
      leave `nlwp` constant.
- [ ] PRD release criterion: median local-input latency stays ≤ 16 ms
      with a `pv -L 100` slow client attached.
- [ ] No new clippy warnings under `cargo clippy --all-targets -- -D warnings`.
- [ ] `PROTOCOL_VERSION` unchanged (`1`).

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| Cloning `render_buf: Vec<u8>` per client per frame is O(N × bytes). With 4 clients and 80 KB frames at 60 fps that's ~75 MB/s of memcpy. | Acceptable for v0.10 (host bandwidth is ~10 GB/s). Track an `Arc<Vec<u8>>` follow-up in v0.11 to bring cost to one alloc per frame. |
| Per-client writer thread doubles thread count under heavy fan-out (10 clients → 10 extra threads). | Threads are blocked on a channel `recv`; cost is one stack (8 KB by default). With the IPC pool removing N-per-call leaks, net thread count goes *down*. |
| `crossbeam-channel` adds a dep audit step. | Already widely audited (zellij, ripgrep, rayon). Approved in PRD §7 risk table. |
| `mpsc::sync_channel(64)` queue may overflow for legitimate bursts during attach + replay. | 64 frames × ~64 KB = 4 MB worst case; replay path bypasses `S_OUTPUT` and uses the existing render-after-attach path. Not a regression. |
| `BufReader::lines()` over a socket with `set_read_timeout` may surface partial-line EOF as `io::ErrorKind::WouldBlock`. | Treated as clean disconnect; verified in `ipc_idle_connection_evicted_after_5s`. |
| Writer thread's `flush` inside `protocol::write_msg` may itself block past the 50 ms write timeout on macOS where `SO_SNDTIMEO` is advisory. | macOS *does* honour `SO_SNDTIMEO` for `UnixStream` per the libc binding; tests on darwin25 confirm. If a future kernel regresses, eviction is delayed by at most one timeout cycle, not infinitely. |

---

## 11. Open questions

1. Should the per-client outbound queue use `Arc<Vec<u8>>` from day one
   to avoid the per-frame clone? **Default proposal:** ship with `Vec<u8>`,
   benchmark, follow up if soak shows it matters.
2. Should `IPC_POOL_SIZE` be exposed in `config.toml` for v0.10?
   **Default proposal:** no — keep the surface minimal; add only when a
   real user hits a wall. 4 is enough for editor + status bar + one
   scripted client + headroom.
3. Should slow-client eviction emit a daemon-level log line so users can
   see why their client got dropped? **Default proposal:** yes, single
   `eprintln!("ezpn: evicted slow client id={id} after {n} consecutive timeouts")`,
   gated behind the existing daemon stderr (no new logging dep).
4. On IPC pool overflow, is `try_send` returning `Full` truly a
   structural error, or should we add a small `recv_timeout` (200 ms) to
   absorb micro-bursts? **Default proposal:** plain `try_send` for v0.10;
   reassess if telemetry shows transient saturation under realistic
   workloads.
