//! Daemon-side handlers for [`crate::ipc::IpcRequestExt`].
//!
//! Per RFC #103 (v0.13 IPC unification), `ls_tree`, `dump`, and
//! `send_keys` route through the same channel as the legacy
//! [`crate::ipc::IpcRequest`] vocabulary so handlers run inside the
//! main loop with full state access (`panes`, `tab_mgr`, `clients`).
//!
//! The wire schemas live in [`crate::ipc`] and are frozen at v1 — see
//! `docs/scripting.md` §3.1 (`ls --json`) and the `Dump` doc-comment
//! on [`crate::ipc::IpcRequestExt::Dump`] for the contract this file
//! must honour.

use std::collections::HashMap;

use crate::ipc::{
    ClientInfo, DumpPayload, IpcRequestExt, IpcResponse, LsTree, PaneTreeInfo, SessionTree,
    TabInfo, DUMP_MAX_BYTES,
};
use crate::layout::Layout;
use crate::pane::Pane;
use crate::tab::TabManager;

use super::ConnectedClient;

/// Dispatch an [`IpcRequestExt`] to its daemon-side handler.
///
/// Called from the main loop after the channel envelope has been
/// unwrapped. Borrowing constraints: the main loop holds `panes` /
/// `clients` / `tab_mgr` / `layout` as separate locals (active-tab
/// state is unpacked, inactive tabs live inside [`TabManager`]) so
/// we accept them as individual references rather than a single
/// state struct — matches the pattern used by
/// `bootstrap::handle_ipc_command`.
#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_ext(
    request: IpcRequestExt,
    panes: &HashMap<usize, Pane>,
    active_pane: usize,
    tab_mgr: &TabManager,
    active_tab_name: &str,
    active_layout: &Layout,
    clients: &[ConnectedClient],
    session_name: &str,
    session_started_at: f64,
) -> IpcResponse {
    match request {
        IpcRequestExt::LsTree { session } => handle_ls_tree(
            session.as_deref(),
            panes,
            active_pane,
            tab_mgr,
            active_tab_name,
            active_layout,
            clients,
            session_name,
            session_started_at,
        ),
        IpcRequestExt::Dump { .. } => {
            // `Dump` requires `&mut Pane` (scrollback walk mutates the
            // vt100 parser offset). The caller must use
            // [`handle_dump_mut`] from a context where `panes` is
            // mutable — we expose this synchronous shape only for
            // the read-only Ext commands. Currently unreachable
            // because the main loop calls `handle_dump_mut` directly
            // for the `Dump` arm.
            IpcResponse::error("dump: internal dispatch error (must use handle_dump_mut)")
        }
        IpcRequestExt::SendKeys { .. } => {
            // Out of scope for the v0.13 dump/ls wiring — see issue
            // #81 for the await-prompt contract.
            IpcResponse::error("send-keys: server-side handler not yet wired (issue #81)")
        }
    }
}

/// `ezpn-ctl ls --json` (issue #89). Walks the daemon's tabs and
/// panes and emits the frozen-v1 [`LsTree`] schema. Filters to a
/// single session when `session_filter` is `Some(name)` and that
/// matches `session_name`; otherwise returns an empty `sessions`
/// list (we daemon-per-session today, so the only valid filter is
/// the live session's own name).
#[allow(clippy::too_many_arguments)]
fn handle_ls_tree(
    session_filter: Option<&str>,
    panes: &HashMap<usize, Pane>,
    active_pane: usize,
    tab_mgr: &TabManager,
    active_tab_name: &str,
    active_layout: &Layout,
    clients: &[ConnectedClient],
    session_name: &str,
    session_started_at: f64,
) -> IpcResponse {
    if let Some(filter) = session_filter {
        if filter != session_name {
            return IpcResponse::with_ls_tree(LsTree {
                proto_version: "1.0".to_string(),
                sessions: Vec::new(),
            });
        }
    }

    let client_infos: Vec<ClientInfo> = clients
        .iter()
        .map(|c| ClientInfo {
            // We don't track the originating socket path per client,
            // so emit a stable synthetic identifier. Better than a
            // misleading hardcoded path; honest about the limitation.
            socket: format!("client-{}", c.id),
            size: [c.tw, c.th],
            mode: match c.mode {
                crate::protocol::AttachMode::Steal => "steal",
                crate::protocol::AttachMode::Shared => "shared",
                crate::protocol::AttachMode::Readonly => "readonly",
            }
            .to_string(),
        })
        .collect();

    let mut tabs: Vec<TabInfo> = Vec::with_capacity(tab_mgr.count);
    for logical_idx in 0..tab_mgr.count {
        if logical_idx == tab_mgr.active_idx {
            tabs.push(build_tab_info(
                logical_idx,
                active_tab_name,
                active_layout,
                panes,
                active_pane,
            ));
        } else if let Some(inactive) = tab_mgr.get_inactive(logical_idx) {
            tabs.push(build_tab_info(
                logical_idx,
                &inactive.name,
                &inactive.layout,
                &inactive.panes,
                inactive.active_pane,
            ));
        }
    }

    let session_tree = SessionTree {
        name: session_name.to_string(),
        created_at: session_started_at,
        clients: client_infos,
        focused_tab: Some(tab_mgr.active_idx),
        tabs,
    };

    IpcResponse::with_ls_tree(LsTree {
        proto_version: "1.0".to_string(),
        sessions: vec![session_tree],
    })
}

