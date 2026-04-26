//! Snapshot helpers used by the daemon event loop.
//!
//! Wraps `WorkspaceSnapshot::from_live` so the three call sites (auto-save
//! on detach, SIGTERM persist, IPC `Save`) can share one signature instead
//! of repeating fourteen positional arguments. Restore logic stays in the
//! event loop because the two restore paths (cold start vs. live `Load`)
//! diverge enough that a shared helper would need a config struct — out of
//! scope for the Tidy First refactor (#24).

use std::collections::HashMap;

use crate::layout::Layout;
use crate::pane::Pane;
use crate::project;
use crate::render::BorderStyle;
use crate::tab::TabManager;
use crate::workspace::WorkspaceSnapshot;

/// Capture the live daemon state into a `WorkspaceSnapshot`.
///
/// Pure forwarding wrapper around `WorkspaceSnapshot::from_live` — the only
/// reason it exists is to give the daemon a single place to call when the
/// `from_live` signature inevitably grows another argument.
#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_workspace(
    tab_mgr: &TabManager,
    tab_name: &str,
    layout: &Layout,
    panes: &HashMap<usize, Pane>,
    active: usize,
    zoomed_pane: Option<usize>,
    broadcast: bool,
    restart_policies: &HashMap<usize, project::RestartPolicy>,
    default_shell: &str,
    border_style: BorderStyle,
    show_status_bar: bool,
    show_tab_bar: bool,
    effective_scrollback: usize,
    persist_scrollback: bool,
) -> WorkspaceSnapshot {
    WorkspaceSnapshot::from_live(
        tab_mgr,
        tab_name,
        layout,
        panes,
        active,
        zoomed_pane,
        broadcast,
        restart_policies,
        default_shell,
        border_style,
        show_status_bar,
        show_tab_bar,
        effective_scrollback,
        persist_scrollback,
    )
}
