//! Client routing: socket reader thread, accept handler, and the
//! smallest-client size policy.
//!
//! Owns nothing — every function takes the daemon state by mutable
//! reference. The reader thread itself is spawned per connection and
//! pumps `ClientMsg` values into the main loop's channel.

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use crossterm::event::Event;

use crate::layout::Layout;
use crate::pane::Pane;
use crate::protocol;
use crate::settings::Settings;

use super::state::{ClientMsg, ConnectedClient, DragState};
use super::writer::{spawn_writer, OutboundMsg, QUEUE_CAP};
use crate::app::state::RenderUpdate;

pub(crate) static NEXT_CLIENT_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Compute the effective terminal size from all active clients.
/// Uses smallest-client policy (like tmux).
pub(crate) fn effective_size(clients: &[ConnectedClient]) -> (u16, u16) {
    let mut min_w: u16 = u16::MAX;
    let mut min_h: u16 = u16::MAX;
    for c in clients {
        min_w = min_w.min(c.tw);
        min_h = min_h.min(c.th);
    }
    if min_w == u16::MAX {
        (80, 24)
    } else {
        (min_w, min_h)
    }
}

/// Convert a `catch_unwind` payload into a printable reason string.
pub(crate) fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        // [perf:cold] clone here: panic reporting path runs at most once per
        // crashed reader thread; cloning the message string is irrelevant.
        return s.clone();
    }
    "unknown panic payload".to_string()
}

/// Reader thread for client socket messages.
pub(crate) fn client_reader(stream: UnixStream, tx: mpsc::Sender<ClientMsg>) {
    let mut reader = BufReader::new(stream);
    loop {
        match protocol::read_msg(&mut reader) {
            Ok((tag, payload)) => {
                let msg = match tag {
                    protocol::C_EVENT => serde_json::from_slice::<Event>(&payload)
                        .ok()
                        .map(ClientMsg::Event),
                    protocol::C_RESIZE => {
                        protocol::decode_resize(&payload).map(|(w, h)| ClientMsg::Resize(w, h))
                    }
                    protocol::C_DETACH => Some(ClientMsg::Detach),
                    protocol::C_KILL => Some(ClientMsg::Kill),
                    _ => None,
                };
                if let Some(msg) = msg {
                    if tx.send(msg).is_err() {
                        break;
                    }
                    crate::pane::wake_main_loop(); // Wake server loop
                }
            }
            Err(_) => {
                let _ = tx.send(ClientMsg::Disconnected);
                break;
            }
        }
    }
}

/// Accept a new client connection, handling steal/shared/readonly modes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn accept_client(
    conn: UnixStream,
    new_w: u16,
    new_h: u16,
    mode: protocol::AttachMode,
    caps: u32,
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
    // Steal mode: detach all existing clients
    if mode == protocol::AttachMode::Steal {
        for c in clients.iter_mut() {
            let _ = c.outbound_tx.try_send(OutboundMsg::Detached);
        }
        clients.clear();
    }

    // Set up the new client
    if let Ok(read_conn) = conn.try_clone() {
        conn.set_read_timeout(None).ok();
        let (msg_tx, msg_rx) = mpsc::channel();
        // [perf:cold] clone here: cloning an `mpsc::Sender` is one Arc bump.
        // Runs once per accepted client (cold path). One clone for the
        // reader's panic-reason path, one for the writer's eviction signal.
        let panic_tx = msg_tx.clone();
        let wake_writer = msg_tx.clone();
        std::thread::spawn(move || {
            // Isolate reader-thread panics so one bad client cannot kill the daemon.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client_reader(read_conn, msg_tx);
            }));
            if let Err(payload) = result {
                let reason = panic_payload_to_string(&payload);
                let _ = panic_tx.send(ClientMsg::Panicked(reason));
                crate::pane::wake_main_loop();
            }
        });
        // Spawn the per-client writer thread. It owns the socket and
        // honours `set_write_timeout` so a slow peer cannot stall the
        // daemon main loop. Eviction signal arrives via `wake_writer`.
        let (out_tx, out_rx) = mpsc::sync_channel::<OutboundMsg>(QUEUE_CAP);
        let writer_handle = spawn_writer(conn, out_rx, wake_writer);
        let client_id = NEXT_CLIENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        clients.push(ConnectedClient {
            id: client_id,
            outbound_tx: out_tx,
            writer_handle: Some(writer_handle),
            event_rx: msg_rx,
            mode,
            caps,
            tw: new_w,
            th: new_h,
        });
    }

    // Recompute effective size and resize panes
    let (ew, eh) = effective_size(clients);
    if ew != *tw || eh != *th {
        *tw = ew;
        *th = eh;
        *drag = None;
        crate::app::lifecycle::resize_all(panes, layout, *tw, *th, settings);
        if let Some(zpid) = zoomed_pane {
            crate::app::render_ctl::resize_zoomed_pane(panes, zpid, *tw, *th, settings);
        }
    }

    // Force full redraw for new client
    update.mark_all(layout);
    update.border_dirty = true;
}
