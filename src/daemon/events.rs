//! SPEC 07 — Event subscription bus.
//!
//! Owns the daemon-side fan-out for `S_EVENT` notifications. The bus is
//! consumed exclusively by the main loop (single-thread access through the
//! daemon's borrow disciplines), so it does not need internal locking.
//!
//! Per-subscriber pipeline:
//!
//! ```text
//!  main loop ──emit(envelope)──► EventBus
//!                                   │
//!                                   ▼ for each matching subscriber:
//!                                 sync_channel(QUEUE_CAPACITY)
//!                                   │   try_send (drop-oldest on full)
//!                                   ▼
//!                                 worker thread
//!                                   │  serde_json::to_vec(envelope)
//!                                   │  protocol::write_msg(conn, S_EVENT, …)
//!                                   ▼
//!                                 client socket
//! ```
//!
//! Drop-oldest semantics (SPEC 07 §4.2):
//! 1. Per-subscriber queue is a `Mutex<VecDeque<OutboundEvent>>` with
//!    capacity `QUEUE_CAPACITY` so the *sender* (main loop) can pop the
//!    oldest entry to make room. `mpsc::sync_channel` cannot do this —
//!    its `try_send` only reports `Full`, leaving the producer with
//!    drop-newest as the only option. With drop-oldest the consumer
//!    always observes the latest state, which matches what subscribers
//!    actually want from a reactive stream.
//! 2. Each oldest pop bumps `Subscriber::dropped_since`. The next emit
//!    prepends a synthetic `S_EVENT_OVERFLOW` envelope carrying the
//!    cumulative drop count and resets the counter.
//! 3. The overflow notice itself is also subject to drop-oldest. In
//!    pathological backpressure the notice may be displaced before the
//!    worker drains it; the next emit will re-issue with a higher count.
//!
//! Diagnostic / reactive events are not transactional — losing a few
//! `pane.resized` is acceptable; freezing the main loop on a wedged
//! consumer is not.

use std::collections::{HashSet, VecDeque};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::protocol::{
    self, EventEnvelope, EventTopic, SubscribeOk, S_EVENT, S_EVENT_OVERFLOW, S_SUBSCRIBE_OK,
};

/// Per-subscriber backlog cap. Sized for ~250 ms of bursty activity at
/// 1 kHz; overflow is harmless because envelopes are diagnostic.
pub(crate) const QUEUE_CAPACITY: usize = 256;

/// One queued envelope. The worker thread serializes lazily on its own
/// thread so the main loop never pays the JSON cost.
struct OutboundEvent {
    /// `true` for the synthetic `S_EVENT_OVERFLOW` notice. Routes the
    /// payload through the alternate tag so consumers can branch on it
    /// without parsing JSON.
    overflow: bool,
    envelope: EventEnvelope,
}

/// Per-subscriber bounded queue with drop-oldest semantics. Mutex+Condvar
/// over `VecDeque` because std `mpsc::sync_channel` does not let the
/// producer pop the oldest entry when full — it can only abandon the new
/// one. Drop-oldest is what reactive consumers actually want: under
/// backpressure they observe the most recent state, not a stale prefix.
pub(crate) struct EventQueue {
    inner: Mutex<EventQueueInner>,
    cv: Condvar,
}