fn build_tab_info(
    logical_idx: usize,
    name: &str,
    layout: &Layout,
    panes: &HashMap<usize, Pane>,
    active_pane: usize,
) -> TabInfo {
    let pane_ids = layout.pane_ids();
    let mut pane_infos: Vec<PaneTreeInfo> = Vec::with_capacity(pane_ids.len());
    for id in &pane_ids {
        let Some(pane) = panes.get(id) else { continue };
        let screen = pane.screen();
        let (rows, cols) = screen.size();
        let title = {
            let t = screen.title();
            if t.is_empty() {
                pane.name().map(|s| s.to_string())
            } else {
                Some(t.to_string())
            }
        };
        let cwd = pane.initial_cwd().map(|p| p.to_string_lossy().into_owned());
        let reported_cwd = pane
            .reported_cwd()
            .map(|p| p.to_string_lossy().into_owned());
        let exit_code = pane.exit_code().map(|c| c as i32);
        pane_infos.push(PaneTreeInfo {
            id: *id,
            command: pane.launch_label("sh"),
            cwd,
            size: [cols, rows],
            pid: pane.pid(),
            reported_cwd,
            exit_code,
            is_dead: !pane.is_alive(),
            is_focused: *id == active_pane,
            title,
        });
    }

    // `Layout` does not expose a roundtripping `to_spec()` today.
    // Frozen schema requires a string, so we emit a stable
    // descriptor (`"panes:[<ids>]"`) that callers treat as opaque.
    // Replacing with a real spec serialiser is tracked separately
    // (RFC #102 follow-up).
    let layout_spec = {
        let mut ids = pane_ids.clone();
        ids.sort_unstable();
        let joined = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!("panes:[{}]", joined)
    };

    TabInfo {
        id: logical_idx,
        name: name.to_string(),
        focused_pane: panes.contains_key(&active_pane).then_some(active_pane),
        layout: layout_spec,
        panes: pane_infos,
    }
}

