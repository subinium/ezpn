//! Per-client writer thread that drains a bounded outbound queue and
//! pushes frames to the socket with a strict write timeout.
//!
//! See `docs/spec/v0.10.0/01-daemon-io-resilience.md` §4.1.
//!
//! Why a thread instead of inline writes: a single slow attached client
//! used to block the entire daemon main loop because every frame went
//! through `BufWriter<UnixStream>::write_all` synchronously. With a
//! per-client thread + `set_write_timeout(50ms)` + bounded mpsc, a slow
//! peer is contained: the main loop's `try_send` either succeeds (fast)
//! or returns `Full` (treated as disconnect on the next iteration).
//!
//! Eviction: after `MAX_WOULDBLOCKS` consecutive timeouts the writer
//! signals `ClientMsg::Disconnected` to the main loop and exits, dropping
//! the socket. The matching `ConnectedClient::drop` joins the handle.

use std::io::BufWriter;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::protocol;

use super::state::ClientMsg;

/// Per-write timeout. Bounds the worst-case main-loop stall: a slow
/// peer that blocks every send still only burns 3 × 50 ms = 150 ms
/// before being evicted (see `MAX_WOULDBLOCKS`).
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_millis(50);

/// Outbound queue depth per client. With ~60 fps render budget and
/// typical 8–32 KB frames this is roughly one second of buffered
/// output before the main loop's `try_send` returns `Full`.
pub(crate) const QUEUE_CAP: usize = 64;

/// After this many consecutive `WouldBlock`/`TimedOut` writes the
/// client is considered dead and the writer thread exits.
pub(crate) const MAX_WOULDBLOCKS: u32 = 3;

/// One outbound message to a connected client. Crafted as an enum (not
/// pre-encoded bytes) so the writer thread can choose the right
/// `S_*` tag and the main loop never touches the socket directly.
pub(crate) enum OutboundMsg {
    /// Rendered frame payload (S_OUTPUT).
    Frame(Vec<u8>),
    /// Raw passthrough output, e.g. OSC 52 echo (S_OUTPUT).
    Output(Vec<u8>),
    /// Server is detaching this client (S_DETACHED).
    Detached,
    /// Server is shutting down (S_EXIT).
    Exit,
    /// Pre-encoded handshake reply payload (S_HELLO_OK or S_HELLO_ERR).
    /// Reserved for SPEC 07 (event subscription stream) wiring.
    #[allow(dead_code)]
    Raw { tag: u8, payload: Vec<u8> },
    /// Sentinel: writer drains the channel and exits, dropping the socket.
    Shutdown,
}

/// Spawn the writer thread for a freshly accepted client.
///
/// The thread owns the socket. On any non-`WouldBlock` error, or after
/// `MAX_WOULDBLOCKS` consecutive timeouts, it sends
/// `ClientMsg::Disconnected` to the main loop via `wake_on_drop`,
/// wakes the loop, and exits.
pub(crate) fn spawn_writer(
    socket: UnixStream,
    rx: mpsc::Receiver<OutboundMsg>,
    wake_on_drop: mpsc::Sender<ClientMsg>,
) -> JoinHandle<()> {
    let _ = socket.set_write_timeout(Some(WRITE_TIMEOUT));
    std::thread::Builder::new()
        .name("ezpn-writer".to_string())
        .spawn(move || run(socket, rx, wake_on_drop))
        .expect("spawn ezpn-writer thread")
}