struct EventQueueInner {
    queue: VecDeque<OutboundEvent>,
    closed: bool,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            inner: Mutex::new(EventQueueInner {
                queue: VecDeque::with_capacity(QUEUE_CAPACITY),
                closed: false,
            }),
            cv: Condvar::new(),
        }
    }

    /// Push with drop-oldest semantics. Returns `Ok(true)` on a clean push,
    /// `Ok(false)` if an oldest entry was kicked out to make room (caller
    /// should bump its overflow counter), `Err(())` if the queue is closed
    /// (subscriber is being dropped — a racy emit can hit this and the bus
    /// reaps the subscriber on the next loop tick).
    fn push_drop_oldest(&self, evt: OutboundEvent) -> Result<bool, ()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed {
            return Err(());
        }
        let clean = inner.queue.len() < QUEUE_CAPACITY;
        if !clean {
            // Drop-oldest: pop the front so the new event lands at the back.
            inner.queue.pop_front();
        }
        inner.queue.push_back(evt);
        self.cv.notify_one();
        Ok(clean)
    }

    /// Block until an event arrives or the queue is closed.
    fn pop_blocking(&self) -> Option<OutboundEvent> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            if let Some(evt) = inner.queue.pop_front() {
                return Some(evt);
            }
            if inner.closed {
                return None;
            }
            inner = self.cv.wait(inner).unwrap();
        }
    }

    /// Mark closed and wake the worker so it can exit `pop_blocking`.
    fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        self.cv.notify_all();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }
}