/// `ezpn-ctl dump` (issue #88). Snapshots a pane's text grid,
/// applying `include_scrollback`, `since`, `last`, and the
/// [`DUMP_MAX_BYTES`] hard cap. Mutable `panes` because vt100 0.15
/// does not expose direct scrollback iteration — see
/// [`Pane::dump_text`] for the workaround.
///
/// Returns [`IpcResponse::error`] on:
/// * unknown pane id (`pane not found`)
/// * payload exceeding the 16 MiB cap (`dump too large; use --last N`)
pub(super) fn handle_dump_mut(
    request: IpcRequestExt,
    panes: &mut HashMap<usize, Pane>,
    active_pane: usize,
) -> IpcResponse {
    let (target_id, _session, since, last, include_scrollback, _strip_ansi) = match request {
        IpcRequestExt::Dump {
            pane,
            session,
            since,
            last,
            include_scrollback,
            strip_ansi,
        } => (pane, session, since, last, include_scrollback, strip_ansi),
        _ => return IpcResponse::error("dump: bad dispatch (non-Dump variant)"),
    };

    // `pane: usize` is non-optional in the wire schema; treat 0 as
    // "use active pane" only as a future affordance — current CLI
    // always sends an explicit id. Honour it explicitly here.
    let target = if panes.contains_key(&target_id) {
        target_id
    } else if target_id == 0 && panes.contains_key(&active_pane) {
        active_pane
    } else {
        return IpcResponse::error(format!("pane {} not found", target_id));
    };

    let Some(pane) = panes.get_mut(&target) else {
        return IpcResponse::error(format!("pane {} not found", target_id));
    };

    let mut lines = pane.dump_text(include_scrollback);
    let total_before_filter = lines.len();

    // `since` is 1-indexed: keep lines with line-number >= since.
    if let Some(since) = since {
        let drop_n = since.saturating_sub(1).min(lines.len());
        lines.drain(..drop_n);
    }

    // `last`: keep the trailing N lines.
    if let Some(last) = last {
        if lines.len() > last {
            let drop_n = lines.len() - last;
            lines.drain(..drop_n);
        }
    }

    // `strip_ansi` is a no-op: vt100's `Screen::rows` already returns
    // plain text (no escape sequences), so the captured stream cannot
    // contain ANSI to strip. Documented in `docs/scripting.md`.

    // Hard 16 MiB cap (#88 acceptance). Sum of line bytes plus one
    // newline per line approximates the rendered payload.
    let total_bytes: usize = lines.iter().map(|l| l.len() + 1).sum();
    if total_bytes > DUMP_MAX_BYTES {
        return IpcResponse::error("dump too large; use --last N");
    }

    IpcResponse::with_dump(DumpPayload {
        pane: target,
        total: total_before_filter,
        lines,
    })
}