fn run(socket: UnixStream, rx: mpsc::Receiver<OutboundMsg>, wake: mpsc::Sender<ClientMsg>) {
    let mut bw = BufWriter::with_capacity(64 * 1024, socket);
    let mut consecutive_wouldblocks: u32 = 0;
    while let Ok(msg) = rx.recv() {
        let result = match &msg {
            OutboundMsg::Shutdown => return,
            OutboundMsg::Frame(b) | OutboundMsg::Output(b) => {
                protocol::write_msg(&mut bw, protocol::S_OUTPUT, b)
            }
            OutboundMsg::Detached => protocol::write_msg(&mut bw, protocol::S_DETACHED, &[]),
            OutboundMsg::Exit => protocol::write_msg(&mut bw, protocol::S_EXIT, &[]),
            OutboundMsg::Raw { tag, payload } => protocol::write_msg(&mut bw, *tag, payload),
        };
        match result {
            Ok(()) => consecutive_wouldblocks = 0,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                consecutive_wouldblocks += 1;
                if consecutive_wouldblocks >= MAX_WOULDBLOCKS {
                    eprintln!(
                        "ezpn: evicted slow client after {consecutive_wouldblocks} consecutive write timeouts"
                    );
                    let _ = wake.send(ClientMsg::Disconnected);
                    crate::pane::wake_main_loop();
                    return;
                }
            }
            Err(_) => {
                let _ = wake.send(ClientMsg::Disconnected);
                crate::pane::wake_main_loop();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    fn pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().expect("UnixStream::pair")
    }

    /// Spawn a thread that drains the peer socket into a Vec, returning
    /// a join handle that yields the collected bytes. Required because
    /// `pair()` socket buffers are small (~8KB on macOS) — without an
    /// active reader, even modest writes hit `WRITE_TIMEOUT`.
    fn spawn_drainer(mut peer: UnixStream) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = peer.read_to_end(&mut buf);
            buf
        })
    }

    #[test]
    fn writer_passes_through_under_normal_load() {
        let (writer_sock, peer) = pair();
        let drainer = spawn_drainer(peer);
        let (tx, rx) = mpsc::sync_channel::<OutboundMsg>(QUEUE_CAP);
        let (wake_tx, _wake_rx) = mpsc::channel();
        let handle = spawn_writer(writer_sock, rx_to_unbounded(rx), wake_tx);

        // 10 frames of 1 KB; the drainer thread reads concurrently so the
        // send buffer never saturates and we never trip WRITE_TIMEOUT.
        for i in 0..10u8 {
            tx.send(OutboundMsg::Frame(vec![i; 1024]))
                .expect("send frame");
        }
        drop(tx);
        let _ = handle.join();
        let buf = drainer.join().expect("drainer thread");
        // Each frame: 1 tag + 4 length + 1024 payload = 1029 bytes.
        assert_eq!(buf.len(), 10 * 1029, "all frames must reach peer in order");
    }

    #[test]
    fn writer_drops_socket_on_shutdown_msg() {
        let (writer_sock, peer) = pair();
        let drainer = spawn_drainer(peer);
        let (tx, rx) = mpsc::sync_channel::<OutboundMsg>(QUEUE_CAP);
        let (wake_tx, _wake_rx) = mpsc::channel();
        let handle = spawn_writer(writer_sock, rx_to_unbounded(rx), wake_tx);

        tx.send(OutboundMsg::Frame(b"hi".to_vec())).unwrap();
        tx.send(OutboundMsg::Shutdown).unwrap();
        drop(tx);
        let _ = handle.join();
        let buf = drainer.join().expect("drainer thread");
        assert!(!buf.is_empty(), "peer must receive the queued frame");
    }

    #[test]
    fn writer_evicts_after_three_wouldblocks() {
        let (writer_sock, peer) = pair();
        // Don't read on the peer side. Set the writer's send buffer small
        // enough that we hit WouldBlock quickly.
        let _ = writer_sock.set_write_timeout(Some(Duration::from_millis(20)));
        // Fill the peer's recv buffer so further writes time out.
        let _ = peer.set_nonblocking(true);

        let (tx, rx) = mpsc::sync_channel::<OutboundMsg>(QUEUE_CAP);
        let (wake_tx, wake_rx) = mpsc::channel();
        let handle = spawn_writer(writer_sock, rx_to_unbounded(rx), wake_tx);

        // Push large frames to saturate the peer's recv buffer.
        for _ in 0..32 {
            let _ = tx.try_send(OutboundMsg::Frame(vec![0u8; 256 * 1024]));
        }

        // Wait for eviction signal (bounded patience).
        let evicted = wake_rx
            .recv_timeout(Duration::from_secs(2))
            .map(|m| matches!(m, ClientMsg::Disconnected))
            .unwrap_or(false);
        assert!(
            evicted,
            "writer must signal Disconnected on repeated timeout"
        );
        drop(tx);
        let _ = handle.join();
        // Keep peer alive until the test ends so the OS doesn't reclaim early.
        drop(peer);
    }

    /// Convert a `SyncSender` channel's receiver into the same `Receiver<T>`
    /// type the writer expects. (Both `mpsc::channel` and `mpsc::sync_channel`
    /// share `Receiver<T>`.)
    fn rx_to_unbounded(rx: mpsc::Receiver<OutboundMsg>) -> mpsc::Receiver<OutboundMsg> {
        rx
    }
}
