//! Local UI state for the foreground (`--no-daemon`) event loop.
//!
//! Pulled out of `main.rs` so `event_loop::run` only has to import the
//! pieces, not redefine them. Each type is small but lives at the boundary
//! between input handling and render orchestration, so this module is
//! intentionally a thin grab-bag rather than carved into smaller files.
//!
//! The daemon (`server.rs`) keeps its own copies of analogous structs —
//! they aren't shared on purpose, since the foreground loop's state model
//! is simpler (no IPC fan-out, no broadcast peers).

use std::collections::HashSet;
use std::time::Instant;

use crate::layout::{Direction, Layout, Rect, SepHit};
use crate::workspace;

/// Input state machine for prefix key support.
pub(crate) enum InputMode {
    Normal,
    Prefix { entered_at: Instant },
    ScrollMode,
    QuitConfirm,
    ResizeMode,
    PaneSelect,
    HelpOverlay,
}

/// Text selection state for copy-on-drag.
#[derive(Clone)]
pub(crate) struct TextSelection {
    pub pane_id: usize,
    pub start_row: u16,
    pub start_col: u16,
    pub end_row: u16,
    pub end_col: u16,
}

impl TextSelection {
    /// Normalized range: (min_row, min_col, max_row, max_col)
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
    pub path: Vec<bool>,
    pub direction: Direction,
    pub area: Rect,
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

#[derive(Default)]
pub(crate) struct RenderUpdate {
    pub dirty_panes: HashSet<usize>,
    pub full_redraw: bool,
    pub border_dirty: bool,
}

impl RenderUpdate {
    pub fn mark_all(&mut self, layout: &Layout) {
        self.full_redraw = true;
        self.dirty_panes.extend(layout.pane_ids());
    }

    pub fn merge(&mut self, other: &mut Self) {
        self.dirty_panes.extend(other.dirty_panes.drain());
        self.full_redraw |= other.full_redraw;
        self.border_dirty |= other.border_dirty;
    }

    pub fn needs_render(&self) -> bool {
        self.full_redraw || !self.dirty_panes.is_empty()
    }
}

/// Extra state from a snapshot restore (all tabs in order).
pub(crate) struct SnapshotExtra {
    /// All tabs in their original order.
    pub all_tabs: Vec<workspace::TabSnapshot>,
    /// Which index in `all_tabs` is the active one (already spawned by build_initial_state).
    pub active_tab_idx: usize,
    /// The snapshot's scrollback value (for consistency across all tabs).
    pub scrollback: usize,
}