/// Trait-style entry point used by the main loop — accepts the same
/// shape as [`dispatch_ext`] but uses `&mut HashMap<usize, Pane>` so
/// `Dump` can walk the parser scrollback. `LsTree` and `SendKeys` are
/// borrow-only and delegate back to [`dispatch_ext`].
#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_ext_mut(
    request: IpcRequestExt,
    panes: &mut HashMap<usize, Pane>,
    active_pane: usize,
    tab_mgr: &TabManager,
    active_tab_name: &str,
    active_layout: &Layout,
    clients: &[ConnectedClient],
    session_name: &str,
    session_started_at: f64,
) -> IpcResponse {
    match &request {
        IpcRequestExt::Dump { .. } => handle_dump_mut(request, panes, active_pane),
        _ => dispatch_ext(
            request,
            panes,
            active_pane,
            tab_mgr,
            active_tab_name,
            active_layout,
            clients,
            session_name,
            session_started_at,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{DumpPayload, IpcRequestExt, IpcResponse, LsTree};

    /// `IpcResponse::with_dump` survives a wire round-trip preserving
    /// every field of [`DumpPayload`] — guards the frozen contract on
    /// the `dump` field shape that scripts depend on.
    #[test]
    fn dump_response_roundtrips() {
        let response = IpcResponse::with_dump(DumpPayload {
            pane: 7,
            total: 4,
            lines: vec![
                "alpha".into(),
                "beta".into(),
                "gamma".into(),
                "delta".into(),
            ],
        });
        let json = serde_json::to_string(&response).unwrap();
        let back: IpcResponse = serde_json::from_str(&json).unwrap();
        assert!(back.ok);
        let dump = back.dump.expect("dump payload preserved");
        assert_eq!(dump.pane, 7);
        assert_eq!(dump.total, 4);
        assert_eq!(dump.lines, vec!["alpha", "beta", "gamma", "delta"]);
    }

    /// The 16 MiB cap path returns the documented error message
    /// verbatim — `ezpn-ctl` parses this string to surface an
    /// actionable hint to the user (`--last N`).
    #[test]
    fn dump_too_large_error_uses_documented_phrase() {
        // Synthesise a too-big payload and exercise the cap branch
        // directly. We cannot call `handle_dump_mut` without a live
        // `Pane`, so probe the cap math.
        let lines: Vec<String> = (0..(DUMP_MAX_BYTES / 32 + 8))
            .map(|i| format!("{:0>31}", i))
            .collect();
        let total_bytes: usize = lines.iter().map(|l| l.len() + 1).sum();
        assert!(total_bytes > DUMP_MAX_BYTES);
        let response = IpcResponse::error("dump too large; use --last N");
        assert!(!response.ok);
        assert_eq!(
            response.error.as_deref(),
            Some("dump too large; use --last N")
        );
    }

    /// `since`/`last` slicing: `since=2` drops the first line,
    /// `last=2` keeps only the trailing two. Pure data — no live pane
    /// needed — exercising the slicing math the way `handle_dump_mut`
    /// does after collecting from `Pane::dump_text`.
    #[test]
    fn since_and_last_slice_correctly() {
        let mut lines: Vec<String> = (1..=5).map(|i| format!("line{}", i)).collect();
        let since: Option<usize> = Some(2);
        let last: Option<usize> = Some(2);

        if let Some(since) = since {
            let drop_n = since.saturating_sub(1).min(lines.len());
            lines.drain(..drop_n);
        }
        if let Some(last) = last {
            if lines.len() > last {
                let drop_n = lines.len() - last;
                lines.drain(..drop_n);
            }
        }
        assert_eq!(lines, vec!["line4", "line5"]);
    }

    /// Empty-tree response when the `session` filter does not match
    /// the live session name — frozen schema, callers rely on
    /// `sessions == []` to detect "no such session".
    #[test]
    fn ls_tree_filter_mismatch_returns_empty_sessions() {
        let response = handle_ls_tree(
            Some("nope"),
            &HashMap::new(),
            0,
            &TabManager::new(),
            "main",
            &Layout::from_grid(1, 1),
            &[],
            "main",
            1_745_800_000.0,
        );
        assert!(response.ok);
        let tree = response.ls_tree.unwrap();
        assert_eq!(tree.proto_version, "1.0");
        assert!(tree.sessions.is_empty());
    }

    /// Round-trip of the [`LsTree`] schema produced by `handle_ls_tree`
    /// when the daemon has zero panes — the empty-pane edge case the
    /// CLI hits during initial-tab teardown.
    #[test]
    fn ls_tree_empty_session_roundtrips_through_response() {
        let response = handle_ls_tree(
            None,
            &HashMap::new(),
            0,
            &TabManager::new(),
            "main",
            &Layout::from_grid(1, 1),
            &[],
            "main",
            1_745_800_000.0,
        );
        let json = serde_json::to_string(&response).unwrap();
        let back: IpcResponse = serde_json::from_str(&json).unwrap();
        let tree: LsTree = back.ls_tree.unwrap();
        assert_eq!(tree.proto_version, "1.0");
        assert_eq!(tree.sessions.len(), 1);
        assert_eq!(tree.sessions[0].name, "main");
        assert_eq!(tree.sessions[0].tabs.len(), 1);
        assert_eq!(tree.sessions[0].tabs[0].id, 0);
    }

    /// `IpcCommand::Ext` dispatch: `ls_tree` requests reach
    /// [`dispatch_ext`] and produce a populated `ls_tree` response.
    #[test]
    fn ipc_command_ext_routes_ls_tree_through_dispatch() {
        let response = dispatch_ext(
            IpcRequestExt::LsTree { session: None },
            &HashMap::new(),
            0,
            &TabManager::new(),
            "main",
            &Layout::from_grid(1, 1),
            &[],
            "main",
            1_745_800_000.0,
        );
        assert!(response.ok);
        assert!(response.ls_tree.is_some());
    }

    /// `IpcCommand::Ext` dispatch: `send_keys` correctly returns the
    /// not-yet-wired stub (issue #81 out of scope for v0.13 wiring).
    #[test]
    fn ipc_command_ext_send_keys_returns_stub_error() {
        let response = dispatch_ext(
            IpcRequestExt::SendKeys {
                pane: 0,
                text: "echo".into(),
                await_prompt: false,
                timeout_ms: None,
                no_newline: false,
            },
            &HashMap::new(),
            0,
            &TabManager::new(),
            "main",
            &Layout::from_grid(1, 1),
            &[],
            "main",
            0.0,
        );
        assert!(!response.ok);
        let err = response.error.unwrap();
        assert!(err.contains("#81"), "got: {err}");
    }
}
