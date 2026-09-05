//! Per-client connection lifecycle: accept, framing, reader thread,
//! detach/disconnect bookkeeping, and the path-socket bind helper.
//!
//! ## Lock & state ordering
//!
//! Per-client readers post events to the main loop. Output workers take
//! queued byte blocks under a short mutex, then release it before I/O.
//! They never acquire pane, layout, or client-list state.
//!
//! When new shared state is introduced, follow this order to avoid
//! cycles:
//!
//! 1. Acquire **`clients`** (the `Vec<ConnectedClient>` the main loop
//!    owns) before any per-client reader state. Reader threads can
//!    only post into the mpsc channel; they never look back at the
//!    `ConnectedClient` they were spawned for.
//! 2. Acquire **`sessions`** / pane state (`HashMap<usize, Pane>`,
//!    `Layout`, `TabManager`) AFTER `clients`. The accept path
//!    (`accept_client`) already follows this order: it mutates the
//!    `clients` vector before touching `panes` for the resize side
//!    effect.
//! 3. The crossterm event channel (per-client `event_rx`) is
//!    drained in the main loop while `clients` is borrowed
//!    `&mut`; never re-borrow `clients` from inside that drain.
//!
//! Stick to the `clients` -> `sessions` direction in any new helper.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crossterm::event::Event;

use crate::layout::Layout;
use crate::pane::Pane;
use crate::protocol;
use crate::settings::Settings;

use super::input_modes::DragState;
use super::RenderUpdate;

/// Client message from the reader thread.
pub(super) enum ClientMsg {
    Event(Event),
    Resize(u16, u16),
    Detach,
    Disconnected,
    /// Kill the server (from `ezpn kill`).
    Kill,
}

/// Connected client with attach mode and per-client state.
pub(super) struct ConnectedClient {
    pub(super) id: u64,
    pub(super) writer: QueuedWriter,
    pub(super) event_rx: ClientReceiver,
    pub(super) mode: protocol::AttachMode,
    pub(super) tw: u16,
    pub(super) th: u16,
}

const MAX_CLIENTS: usize = 32;
const QUEUE_BYTES: usize = protocol::MAX_PAYLOAD + 5;
const OUTPUT_BLOCK_BYTES: usize = 64 * 1024;
static OUTPUT_WORKERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct OutputBlocks {
    blocks: VecDeque<Vec<u8>>,
    closed: bool,
}

#[derive(Default)]
struct OutputQueue {
    state: Mutex<OutputBlocks>,
    ready: Condvar,
}

struct OutputWorkerGuard;
impl Drop for OutputWorkerGuard {
    fn drop(&mut self) {
        OUTPUT_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Frames are enqueued only at flush boundaries, preserving v1 framing.
/// No daemon-main-loop write waits for a terminal or SSH transport.
pub(super) struct QueuedWriter {
    socket: UnixStream,
    queue: Arc<OutputQueue>,
    pending: Vec<u8>,
    queued_bytes: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
}

impl QueuedWriter {
    fn new(socket: UnixStream) -> io::Result<Self> {
        OUTPUT_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 64).then_some(n + 1)
            })
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "too many client transports"))?;
        let guard = OutputWorkerGuard;
        let worker_socket = socket.try_clone()?;
        let queue = Arc::new(OutputQueue::default());
        let worker_queue = Arc::clone(&queue);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::clone(&queued_bytes);
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = Arc::clone(&failed);
        std::thread::Builder::new()
            .name("ezpn-output".into())
            .spawn(move || {
                let _guard = guard;
                loop {
                    let frame = {
                        let mut state = worker_queue.state.lock().unwrap();
                        while state.blocks.is_empty() && !state.closed {
                            state = worker_queue.ready.wait(state).unwrap();
                        }
                        let Some(frame) = state.blocks.pop_front() else {
                            break;
                        };
                        frame
                    };
                    let result =
                        protocol::DeadlineStream::new(&worker_socket, Duration::from_secs(2))
                            .write_all(&frame);
                    bytes.fetch_sub(frame.len(), Ordering::AcqRel);
                    if let Err(error) = result {
                        tracing::warn!(%error, "client output transport stopped");
                        break;
                    }
                }
                worker_failed.store(true, Ordering::Release);
                let mut state = worker_queue.state.lock().unwrap();
                state.closed = true;
                let remaining: usize = state.blocks.drain(..).map(|block| block.len()).sum();
                bytes.fetch_sub(remaining, Ordering::AcqRel);
                drop(state);
                let _ = worker_socket.shutdown(std::net::Shutdown::Both);
                crate::pane::wake_main_loop();
            })?;
        Ok(Self {
            socket,
            queue,
            pending: Vec::new(),
            queued_bytes,
            failed,
        })
    }

    fn fail(&self) -> io::Error {
        tracing::warn!(
            queued_bytes = self.queued_bytes.load(Ordering::Relaxed),
            pending_bytes = self.pending.len(),
            "client output queue unavailable or full"
        );
        self.failed.store(true, Ordering::Release);
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "client output queue unavailable or full",
        )
    }
}

