//! Daemon state types: input mode, tab actions, drag/selection state, and
//! the per-client struct shared across the daemon submodules.
//!
//! Pure data definitions — no logic beyond trivial constructors and `Drop`
//! plumbing for `ConnectedClient`. Lives in its own file so the rest of the
//! daemon can `use crate::daemon::state::*` without circular imports.

use std::sync::mpsc;
use std::time::Instant;

use crossterm::event::Event;

use crate::layout::{Direction, Rect, SepHit};
use crate::protocol;

use super::writer::OutboundMsg;

/// Input state machine for prefix key support.
#[allow(dead_code)]
pub(crate) enum InputMode {
    Normal,
    Prefix {
        entered_at: Instant,
    },
    CopyMode(crate::copy_mode::CopyModeState),
    QuitConfirm,
    CloseConfirm,
    CloseTabConfirm,
    ResizeMode,
    PaneSelect,
    HelpOverlay,
    /// Tab rename: typing a new name for the current tab.
    RenameTab {
        buffer: String,
    },
    /// Command palette: typing a command to execute.
    CommandPalette {
        buffer: String,
    },
}

/// Tab action requested by the key handler. The main loop handles the switch.
pub(crate) enum TabAction {
    None,
    NewTab,
    NextTab,
    PrevTab,
    GoToTab(usize),
    CloseTab,
    Rename(String),
    KillSession,
}

/// Text selection state for copy-on-drag.
#[derive(Clone)]
pub(crate) struct TextSelection {
    pub(crate) pane_id: usize,
    pub(crate) start_row: u16,
    pub(crate) start_col: u16,
    pub(crate) end_row: u16,
    pub(crate) end_col: u16,
}

impl TextSelection {
    pub(crate) fn normalized(&self) -> (u16, u16, u16, u16) {
        if self.start_row < self.end_row
            || (self.start_row == self.end_row && self.start_col <= self.end_col)
        {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
        }
    }
}

pub(crate) struct DragState {
    pub(crate) path: Vec<bool>,
    pub(crate) direction: Direction,
    pub(crate) area: Rect,
}

impl DragState {
    pub(crate) fn from_hit(hit: SepHit) -> Self {
        Self {
            path: hit.path,
            direction: hit.direction,
            area: hit.area,
        }
    }

    pub(crate) fn calc_ratio(&self, mx: u16, my: u16) -> f32 {
        match self.direction {
            Direction::Horizontal => {
                let usable = self.area.w.saturating_sub(1) as f32;
                if usable <= 0.0 {
                    return 0.5;
                }
                ((mx as f32 - self.area.x as f32) / usable).clamp(0.1, 0.9)
            }
            Direction::Vertical => {
                let usable = self.area.h.saturating_sub(1) as f32;
                if usable <= 0.0 {
                    return 0.5;
                }
                ((my as f32 - self.area.y as f32) / usable).clamp(0.1, 0.9)
            }
        }
    }
}

/// Client message from the reader thread.
pub(crate) enum ClientMsg {
    Event(Event),
    Resize(u16, u16),
    Detach,
    Disconnected,
    /// Kill the server (from `ezpn kill`).
    Kill,
    /// Reader thread panicked. Server treats this like Disconnected
    /// after logging the payload to stderr.
    Panicked(String),
}

/// Connected client with attach mode and per-client state.
///
/// Outbound frames are sent through `outbound_tx` (bounded mpsc) and
/// drained by a dedicated writer thread (`writer_handle`). See SPEC 01
/// `docs/spec/v0.10.0/01-daemon-io-resilience.md` §4.1 for rationale.
pub(crate) struct ConnectedClient {
    pub(crate) id: u64,
    /// Bounded outbound queue. `try_send` returning `Full` means the
    /// writer thread is wedged behind a slow peer — main loop treats
    /// the client as dead on the next iteration.
    pub(crate) outbound_tx: mpsc::SyncSender<OutboundMsg>,
    /// Writer thread handle. `Drop` joins after sending `Shutdown`.
    pub(crate) writer_handle: Option<std::thread::JoinHandle<()>>,
    pub(crate) event_rx: mpsc::Receiver<ClientMsg>,
    pub(crate) mode: protocol::AttachMode,
    /// Capability bits negotiated during the C_HELLO handshake. Zero for
    /// legacy clients that connected without a Hello — those are treated
    /// as having no extended capabilities.
    #[allow(dead_code)]
    pub(crate) caps: u32,
    pub(crate) tw: u16,
    pub(crate) th: u16,
}

impl Drop for ConnectedClient {
    fn drop(&mut self) {
        // Tell the writer to drain and exit. `try_send` because we don't
        // want to block here if the queue is already full — the writer
        // will see the channel close and exit anyway.
        let _ = self.outbound_tx.try_send(OutboundMsg::Shutdown);
        if let Some(handle) = self.writer_handle.take() {
            // Bounded patience: the writer should drain within at most
            // one write_timeout (50ms). If it doesn't, we leak the join
            // handle — the OS reaps the thread on process exit.
            let _ = handle.join();
        }
    }
}