/// One active subscriber. Created by `EventBus::register` after a
/// successful `C_SUBSCRIBE` handshake.
pub(crate) struct Subscriber {
    pub(crate) id: u64,
    topics: HashSet<EventTopic>,
    /// Shared with the worker thread. `Option`-wrapped so `Drop` can
    /// `.take()`-and-close before joining (without it, the worker would
    /// stay parked in `pop_blocking` forever).
    queue: Option<Arc<EventQueue>>,
    /// Cumulative drops since the last `S_EVENT_OVERFLOW` notice was
    /// shipped. Reset to 0 when an overflow notice is enqueued (best
    /// effort — the notice itself is subject to drop-oldest, and the
    /// next emit re-issues with a fresh count if needed).
    dropped_since: u64,
    /// Worker handle — `take()`-d at reap time so `Drop` is idempotent.
    handle: Option<JoinHandle<()>>,
    /// Optional filter (per SPEC 07 §4.2 — only `session` honoured today).
    filter_session: Option<String>,
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        // Close the queue FIRST so the worker's `pop_blocking()` returns
        // `None` and the loop exits. Only then is it safe to join the
        // handle — otherwise we'd deadlock (see the `queue` field doc).
        if let Some(q) = self.queue.take() {
            q.close();
            // Drop our Arc; the worker thread holds the other reference
            // and will release it on exit.
            drop(q);
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Daemon-side event fan-out. Single-thread access from the main loop.
pub(crate) struct EventBus {
    subscribers: Vec<Subscriber>,
    next_id: u64,
    session_name: String,
}

impl EventBus {
    pub(crate) fn new(session_name: impl Into<String>) -> Self {
        Self {
            subscribers: Vec::new(),
            next_id: 1,
            session_name: session_name.into(),
        }
    }

    /// Read-only accessor for the session name baked into every envelope.
    /// Currently used only by tests; kept on the public surface so future
    /// consumers (CLI subcommand, debug logs) don't need to thread it
    /// through separately.
    #[allow(dead_code)]
    pub(crate) fn session_name(&self) -> &str {
        &self.session_name
    }

    /// `true` if any active subscriber is interested in `topic`. Lets the
    /// main loop skip building an envelope (which would clone strings) when
    /// nobody is listening.
    pub(crate) fn has_subscriber_for(&self, topic: EventTopic) -> bool {
        self.subscribers.iter().any(|s| s.topics.contains(&topic))
    }

    /// Register a new subscriber after the `C_SUBSCRIBE` handshake. Sends
    /// `S_SUBSCRIBE_OK` synchronously in the caller (the daemon main loop)
    /// so the ack is on the wire before any `S_EVENT` from a concurrent
    /// emit. The connection is then moved into the per-subscriber worker
    /// thread, which only ships `S_EVENT` / `S_EVENT_OVERFLOW` frames.
    /// Returns the new subscriber id, or `None` if writing the ack failed
    /// (consumer hung up between hello and subscribe).
    pub(crate) fn register(
        &mut self,
        topics: Vec<EventTopic>,
        filter_session: Option<String>,
        mut conn: UnixStream,
    ) -> Option<u64> {
        let id = self.next_id;
        let ack = SubscribeOk {
            subscriber_id: id,
            topics: topics.clone(),
        };
        let ack_bytes = match serde_json::to_vec(&ack) {
            Ok(b) => b,
            Err(_) => return None,
        };
        if protocol::write_msg(&mut conn, S_SUBSCRIBE_OK, &ack_bytes).is_err() {
            return None;
        }
        self.next_id += 1;
        let queue = Arc::new(EventQueue::new());
        let queue_for_worker = Arc::clone(&queue);
        let handle = std::thread::Builder::new()
            .name(format!("ezpn-events-{id}"))
            .spawn(move || run_subscriber(id, conn, queue_for_worker))
            .expect("spawn event subscriber thread");
        self.subscribers.push(Subscriber {
            id,
            topics: topics.into_iter().collect(),
            queue: Some(queue),
            dropped_since: 0,
            handle: Some(handle),
            filter_session,
        });
        Some(id)
    }

    /// Emit one event to all matching subscribers. The main loop uses this
    /// directly; per-topic helpers (`emit_pane`, …) just build the
    /// envelope and call through.
    pub(crate) fn emit(&mut self, envelope_topic: EventTopic, envelope: EventEnvelope) {
        let session_filter = &envelope.session;
        for sub in &mut self.subscribers {
            if !sub.topics.contains(&envelope_topic) {
                continue;
            }
            if let Some(want) = &sub.filter_session {
                if want != session_filter {
                    continue;
                }
            }
            let outbound = OutboundEvent {
                overflow: false,
                envelope: envelope.clone(),
            };
            send_with_drop_oldest(sub, outbound);
        }
    }

    /// Reap subscribers whose worker thread has exited (socket closed or
    /// channel disconnected). Called once per main-loop iteration so dead
    /// subscribers do not accumulate.
    pub(crate) fn reap_dead(&mut self) {
        self.subscribers.retain(|s| {
            // `is_finished` is the cheapest way to ask the runtime whether
            // a thread has joined. A finished worker means the socket is
            // gone (EPIPE, EOF, or explicit close on the consumer side).
            match s.handle.as_ref() {
                Some(h) => !h.is_finished(),
                None => false,
            }
        });
    }

    /// Active subscriber count. Test/diagnostic only.
    #[allow(dead_code)]
    pub(crate) fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    // ── Per-topic emit helpers ──────────────────────────────────────────
    //
    // Each helper short-circuits on `!has_subscriber_for(topic)` so the
    // main loop never builds an envelope (which clones strings) when no
    // subscriber is interested. Borderline cases — clients with topic
    // mid-add — are handled by `emit` itself doing one final per-subscriber
    // membership check.

    /// SPEC 07 §4.5 `pane.created` envelope. Wired up in a follow-up PR
    /// once we land the source-side pane-spawn detection (see #39).
    #[allow(dead_code)]
    pub(crate) fn emit_pane_created(
        &mut self,
        pane_id: usize,
        tab_index: usize,
        command: &str,
        cols: u16,
        rows: u16,
    ) {
        if !self.has_subscriber_for(EventTopic::Pane) {
            return;
        }
        self.emit(
            EventTopic::Pane,
            EventEnvelope {
                v: 1,
                ts: EventEnvelope::now_ts(),
                topic: "pane",
                type_: "pane.created",
                session: self.session_name.clone(),
                data: serde_json::json!({
                    "pane_id": pane_id,
                    "tab_index": tab_index,
                    "command": command,
                    "cols": cols,
                    "rows": rows,
                }),
            },
        );
    }

    /// SPEC 07 §4.5 `pane.exited` envelope. Wired up in a follow-up PR
    /// once we hook into the SIGCHLD/restart path.
    #[allow(dead_code)]
    pub(crate) fn emit_pane_exited(
        &mut self,
        pane_id: usize,
        tab_index: usize,
        exit_code: Option<u32>,
    ) {
        if !self.has_subscriber_for(EventTopic::Pane) {
            return;
        }
        self.emit(
            EventTopic::Pane,
            EventEnvelope {
                v: 1,
                ts: EventEnvelope::now_ts(),
                topic: "pane",
                type_: "pane.exited",
                session: self.session_name.clone(),
                data: serde_json::json!({
                    "pane_id": pane_id,
                    "tab_index": tab_index,
                    "exit_code": exit_code,
                }),
            },
        );
    }

    pub(crate) fn emit_client_attached(
        &mut self,
        client_id: u64,
        mode: protocol::AttachMode,
        cols: u16,
        rows: u16,
    ) {
        if !self.has_subscriber_for(EventTopic::Client) {
            return;
        }
        let mode_str = match mode {
            protocol::AttachMode::Steal => "steal",
            protocol::AttachMode::Shared => "shared",
            protocol::AttachMode::Readonly => "readonly",
        };
        self.emit(
            EventTopic::Client,
            EventEnvelope {
                v: 1,
                ts: EventEnvelope::now_ts(),
                topic: "client",
                type_: "client.attached",
                session: self.session_name.clone(),
                data: serde_json::json!({
                    "client_id": client_id,
                    "mode": mode_str,
                    "cols": cols,
                    "rows": rows,
                }),
            },
        );
    }

    pub(crate) fn emit_client_detached(&mut self, client_id: u64, reason: &str) {
        if !self.has_subscriber_for(EventTopic::Client) {
            return;
        }
        self.emit(
            EventTopic::Client,
            EventEnvelope {
                v: 1,
                ts: EventEnvelope::now_ts(),
                topic: "client",
                type_: "client.detached",
                session: self.session_name.clone(),
                data: serde_json::json!({
                    "client_id": client_id,
                    "reason": reason,
                }),
            },
        );
    }

    pub(crate) fn emit_tab_switched(&mut self, from_index: usize, to_index: usize, name: &str) {
        if !self.has_subscriber_for(EventTopic::Tab) {
            return;
        }
        self.emit(
            EventTopic::Tab,
            EventEnvelope {
                v: 1,
                ts: EventEnvelope::now_ts(),
                topic: "tab",
                type_: "tab.switched",
                session: self.session_name.clone(),
                data: serde_json::json!({
                    "from_index": from_index,
                    "to_index": to_index,
                    "name": name,
                }),
            },
        );
    }
}

/// SPEC 07 §4.2 drop-oldest enqueue. Returns `true` on a clean push;
/// `false` if the queue was full and an oldest entry was kicked out
/// (which the caller will surface via the next overflow notice).
fn send_with_drop_oldest(sub: &mut Subscriber, evt: OutboundEvent) -> bool {
    let Some(queue) = sub.queue.as_ref() else {
        return false;
    };
    // Flush any pending overflow notice ahead of the new event so the
    // consumer sees the gap marker before the next "normal" envelope.
    if sub.dropped_since > 0 {
        let notice = OutboundEvent {
            overflow: true,
            envelope: EventEnvelope {
                v: 1,
                ts: EventEnvelope::now_ts(),
                topic: "_meta",
                type_: "overflow",
                session: evt.envelope.session.clone(),
                data: serde_json::json!({
                    "dropped": sub.dropped_since,
                    "subscriber_id": sub.id,
                }),
            },
        };
        // The notice itself is subject to drop-oldest. Either way we
        // reset the counter — the next emit will re-issue with a fresh
        // count if more drops accumulate.
        let _ = queue.push_drop_oldest(notice);
        sub.dropped_since = 0;
    }
    match queue.push_drop_oldest(evt) {
        Ok(true) => true,
        Ok(false) => {
            // We just kicked out an oldest envelope — bump the counter so
            // the next emit prepends an overflow notice.
            sub.dropped_since = sub.dropped_since.saturating_add(1);
            false
        }
        Err(()) => {
            // Queue closed (subscriber being dropped). `reap_dead` will
            // remove this subscriber on the next loop tick.
            false
        }
    }
}

/// Per-subscriber worker thread. Drains the bounded queue, serializes
/// each envelope into JSON, and ships it through the binary protocol.
/// Exits cleanly when the queue closes (subscriber dropped) or the
/// socket write fails (consumer hung up).
fn run_subscriber(_id: u64, mut conn: UnixStream, queue: Arc<EventQueue>) {
    while let Some(evt) = queue.pop_blocking() {
        let bytes = match serde_json::to_vec(&evt.envelope) {
            Ok(b) => b,
            Err(_) => continue, // a malformed envelope is a bug, not fatal
        };
        let tag = if evt.overflow {
            S_EVENT_OVERFLOW
        } else {
            S_EVENT
        };
        if protocol::write_msg(&mut conn, tag, &bytes).is_err() {
            // Consumer closed the socket — exit so `reap_dead` removes us.
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream as Std;
    use std::time::Duration;

    fn pane_envelope(session: &str, pane_id: u64) -> EventEnvelope {
        EventEnvelope {
            v: 1,
            ts: EventEnvelope::now_ts(),
            topic: "pane",
            type_: "pane.created",
            session: session.to_string(),
            data: serde_json::json!({"pane_id": pane_id, "cols": 80, "rows": 24}),
        }
    }

    #[test]
    fn register_then_emit_writes_one_frame() {
        let (peer_a, peer_b) = Std::pair().unwrap();
        let mut bus = EventBus::new("test");
        let _id = bus.register(vec![EventTopic::Pane], None, peer_a);

        // register() writes S_SUBSCRIBE_OK synchronously before spawning
        // the worker — drain it first so the next read is the event.
        peer_b
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let (ack_tag, _) = protocol::read_msg(&mut &peer_b).expect("read S_SUBSCRIBE_OK");
        assert_eq!(ack_tag, S_SUBSCRIBE_OK);

        bus.emit(EventTopic::Pane, pane_envelope("test", 7));

        let (tag, payload) = protocol::read_msg(&mut &peer_b).expect("read S_EVENT");
        assert_eq!(tag, S_EVENT);
        let json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(json["topic"], "pane");
        assert_eq!(json["type"], "pane.created");
        assert_eq!(json["data"]["pane_id"], 7);
    }

    #[test]
    fn topic_filter_skips_non_matching_subscriber() {
        let (peer_a, peer_b) = Std::pair().unwrap();
        let mut bus = EventBus::new("test");
        let _ = bus.register(vec![EventTopic::Tab], None, peer_a);
        // Drain the synchronous ack — verifying the topic filter requires
        // checking that NO further frames arrive.
        peer_b
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let (ack_tag, _) = protocol::read_msg(&mut &peer_b).expect("ack");
        assert_eq!(ack_tag, S_SUBSCRIBE_OK);

        bus.emit(EventTopic::Pane, pane_envelope("test", 1));

        // Non-matching topic → no S_EVENT. Drop the bus first so the
        // worker channel disconnects and the peer reads EOF instead of
        // blocking on the read timeout.
        peer_b
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        drop(bus);
        let mut buf = [0u8; 1];
        let n = (&peer_b).read(&mut buf).unwrap_or(0);
        assert_eq!(n, 0, "non-matching topic must not produce a frame");
    }

    #[test]
    fn session_filter_skips_other_sessions() {
        let (peer_a, peer_b) = Std::pair().unwrap();
        let mut bus = EventBus::new("test");
        let _ = bus.register(
            vec![EventTopic::Pane],
            Some("other-session".to_string()),
            peer_a,
        );
        peer_b
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let (ack_tag, _) = protocol::read_msg(&mut &peer_b).expect("ack");
        assert_eq!(ack_tag, S_SUBSCRIBE_OK);

        // Bus session is "test"; subscriber filter is "other-session" —
        // envelope's session field is "test" so the filter rejects.
        bus.emit(EventTopic::Pane, pane_envelope("test", 1));

        peer_b
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        drop(bus);
        let mut buf = [0u8; 1];
        let n = (&peer_b).read(&mut buf).unwrap_or(0);
        assert_eq!(n, 0, "session filter must drop non-matching event");
    }

    #[test]
    fn reap_dead_drops_disconnected_subscriber() {
        let (peer_a, peer_b) = Std::pair().unwrap();
        let mut bus = EventBus::new("test");
        let _ = bus.register(vec![EventTopic::Pane], None, peer_a);
        assert_eq!(bus.subscriber_count(), 1);

        // Consumer hangs up.
        drop(peer_b);
        // Force the worker to attempt a write so it observes EPIPE.
        bus.emit(EventTopic::Pane, pane_envelope("test", 1));
        // Allow the worker thread to actually exit before reaping.
        std::thread::sleep(Duration::from_millis(50));
        bus.reap_dead();
        assert_eq!(bus.subscriber_count(), 0, "dead subscriber must be reaped");
    }

    #[test]
    fn event_queue_drops_oldest_when_full() {
        // Push QUEUE_CAPACITY + 5 envelopes; the first 5 must be displaced
        // (drop-oldest) and the queue must contain envelopes 5..QC+5.
        let q = EventQueue::new();
        for i in 0..(QUEUE_CAPACITY + 5) {
            let evt = OutboundEvent {
                overflow: false,
                envelope: EventEnvelope {
                    v: 1,
                    ts: i as u64,
                    topic: "pane",
                    type_: "pane.created",
                    session: "test".to_string(),
                    data: serde_json::json!({ "i": i }),
                },
            };
            let _ = q.push_drop_oldest(evt);
        }
        assert_eq!(q.len(), QUEUE_CAPACITY);

        // Close so `pop_blocking` returns None once the queue drains.
        q.close();

        // First entry must be #5 (entries 0..=4 were kicked out).
        let first = q.pop_blocking().expect("non-empty after fill");
        assert_eq!(first.envelope.data["i"].as_u64(), Some(5));

        // Remaining entries must be strictly increasing in `i` and total
        // QUEUE_CAPACITY - 1 (we already consumed one).
        let mut last_i = 5u64;
        let mut count = 1usize;
        while let Some(evt) = q.pop_blocking() {
            let i = evt.envelope.data["i"].as_u64().unwrap();
            assert!(i > last_i, "drop-oldest must preserve insertion order");
            last_i = i;
            count += 1;
        }
        assert_eq!(count, QUEUE_CAPACITY);
        assert_eq!(last_i, (QUEUE_CAPACITY + 4) as u64);
    }

    #[test]
    fn event_queue_close_unblocks_waiter() {
        // A worker parked in `pop_blocking` on an empty queue must wake
        // and return None when the queue is closed (mirrors the Drop path).
        let q = Arc::new(EventQueue::new());
        let qc = Arc::clone(&q);
        let handle = std::thread::spawn(move || qc.pop_blocking());
        // Give the spawned thread a moment to enter pop_blocking.
        std::thread::sleep(Duration::from_millis(50));
        q.close();
        let result = handle.join().expect("thread didn't panic");
        assert!(result.is_none(), "close must unblock the waiter with None");
    }

    #[test]
    fn has_subscriber_for_reflects_topic_membership() {
        let (peer_a, _peer_b) = Std::pair().unwrap();
        let mut bus = EventBus::new("test");
        let _ = bus.register(vec![EventTopic::Pane, EventTopic::Tab], None, peer_a);

        assert!(bus.has_subscriber_for(EventTopic::Pane));
        assert!(bus.has_subscriber_for(EventTopic::Tab));
        assert!(!bus.has_subscriber_for(EventTopic::Layout));
    }
}