impl Write for QueuedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.failed.load(Ordering::Acquire) || buf.len() > QUEUE_BYTES - self.pending.len() {
            return Err(self.fail());
        }
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.failed.load(Ordering::Acquire) {
            return Err(self.fail());
        }
        if self.pending.is_empty() {
            return Ok(());
        }
        let len = self.pending.len();
        let mut state = self.queue.state.lock().unwrap();
        if state.closed || self.failed.load(Ordering::Acquire) {
            return Err(self.fail());
        }
        if self
            .queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(len).filter(|sum| *sum <= QUEUE_BYTES)
            })
            .is_err()
        {
            return Err(self.fail());
        }
        let frame = std::mem::take(&mut self.pending);
        // Preserve every framed byte, including incremental ANSI and detach.
        // Coalescing small frames bounds allocation overhead by bytes rather
        // than disconnecting healthy consumers on a transient frame-count cap.
        if let Some(tail) = state
            .blocks
            .back_mut()
            .filter(|tail| tail.len().saturating_add(frame.len()) <= OUTPUT_BLOCK_BYTES)
        {
            tail.extend_from_slice(&frame);
        } else {
            state.blocks.push_back(frame);
        }
        drop(state);
        self.queue.ready.notify_one();
        Ok(())
    }
}

impl Drop for QueuedWriter {
    fn drop(&mut self) {
        // Wake the input reader now, but let the writer drain an already
        // queued S_DETACHED/S_EXIT before it closes the write half.
        let _ = self.socket.shutdown(std::net::Shutdown::Read);
        self.queue.state.lock().unwrap().closed = true;
        self.queue.ready.notify_one();
    }
}

pub(super) struct ClientReceiver {
    rx: mpsc::Receiver<(ClientMsg, usize)>,
    bytes: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
}

impl Drop for ClientReceiver {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

impl ClientReceiver {
    pub(super) fn try_recv(&self) -> Result<ClientMsg, mpsc::TryRecvError> {
        self.rx.try_recv().map(|(msg, size)| {
            self.bytes.fetch_sub(size, Ordering::AcqRel);
            msg
        })
    }
}

/// One shared shutdown budget for every client, never N times a timeout.
pub(super) fn drain_output(clients: &[ConnectedClient], timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline
        && clients.iter().any(|client| {
            client.writer.queued_bytes.load(Ordering::Acquire) > 0
                && !client.writer.failed.load(Ordering::Acquire)
        })
    {
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Compute the effective terminal size from all active clients.
/// Uses smallest-client policy (like tmux).
pub(super) fn effective_size(clients: &[ConnectedClient]) -> (u16, u16) {
    let has_writer = clients
        .iter()
        .any(|c| c.mode != protocol::AttachMode::Readonly);
    clients
        .iter()
        .filter(|c| !has_writer || c.mode != protocol::AttachMode::Readonly)
        .map(|c| protocol::normalize_size(c.tw, c.th))
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)))
        .unwrap_or((80, 24))
}

