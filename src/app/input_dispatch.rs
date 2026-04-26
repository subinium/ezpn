//! Out-of-band input dispatch (IPC commands).
//!
//! In-band input (keyboard / mouse) is still handled inline in
//! `event_loop::run` because the handlers mutate ~15 local variables and
//! peeling them out would require an even larger context struct than the
//! current setup. That refactor is tracked separately; for now this module
//! owns the `ezpn-ctl` IPC dispatch table.
//!
//! [`handle_ipc_command`] is invoked from the foreground loop when a
//! command arrives on the IPC socket. It mirrors the keyboard handlers
//! (split / close / focus / equalize / layout / exec / save / load) but
//! without the input-mode state machine, so it can drive the same
//! mutations that the user would trigger via prefix keys.

use std::collections::HashMap;

use crate::app::lifecycle::{
    apply_snapshot, close_pane, do_split, kill_all_panes, replace_pane, resize_all,
    spawn_layout_panes,
};
use crate::app::render_ctl::make_inner;
use crate::app::state::RenderUpdate;
use crate::ipc;
use crate::keymap::keyspec;
use crate::layout::{Direction, Layout};
use crate::pane::{Pane, PaneLaunch};
use crate::project;
use crate::settings::Settings;
use crate::workspace::{self, WorkspaceSnapshot};

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_ipc_command(
    cmd: ipc::IpcRequest,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    shell: &mut String,
    tw: u16,
    th: u16,
    settings: &mut Settings,
    scrollback: usize,
    max_scrollback: usize,
    restart_policies: &mut HashMap<usize, project::RestartPolicy>,
    restart_state: &mut HashMap<usize, (std::time::Instant, u32)>,
    zoomed_pane: &mut Option<usize>,
) -> (ipc::IpcResponse, RenderUpdate) {
    let mut update = RenderUpdate::default();

    let response = match cmd {
        ipc::IpcRequest::Split { direction, pane } => {
            let target = pane.unwrap_or(*active);
            if !panes.contains_key(&target) {
                ipc::IpcResponse::error("pane not found")
            } else {
                let dir = match direction {
                    ipc::SplitDirection::Horizontal => Direction::Horizontal,
                    ipc::SplitDirection::Vertical => Direction::Vertical,
                };
                match do_split(
                    layout, panes, target, dir, shell, tw, th, settings, scrollback,
                ) {
                    Ok(()) => {
                        update.mark_all(layout);
                        update.border_dirty = true;
                        ipc::IpcResponse::success("split ok")
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            }
        }
        ipc::IpcRequest::Close { pane } => {
            if !panes.contains_key(&pane) && !layout.pane_ids().contains(&pane) {
                ipc::IpcResponse::error("pane not found")
            } else {
                close_pane(
                    layout,
                    panes,
                    active,
                    pane,
                    restart_policies,
                    restart_state,
                    zoomed_pane,
                );
                resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
                ipc::IpcResponse::success("closed")
            }
        }
        ipc::IpcRequest::Focus { pane } => {
            if panes.contains_key(&pane) {
                *active = pane;
                update.full_redraw = true;
                ipc::IpcResponse::success("focused")
            } else {
                ipc::IpcResponse::error("pane not found")
            }
        }
        ipc::IpcRequest::Equalize => {
            layout.equalize();
            resize_all(panes, layout, tw, th, settings);
            update.mark_all(layout);
            update.border_dirty = true;
            ipc::IpcResponse::success("equalized")
        }
        ipc::IpcRequest::List => {
            let inner = make_inner(tw, th, settings.show_status_bar);
            let rects = layout.pane_rects(&inner);
            let panes = layout
                .pane_ids()
                .into_iter()
                .enumerate()
                .map(|(index, id)| {
                    let (cols, rows) = rects
                        .get(&id)
                        .map(|rect| (rect.w, rect.h))
                        .unwrap_or((0, 0));
                    let pane = panes.get(&id);
                    ipc::PaneInfo {
                        index,
                        id,
                        cols,
                        rows,
                        alive: pane.is_some_and(|pane| pane.is_alive()),
                        active: id == *active,
                        command: pane
                            .map(|pane| pane.launch_label(shell))
                            .unwrap_or_else(|| shell.to_string()),
                    }
                })
                .collect();
            ipc::IpcResponse::with_panes(panes)
        }
        ipc::IpcRequest::Layout { spec } => match Layout::from_spec(&spec) {
            Ok(new_layout) => {
                match spawn_layout_panes(
                    &new_layout,
                    HashMap::new(),
                    shell,
                    tw,
                    th,
                    settings,
                    scrollback,
                ) {
                    Ok(new_panes) => {
                        kill_all_panes(panes);
                        *layout = new_layout;
                        *panes = new_panes;
                        *active = *layout.pane_ids().first().unwrap_or(&0);
                        update.mark_all(layout);
                        update.border_dirty = true;
                        ipc::IpcResponse::success("layout applied")
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            }
            Err(error) => ipc::IpcResponse::error(error),
        },
        ipc::IpcRequest::Exec { pane, command } => {
            if !panes.contains_key(&pane) {
                ipc::IpcResponse::error("pane not found")
            } else {
                match replace_pane(
                    panes,
                    layout,
                    pane,
                    PaneLaunch::Command(command),
                    shell,
                    tw,
                    th,
                    settings,
                    scrollback,
                ) {
                    Ok(()) => {
                        update.dirty_panes.insert(pane);
                        ipc::IpcResponse::success("exec ok")
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            }
        }
        ipc::IpcRequest::Save { path } => {
            // IPC save uses a single-tab snapshot (no TabManager available here)
            let tab = workspace::TabSnapshot {
                name: "1".to_string(),
                layout: layout.clone(),
                active_pane: *active,
                zoomed_pane: None,
                broadcast: false,
                panes: layout
                    .pane_ids()
                    .into_iter()
                    .map(|id| {
                        let pane = panes.get(&id);
                        workspace::PaneSnapshot {
                            id,
                            launch: pane
                                .map(|p| p.launch().clone())
                                .unwrap_or(PaneLaunch::Shell),
                            name: pane.and_then(|p| p.name().map(|s| s.to_string())),
                            cwd: pane
                                .and_then(|p| p.live_cwd())
                                .map(|p| p.to_string_lossy().to_string()),
                            env: pane.map(|p| p.initial_env().clone()).unwrap_or_default(),
                            restart: project::RestartPolicy::default(),
                            shell: pane.and_then(|p| p.initial_shell().map(|s| s.to_string())),
                            scrollback_blob: None,
                        }
                    })
                    .collect(),
            };
            let snapshot = WorkspaceSnapshot {
                version: workspace::SNAPSHOT_VERSION,
                shell: shell.clone(),
                border_style: settings.border_style,
                show_status_bar: settings.show_status_bar,
                show_tab_bar: settings.show_tab_bar,
                scrollback,
                active_tab: 0,
                tabs: vec![tab],
            };
            match workspace::save_snapshot(&path, &snapshot) {
                Ok(()) => ipc::IpcResponse::success(format!("saved {}", path)),
                Err(error) => ipc::IpcResponse::error(error.to_string()),
            }
        }
        ipc::IpcRequest::Load { path } => match workspace::load_snapshot(&path) {
            Ok(snapshot) => {
                match apply_snapshot(
                    snapshot, layout, panes, active, shell, settings, tw, th, scrollback,
                ) {
                    Ok(()) => {
                        update.mark_all(layout);
                        update.border_dirty = true;
                        ipc::IpcResponse::success(format!("loaded {}", path))
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            }
            Err(error) => ipc::IpcResponse::error(error.to_string()),
        },
        ipc::IpcRequest::ClearHistory { pane } => {
            if let Some(p) = panes.get_mut(&pane) {
                match p.clear_history() {
                    Ok(()) => {
                        update.dirty_panes.insert(pane);
                        ipc::IpcResponse::success(format!("cleared history for pane {pane}"))
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            } else {
                ipc::IpcResponse::error(format!("no pane {pane}"))
            }
        }
        ipc::IpcRequest::SetHistoryLimit { pane, lines } => {
            let lines = lines.min(max_scrollback);
            if let Some(p) = panes.get_mut(&pane) {
                match p.set_scrollback_lines(lines) {
                    Ok(()) => {
                        update.dirty_panes.insert(pane);
                        ipc::IpcResponse::success(format!(
                            "scrollback for pane {pane} set to {lines}"
                        ))
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            } else {
                ipc::IpcResponse::error(format!("no pane {pane}"))
            }
        }
        ipc::IpcRequest::SendKeys {
            target,
            keys,
            literal,
        } => handle_send_keys(panes, *active, target, keys, literal),
    };

    (response, update)
}

/// SPEC 06 §4 — `send-keys` dispatch.
///
/// Resolves the target pane, optionally compiles the chord-token list into
/// raw PTY bytes (`literal=false`) or concatenates them verbatim
/// (`literal=true`), and writes the result through `Pane::write_bytes`. The
/// success message reports the byte count for debugging visibility.
fn handle_send_keys(
    panes: &mut HashMap<usize, Pane>,
    active: usize,
    target: ipc::PaneTarget,
    tokens: Vec<String>,
    literal: bool,
) -> ipc::IpcResponse {
    if tokens.is_empty() {
        return ipc::IpcResponse::error("no keys to send");
    }
    let total_bytes: usize = tokens.iter().map(|s| s.len()).sum();
    if total_bytes > ipc::SEND_KEYS_MAX_BYTES {
        return ipc::IpcResponse::error(format!(
            "send-keys payload too large ({total_bytes} > {} bytes)",
            ipc::SEND_KEYS_MAX_BYTES
        ));
    }

    let pane_id = match target {
        ipc::PaneTarget::Id { value } => value,
        ipc::PaneTarget::Current => active,
    };

    let bytes = if literal {
        // Per SPEC §4.5, literal mode forbids token strings that would
        // otherwise compile to a Named key — surfaces the user's intent
        // mismatch ("did you mean to press Enter or send the 5 letters
        // E-n-t-e-r?") rather than silently writing raw bytes.
        if let Some(named) = tokens.iter().find(|t| keyspec::is_named_key(t)) {
            return ipc::IpcResponse::error(format!(
                "--literal forbids named keys (got '{named}')"
            ));
        }
        let mut out = Vec::with_capacity(total_bytes);
        for tok in &tokens {
            out.extend_from_slice(tok.as_bytes());
        }
        out
    } else {
        match keyspec::compile_to_bytes(&tokens) {
            Ok(bytes) => bytes,
            Err(error) => return ipc::IpcResponse::error(format!("parse: {error}")),
        }
    };

    let Some(pane) = panes.get_mut(&pane_id) else {
        return ipc::IpcResponse::error(format!("pane {pane_id} not found"));
    };
    if !pane.is_alive() {
        return ipc::IpcResponse::error(format!("pane {pane_id} not alive"));
    }
    pane.write_bytes(&bytes);
    ipc::IpcResponse::success(format!("sent {} bytes", bytes.len()))
}
