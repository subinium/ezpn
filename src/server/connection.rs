//! Per-client connection lifecycle: accept, framing, reader thread,
//! detach/disconnect bookkeeping, and the path-socket bind helper.
//!
//! ## Lock & state ordering
//!
//! The server is single-threaded except for the per-client *reader*
//! threads spawned by [`accept_client`]. Those readers communicate with
//! the main loop only through the `mpsc::channel` they own, so there
//! are no shared mutexes between threads — `Mutex<...>` boundaries do
//! not exist in this module today.
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

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;

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
    pub(super) writer: std::io::BufWriter<UnixStream>,
    pub(super) event_rx: mpsc::Receiver<ClientMsg>,
    pub(super) mode: protocol::AttachMode,
    pub(super) tw: u16,
    pub(super) th: u16,
}

impl Drop for ConnectedClient {
    fn drop(&mut self) {
        // Shutdown the underlying socket to force the reader thread to exit.
        let _ = self.writer.get_ref().shutdown(std::net::Shutdown::Both);
    }
}

/// Compute the effective terminal size from all active clients.
/// Uses smallest-client policy (like tmux).
pub(super) fn effective_size(clients: &[ConnectedClient]) -> (u16, u16) {
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
    let _ = std::fs::remove_file(sock_path);

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
fn client_reader(stream: UnixStream, tx: mpsc::Sender<ClientMsg>) {
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
    // Steal mode: detach all existing clients
    if mode == protocol::AttachMode::Steal {
        for c in clients.iter_mut() {
            let _ = protocol::write_msg(&mut c.writer, protocol::S_DETACHED, &[]);
        }
        clients.clear();
    }

    // Set up the new client
    if let Ok(read_conn) = conn.try_clone() {
        conn.set_read_timeout(None).ok();
        let (msg_tx, msg_rx) = mpsc::channel();
        std::thread::spawn(move || {
            client_reader(read_conn, msg_tx);
        });
        let client_id = NEXT_CLIENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        clients.push(ConnectedClient {
            id: client_id,
            writer: std::io::BufWriter::new(conn),
            event_rx: msg_rx,
            mode,
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
        crate::resize_all(panes, layout, *tw, *th, settings);
        if let Some(zpid) = zoomed_pane {
            crate::resize_zoomed_pane(panes, zpid, *tw, *th, settings);
        }
    }

    // Force full redraw for new client
    update.mark_all(layout);
    update.border_dirty = true;
}