static NEXT_CLIENT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Bind the session socket as a pathname-based Unix socket with the
/// hardening steps required by issue #65: validate the parent dir,
/// tighten `umask` to `0o077` across the `bind` call, then chmod the
/// inode to `0o600` and assert ownership. Returns the bound listener
/// in non-blocking mode.
pub(super) fn bind_path_socket(sock_path: &std::path::Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = sock_path.parent() {
        crate::socket_security::harden_socket_dir(parent)?;
    }
    crate::socket_security::remove_stale_socket(sock_path)?;

    // SAFETY: umask() is a per-process setting; restoring the prior mask
    // is correct as long as we don't bind concurrently from another
    // thread, which we don't (this runs single-threaded during startup).
    let prev_umask = unsafe { libc::umask(0o077) };
    let bind_result = UnixListener::bind(sock_path);
    unsafe {
        libc::umask(prev_umask);
    }
    let listener = bind_result?;
    listener.set_nonblocking(true)?;

    crate::socket_security::fix_socket_permissions(sock_path)?;
    Ok(listener)
}

/// Reader thread for client socket messages.
fn client_reader(
    stream: UnixStream,
    tx: mpsc::SyncSender<(ClientMsg, usize)>,
    bytes: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
) {
    'frames: while let Ok((tag, payload)) = protocol::read_socket_msg(&stream) {
        let msg = match tag {
            protocol::C_EVENT => {
                serde_json::from_slice::<Event>(&payload)
                    .ok()
                    .map(|event| match event {
                        Event::Resize(w, h) => {
                            let (w, h) = protocol::normalize_size(w, h);
                            ClientMsg::Resize(w, h)
                        }
                        event => ClientMsg::Event(event),
                    })
            }
            protocol::C_RESIZE => protocol::decode_resize(&payload).map(|(w, h)| {
                let (w, h) = protocol::normalize_size(w, h);
                ClientMsg::Resize(w, h)
            }),
            protocol::C_DETACH if payload.is_empty() => Some(ClientMsg::Detach),
            protocol::C_KILL if payload.is_empty() => Some(ClientMsg::Kill),
            _ => None,
        };
        if let Some(msg) = msg {
            let size = payload.len().max(64);
            while bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    n.checked_add(size).filter(|sum| *sum <= QUEUE_BYTES)
                })
                .is_err()
            {
                if closed.load(Ordering::Acquire) {
                    break 'frames;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            // A full input queue backpressures ordinary pasted commands.
            if tx.send((msg, size)).is_err() {
                bytes.fetch_sub(size, Ordering::AcqRel);
                let _ = stream.shutdown(std::net::Shutdown::Both);
                break;
            }
            crate::pane::wake_main_loop(); // Wake server loop
        }
    }
    let _ = tx.try_send((ClientMsg::Disconnected, 0));
    crate::pane::wake_main_loop();
}

/// Accept a new client connection, handling steal/shared/readonly modes.
#[allow(clippy::too_many_arguments)]
pub(super) fn accept_client(
    conn: UnixStream,
    new_w: u16,
    new_h: u16,
    mode: protocol::AttachMode,
    clients: &mut Vec<ConnectedClient>,
    panes: &mut HashMap<usize, Pane>,
    layout: &Layout,
    settings: &Settings,
    tw: &mut u16,
    th: &mut u16,
    drag: &mut Option<DragState>,
    zoomed_pane: Option<usize>,
    update: &mut RenderUpdate,
) {
    if mode != protocol::AttachMode::Steal && clients.len() >= MAX_CLIENTS {
        return;
    }
    let Ok(read_conn) = conn.try_clone() else {
        return;
    };
    if conn.set_nonblocking(false).is_err() {
        return;
    }
    let Ok(writer) = QueuedWriter::new(conn) else {
        return;
    };
    let bytes = Arc::new(AtomicUsize::new(0));
    let reader_bytes = Arc::clone(&bytes);
    let closed = Arc::new(AtomicBool::new(false));
    let reader_closed = Arc::clone(&closed);
    let (msg_tx, msg_rx) = mpsc::sync_channel(64);
    if std::thread::Builder::new()
        .name("ezpn-input".into())
        .spawn(move || {
            client_reader(read_conn, msg_tx, reader_bytes, reader_closed);
        })
        .is_err()
    {
        return;
    }

    // Steal mode: detach all existing clients
    if mode == protocol::AttachMode::Steal {
        for c in clients.iter_mut() {
            let _ = protocol::write_msg(&mut c.writer, protocol::S_DETACHED, &[]);
        }
        drain_output(clients, Duration::from_millis(100));
        clients.clear();
    }

    // Set up the new client
    {
        let client_id = NEXT_CLIENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        clients.push(ConnectedClient {
            id: client_id,
            writer,
            event_rx: ClientReceiver {
                rx: msg_rx,
                bytes,
                closed,
            },
            mode,
            tw: protocol::normalize_size(new_w, new_h).0,
            th: protocol::normalize_size(new_w, new_h).1,
        });
    }

    // Recompute effective size and resize panes
    let (ew, eh) = effective_size(clients);
    if ew != *tw || eh != *th {
        *tw = ew;
        *th = eh;
        *drag = None;
        crate::resize_all(panes, layout, *tw, *th, settings);
        if let Some(zpid) = zoomed_pane {
            crate::resize_zoomed_pane(panes, zpid, *tw, *th, settings);
        }
    }

    // Force full redraw for new client
    update.mark_all(layout);
    update.border_dirty = true;

    // The outer client owns one crossterm keyboard-protocol push/pop pair.
    // Pane keyboard state is used to encode PTY input, not pushed onto the
    // outer terminal once per pane (which leaked stack entries on detach).
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn input_burst_larger_than_queue_is_delivered_without_disconnect() {
        let (server, mut peer) = UnixStream::pair().unwrap();
        peer.set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let bytes = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let receiver = ClientReceiver {
            rx,
            bytes: Arc::clone(&bytes),
            closed: Arc::clone(&closed),
        };
        let worker = std::thread::spawn(move || client_reader(server, tx, bytes, closed));
        // Linux accounts socket writes by packet overhead, not just payload
        // bytes. Drain concurrently so the test cannot deadlock the producer
        // against the deliberately one-slot application queue.
        let producer = std::thread::spawn(move || {
            for width in 1..=256 {
                protocol::write_msg(
                    &mut peer,
                    protocol::C_RESIZE,
                    &protocol::encode_resize(width, 24),
                )
                .unwrap();
            }
        });
        for expected in 1..=256 {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                match receiver.try_recv() {
                    Ok(ClientMsg::Resize(width, 24)) => {
                        assert_eq!(width, expected);
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    _ => panic!("input burst was disconnected or lost"),
                }
            }
        }
        // Drop closes backpressure even on an assertion panic; the peer has
        // its own write deadline, so neither worker can wait indefinitely.
        drop(receiver);
        producer.join().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn dropping_receiver_cancels_input_backpressure() {
        let (server, mut peer) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let bytes = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let receiver = ClientReceiver {
            rx,
            bytes: Arc::clone(&bytes),
            closed: Arc::clone(&closed),
        };
        let worker_bytes = Arc::clone(&bytes);
        let worker = std::thread::spawn(move || client_reader(server, tx, worker_bytes, closed));
        for _ in 0..3 {
            protocol::write_msg(
                &mut peer,
                protocol::C_RESIZE,
                &protocol::encode_resize(80, 24),
            )
            .unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while bytes.load(Ordering::Acquire) < 128 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(bytes.load(Ordering::Acquire) >= 128);
        drop(receiver);
        worker.join().unwrap();
    }

    fn client(mode: protocol::AttachMode, tw: u16, th: u16) -> (ConnectedClient, UnixStream) {
        let (server, peer) = UnixStream::pair().unwrap();
        let (_, rx) = mpsc::sync_channel(1);
        (
            ConnectedClient {
                id: 1,
                writer: QueuedWriter::new(server).unwrap(),
                event_rx: ClientReceiver {
                    rx,
                    bytes: Arc::new(AtomicUsize::new(0)),
                    closed: Arc::new(AtomicBool::new(false)),
                },
                mode,
                tw,
                th,
            },
            peer,
        )
    }

    #[test]
    fn readonly_cannot_shrink_writable_geometry_and_sizes_are_bounded() {
        let (writer, _a) = client(protocol::AttachMode::Shared, 120, 40);
        let (observer, _b) = client(protocol::AttachMode::Readonly, 1, 1);
        assert_eq!(effective_size(&[writer, observer]), (120, 40));
        let (huge, _c) = client(protocol::AttachMode::Shared, u16::MAX, u16::MAX);
        assert_eq!(effective_size(&[huge]), (1000, 500));
        let (zero, _d) = client(protocol::AttachMode::Shared, 0, 0);
        assert_eq!(effective_size(&[zero]), (1, 1));
    }

    #[test]
    fn output_frames_remain_atomic_and_detach_drains_on_drop() {
        let (server, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut writer = QueuedWriter::new(server).unwrap();
        protocol::write_msg(&mut writer, protocol::S_OUTPUT, b"first").unwrap();
        protocol::write_msg(&mut writer, protocol::S_OUTPUT, b"second").unwrap();
        protocol::write_msg(&mut writer, protocol::S_DETACHED, &[]).unwrap();
        drop(writer);
        assert_eq!(
            protocol::read_msg(&mut peer).unwrap(),
            (protocol::S_OUTPUT, b"first".to_vec())
        );
        assert_eq!(
            protocol::read_msg(&mut peer).unwrap(),
            (protocol::S_OUTPUT, b"second".to_vec())
        );
        assert_eq!(
            protocol::read_msg(&mut peer).unwrap().0,
            protocol::S_DETACHED
        );
    }

    #[test]
    fn output_small_frame_burst_preserves_every_frame_before_detach() {
        let (server, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut writer = QueuedWriter::new(server).unwrap();
        // Deliberately pause the consumer. A short burst below the byte cap
        // must not disconnect merely because it contains many small deltas.
        for sequence in 0_u32..4096 {
            protocol::write_msg(&mut writer, protocol::S_OUTPUT, &sequence.to_be_bytes()).unwrap();
        }
        assert!(writer.queued_bytes.load(Ordering::Acquire) <= QUEUE_BYTES);
        protocol::write_msg(&mut writer, protocol::S_DETACHED, &[]).unwrap();
        drop(writer);
        for sequence in 0_u32..4096 {
            assert_eq!(
                protocol::read_msg(&mut peer).unwrap(),
                (protocol::S_OUTPUT, sequence.to_be_bytes().to_vec())
            );
        }
        assert_eq!(
            protocol::read_msg(&mut peer).unwrap().0,
            protocol::S_DETACHED
        );
    }

    #[test]
    fn slow_output_peer_is_disconnected_without_blocking_producer() {
        let (server, _peer) = UnixStream::pair().unwrap();
        let mut writer = QueuedWriter::new(server).unwrap();
        let payload = vec![b'x'; 1024 * 1024];
        let start = std::time::Instant::now();
        let mut rejected = false;
        for _ in 0..64 {
            if protocol::write_msg(&mut writer, protocol::S_OUTPUT, &payload).is_err() {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "slow peer must hit the byte cap");
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(writer.queued_bytes.load(Ordering::Acquire) <= QUEUE_BYTES);
    }

    #[test]
    fn json_resize_cannot_bypass_size_normalization() {
        let (server, mut peer) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::sync_channel(4);
        let worker = std::thread::spawn(move || {
            client_reader(
                server,
                tx,
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicBool::new(false)),
            )
        });
        protocol::write_msg(
            &mut peer,
            protocol::C_EVENT,
            &serde_json::to_vec(&Event::Resize(0, u16::MAX)).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap().0,
            ClientMsg::Resize(1, 500)
        ));
        drop(peer);
        worker.join().unwrap();
    }

    #[test]
    fn bind_refuses_live_listener_without_unlinking() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("session.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let inode = std::fs::metadata(&path).unwrap().ino();
        assert!(bind_path_socket(&path).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    }
}
