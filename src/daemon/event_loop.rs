//! The daemon's `run()` event loop.
//!
//! Reads PTY output, restarts dead panes, accepts new clients, drains
//! per-client event channels, applies tab actions, services IPC, renders
//! a frame, broadcasts it, then waits on the wake channel and loops.
//!
//! See issue #24 for the spec deviation: the spec target was ≤450 lines,
//! but `run()` is structurally one big loop with 14+ pieces of mutable
//! state and breaking it up further would require a full state-struct
//! refactor (out of scope for the Tidy First pass).

use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::app::state::RenderUpdate;
use crate::config;
use crate::daemon::dispatch::process_event;
use crate::daemon::render::render_frame_to_buf;
use crate::daemon::router::{accept_client, effective_size};
use crate::daemon::snapshot::capture_workspace;
use crate::daemon::state::{
    ClientMsg, ConnectedClient, DragState, InputMode, TabAction, TextSelection,
};
use crate::daemon::writer::OutboundMsg;
use crate::ipc;
use crate::layout::Layout;
use crate::pane::PaneLaunch;
use crate::project;
use crate::protocol;
use crate::render::{self, BorderCache};
use crate::session;
use crate::settings::Settings;
use crate::signals::{Signal as Sig, SignalHandlers};
use crate::workspace::{self};

/// Run the server daemon. This function does not return until all panes die
/// or the server is killed.
pub fn run(session_name: &str, args: &[String]) -> anyhow::Result<()> {
    let config = crate::cli::parse::parse_args_from(args)?;

    // Load config file defaults
    let file_config = config::load_config();
    let effective_scrollback = file_config.scrollback;
    let mut default_shell = if config.has_shell_override {
        // [perf:init] clone here: read once at daemon startup; needed because
        // `config` is borrowed below for other fields. Cold path.
        config.shell.clone()
    } else {
        file_config.shell
    };
    let effective_border = if config.has_border_override {
        config.border
    } else {
        file_config.border
    };
    let theme = crate::theme::load_theme(&file_config.theme).adapt(crate::theme::detect_caps());
    let mut settings = Settings::with_theme(effective_border, theme);
    settings.show_status_bar = file_config.show_status_bar;
    settings.show_tab_bar = file_config.show_tab_bar;
    let prefix_key = file_config.prefix_key;

    // Install POSIX signal handlers (issue #11). Failure to install is logged
    // but non-fatal — the daemon still runs, just without graceful shutdown
    // and zombie reaping. We never want a syscall failure to block startup.
    let mut sig_handlers = match SignalHandlers::install() {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("ezpn: signal handlers unavailable, running without them: {e}");
            None
        }
    };

    // Auto-restart state
    let mut restart_policies: HashMap<usize, project::RestartPolicy> = HashMap::new();

    // Scrollback persistence opt-in: starts from the global config and may be
    // overridden by `.ezpn.toml`'s `[workspace] persist_scrollback`.
    let mut persist_scrollback = file_config.persist_scrollback;

    // Build layout and spawn panes (same logic as direct mode)
    let (mut layout, mut panes, mut active, snapshot_extra) =
        crate::app::bootstrap::build_initial_state(
            &config,
            &mut default_shell,
            &mut settings,
            &mut restart_policies,
            effective_scrollback,
            &mut persist_scrollback,
        )?;

    let mut drag: Option<DragState> = None;
    let mut zoomed_pane: Option<usize> = None;
    let mut last_click: Option<(Instant, u16, u16)> = None;
    let mut broadcast = false;
    let mut last_active: usize = active;
    let mut selection_anchor: Option<(usize, u16, u16)> = None;
    let mut text_selection: Option<TextSelection> = None;

    let mut restart_state: HashMap<usize, (Instant, u32)> = HashMap::new();

    // Tab management
    use crate::tab::{Tab, TabManager};
    let mut tab_mgr = TabManager::new();
    let mut tab_name = String::from("1");
    let mut tab_names_cache: Vec<(usize, String, bool)> = Vec::new();
    let mut tab_names_dirty = true;

    // Restore all tabs from snapshot, preserving original order.
    if let Some(extra) = snapshot_extra {
        if extra.all_tabs.len() > 1 {
            // Spawn ALL tabs in original order, using snapshot's scrollback
            let mut all_spawned: Vec<Tab> = Vec::with_capacity(extra.all_tabs.len());
            let snap_scrollback = extra.scrollback;

            for tab_snap in &extra.all_tabs {
                let tab_panes = crate::app::lifecycle::spawn_snapshot_panes(
                    &tab_snap.layout,
                    tab_snap,
                    &default_shell,
                    80,
                    24,
                    &settings,
                    snap_scrollback,
                )?;
                let mut tab_restart = HashMap::new();
                for ps in &tab_snap.panes {
                    if ps.restart != project::RestartPolicy::Never {
                        // [perf:init] clone here: tab restore at daemon startup.
                        // `RestartPolicy` is a small Copy-able enum but currently
                        // owns a `Vec<u8>` for the deprecated `Custom` variant,
                        // so it's not `Copy`. One-off cost per pane on cold start.
                        tab_restart.insert(ps.id, ps.restart.clone());
                    }
                }
                let mut tab = Tab::new(
                    // [perf:init] clone here: tab metadata into Tab on startup.
                    tab_snap.name.clone(),
                    // [perf:init] clone here: snapshot Layout owns Vec<usize>
                    // for pane IDs; cloning happens once per tab on cold start.
                    // TODO(perf): wrap `Layout` in `Arc` once snapshot schema
                    // bumps to v4 — would let cold-start path share the Arc
                    // instead of duplicating the layout tree.
                    tab_snap.layout.clone(),
                    tab_panes,
                    tab_snap.active_pane,
                );
                tab.restart_policies = tab_restart;
                tab.zoomed_pane = tab_snap.zoomed_pane;
                tab.broadcast = tab_snap.broadcast;
                all_spawned.push(tab);
            }

            // Kill the panes from build_initial_state (we re-spawned everything)
            crate::app::lifecycle::kill_all_panes(&mut panes);

            // Build TabManager with correct order; active tab is unpacked
            let (new_mgr, active_tab) = TabManager::from_tabs(all_spawned, extra.active_tab_idx);
            tab_mgr = new_mgr;
            tab_name = active_tab.name;
            layout = active_tab.layout;
            panes = active_tab.panes;
            active = active_tab.active_pane;
            restart_policies = active_tab.restart_policies;
            restart_state = active_tab.restart_state;
            zoomed_pane = active_tab.zoomed_pane;
            broadcast = active_tab.broadcast;
        } else {
            // Single tab — just apply metadata
            let snap = &extra.all_tabs[0];
            // [perf:init] clone here: single-tab restore on cold start.
            tab_name = snap.name.clone();
            zoomed_pane = snap.zoomed_pane;
            broadcast = snap.broadcast;
        }
    }
    const MAX_RESTART_RETRIES: u32 = 10;
    const RESTART_DELAY: Duration = Duration::from_secs(2);
    const RESTART_BACKOFF_THRESHOLD: u32 = 3;

    let mut mode = InputMode::Normal;
    let mut tw: u16 = 80;
    let mut th: u16 = 24;

    // Init wake channel — PTY reader threads will wake us via this channel
    let wake_rx = crate::pane::init_wake_channel();

    // Start IPC listener (existing ezpn-ctl support)
    let ipc_rx = ipc::start_listener()
        .map_err(|e| eprintln!("ezpn-server: IPC unavailable ({e})"))
        .ok();

    // Create session socket and listen for client connections.
    //
    // Bind safety (issue #22): the higher-level `resolve_session_name` in
    // `main` already arbitrates collisions, but a narrow race remains
    // between resolve and bind when two `ezpn` processes target the same
    // free slot in the same millisecond. If the first bind fails with
    // `EADDRINUSE`, we re-probe the existing socket: if it's a stale file
    // (no live server answering C_PING), unlink and retry once. If the
    // sibling is genuinely live we propagate the error rather than
    // silently renaming, since the parent (`spawn_server`) is polling the
    // original `session_name` and a rename would break its readiness probe.
    let sock_path = session::socket_path(session_name);
    let _ = std::fs::remove_file(&sock_path);
    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if !session::is_alive(&sock_path) {
                let _ = std::fs::remove_file(&sock_path);
                UnixListener::bind(&sock_path)?
            } else {
                return Err(e.into());
            }
        }
        Err(e) => return Err(e.into()),
    };
    listener.set_nonblocking(true)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
    }

    // Issue #13: signal the parent (`session::spawn_server`) that the
    // socket is bound and accepting connections. The parent blocks on
    // `poll(2)` for this byte instead of polling the socket every 50 ms,
    // dropping warm-attach latency to a few ms. If the env var isn't set
    // (e.g. the daemon was launched manually for tests), this is a no-op.
    if let Ok(fd_str) = std::env::var(session::READY_FD_ENV) {
        if let Ok(fd) = fd_str.parse::<i32>() {
            unsafe {
                let _ = libc::write(fd, b"1".as_ptr() as *const _, 1);
                libc::close(fd);
            }
        }
        // Don't leave the env around — child PTYs would inherit it,
        // which is meaningless to them and could mask debugging.
        std::env::remove_var(session::READY_FD_ENV);
    }

    let mut clients: Vec<ConnectedClient> = Vec::new();
    let mut border_cache: Option<BorderCache> = None;
    let mut render_buf: Vec<u8> = Vec::with_capacity(64 * 1024); // Reusable render buffer

    let mut prev_active = active;

    loop {
        // Track last-active pane + synthesize focus events
        if active != prev_active {
            // Send FocusOut to old pane if it wants focus events
            if let Some(old_pane) = panes.get_mut(&prev_active) {
                if old_pane.is_alive() && old_pane.wants_focus() {
                    old_pane.write_bytes(b"\x1b[O");
                }
            }
            // Send FocusIn to new pane
            if let Some(new_pane) = panes.get_mut(&active) {
                if new_pane.is_alive() && new_pane.wants_focus() {
                    new_pane.write_bytes(b"\x1b[I");
                }
            }
            last_active = prev_active;
            prev_active = active;
        }

        let mut update = RenderUpdate::default();

        // ── Read PTY output ──
        for (&pid, pane) in &mut panes {
            if pane.read_output() {
                update.dirty_panes.insert(pid);
            }
            // Forward OSC 52 clipboard sequences from child to all clients
            let osc52_seqs = pane.take_osc52();
            if !osc52_seqs.is_empty() {
                for c in &mut clients {
                    for seq in &osc52_seqs {
                        let _ = c.outbound_tx.try_send(OutboundMsg::Output(seq.clone()));
                    }
                }
            }
        }

        // ── Auto-restart dead panes ──
        {
            let dead_restartable: Vec<usize> = panes
                .iter()
                .filter(|(pid, pane)| {
                    !pane.is_alive()
                        && restart_policies.get(pid).is_some_and(|p| {
                            *p == project::RestartPolicy::Always
                                || *p == project::RestartPolicy::OnFailure
                        })
                })
                .map(|(&pid, _)| pid)
                .collect();

            for pid in dead_restartable {
                let (last_death, retries) = restart_state
                    .entry(pid)
                    .or_insert((Instant::now() - RESTART_DELAY, 0));

                if *retries >= MAX_RESTART_RETRIES {
                    continue;
                }

                let delay = if *retries >= RESTART_BACKOFF_THRESHOLD {
                    RESTART_DELAY * (*retries - RESTART_BACKOFF_THRESHOLD + 1)
                } else {
                    RESTART_DELAY
                };

                if last_death.elapsed() < delay {
                    continue;
                }

                let (launch, old_name, pane_shell) = panes
                    .get(&pid)
                    .map(|p| {
                        (
                            // [perf:cold] clone here: auto-restart of a dead
                            // pane fires at most once every RESTART_DELAY per
                            // pane, then with backoff. Cloning `PaneLaunch`
                            // is dominated by the fork+exec that follows.
                            p.launch().clone(),
                            p.name().map(String::from),
                            p.initial_shell().map(String::from),
                        )
                    })
                    .unwrap_or((PaneLaunch::Shell, None, None));
                let effective_shell = pane_shell.as_deref().unwrap_or(&default_shell);
                if crate::app::lifecycle::replace_pane(
                    &mut panes,
                    &layout,
                    pid,
                    launch,
                    effective_shell,
                    tw,
                    th,
                    &settings,
                    effective_scrollback,
                )
                .is_ok()
                {
                    if let Some(pane) = panes.get_mut(&pid) {
                        pane.set_name(old_name);
                        if let Some(ref shell_override) = pane_shell {
                            // [perf:cold] clone here: re-applies the pane's
                            // shell override after auto-restart.
                            pane.set_initial_shell(Some(shell_override.clone()));
                        }
                    }
                    *retries += 1;
                    *last_death = Instant::now();
                    update.dirty_panes.insert(pid);
                }
            }
        }

        // ── Check all dead (active tab) ──
        let active_tab_dead = panes.is_empty()
            || panes.iter().all(|(pid, pane)| {
                if pane.is_alive() {
                    return false;
                }
                let has_restart = restart_policies.get(pid).is_some_and(|p| {
                    *p == project::RestartPolicy::Always || *p == project::RestartPolicy::OnFailure
                });
                if !has_restart {
                    return true;
                }
                restart_state
                    .get(pid)
                    .is_some_and(|(_, retries)| *retries >= MAX_RESTART_RETRIES)
            });
        if active_tab_dead {
            if tab_mgr.count > 1 {
                // Other tabs still alive — auto-close this dead tab and switch
                crate::app::lifecycle::kill_all_panes(&mut panes);
                if let Some(new_tab) = tab_mgr.close_active() {
                    tab_name = new_tab.name;
                    layout = new_tab.layout;
                    panes = new_tab.panes;
                    active = new_tab.active_pane;
                    restart_policies = new_tab.restart_policies;
                    restart_state = new_tab.restart_state;
                    zoomed_pane = new_tab.zoomed_pane;
                    broadcast = new_tab.broadcast;
                    drag = None;
                    selection_anchor = None;
                    text_selection = None;
                    last_click = None;
                    last_active = active;
                    prev_active = active;
                    mode = InputMode::Normal;
                    crate::app::lifecycle::resize_all(&mut panes, &layout, tw, th, &settings);
                    border_cache = Some(render::build_border_cache_with_style(
                        &layout,
                        settings.show_status_bar,
                        tw,
                        th,
                        settings.border_style,
                    ));
                    update.mark_all(&layout);
                    update.border_dirty = true;
                }
            } else {
                // Last tab — exit server
                for c in &mut clients {
                    let _ = c.outbound_tx.try_send(OutboundMsg::Exit);
                }
                break;
            }
        }

        // Unzoom if zoomed pane no longer exists
        if let Some(zpid) = zoomed_pane {
            if !panes.contains_key(&zpid) {
                zoomed_pane = None;
                crate::app::lifecycle::resize_all(&mut panes, &layout, tw, th, &settings);
                update.mark_all(&layout);
                update.border_dirty = true;
            }
        }

        // Prefix mode timeout
        if let InputMode::Prefix { entered_at } = &mode {
            if entered_at.elapsed() > Duration::from_secs(3) {
                mode = InputMode::Normal;
                update.full_redraw = true;
            }
        }

        // ── Accept new connections with handshake ──
        // Read the first message to determine intent:
        //   C_PING   → respond with S_PONG, close (no side effects)
        //   C_KILL   → kill server
        //   C_RESIZE → legacy client attach (steal mode)
        //   C_ATTACH → new protocol attach with mode
        if let Ok((conn, _)) = listener.accept() {
            conn.set_nonblocking(false).ok();
            // Short timeout for handshake — fail-close if setting fails
            if conn
                .set_read_timeout(Some(Duration::from_millis(100)))
                .is_err()
            {
                drop(conn);
            } else {
                // The first message may be C_HELLO; if so, negotiate and read again.
                // This is the only place version negotiation happens — once accepted,
                // the rest of the connection uses tags as defined in protocol.rs.
                let mut negotiated_caps: u32 = 0;
                let mut first_msg = protocol::read_msg(&mut &conn);
                if let Ok((protocol::C_HELLO, ref payload)) = first_msg {
                    match serde_json::from_slice::<protocol::HelloMessage>(payload) {
                        Ok(hello) if hello.version == protocol::PROTOCOL_VERSION => {
                            negotiated_caps = hello.capabilities & protocol::SERVER_CAPABILITIES;
                            let ok = protocol::HelloOk {
                                version: protocol::PROTOCOL_VERSION,
                                capabilities: negotiated_caps,
                                server: format!("ezpn {}", env!("CARGO_PKG_VERSION")),
                            };
                            let mut w = &conn;
                            let _ = protocol::write_msg(
                                &mut w,
                                protocol::S_HELLO_OK,
                                &serde_json::to_vec(&ok).unwrap_or_default(),
                            );
                            // Read the real first message (attach / ping / kill / …)
                            first_msg = protocol::read_msg(&mut &conn);
                        }
                        _ => {
                            // Mismatched major or malformed payload → reject + close.
                            let err = protocol::HelloErr {
                                reason:
                                    "client/server protocol version mismatch — please upgrade ezpn"
                                        .to_string(),
                                server_version: protocol::PROTOCOL_VERSION,
                            };
                            let mut w = &conn;
                            let _ = protocol::write_msg(
                                &mut w,
                                protocol::S_HELLO_ERR,
                                &serde_json::to_vec(&err).unwrap_or_default(),
                            );
                            drop(conn);
                            continue;
                        }
                    }
                }

                match first_msg {
                    Ok((protocol::C_PING, _)) => {
                        // Liveness probe — respond and close, no side effects
                        let mut w = &conn;
                        let _ = protocol::write_msg(&mut w, protocol::S_PONG, &[]);
                    }
                    Ok((protocol::C_KILL, _)) => {
                        // Kill server: kill all panes and exit
                        for pane in panes.values_mut() {
                            pane.kill();
                        }
                        for c in &mut clients {
                            let _ = c.outbound_tx.try_send(OutboundMsg::Exit);
                        }
                        session::cleanup(session_name);
                        ipc::cleanup();
                        return Ok(());
                    }
                    Ok((protocol::C_ATTACH, payload)) => {
                        // New protocol: attach with mode
                        if let Ok(req) = serde_json::from_slice::<protocol::AttachRequest>(&payload)
                        {
                            accept_client(
                                conn,
                                req.cols,
                                req.rows,
                                req.mode,
                                negotiated_caps,
                                &mut clients,
                                &mut panes,
                                &layout,
                                &settings,
                                &mut tw,
                                &mut th,
                                &mut drag,
                                zoomed_pane,
                                &mut update,
                            );
                        }
                    }
                    Ok((protocol::C_RESIZE, payload)) => {
                        // Legacy client attach — always steal mode
                        if let Some((w, h)) = protocol::decode_resize(&payload) {
                            accept_client(
                                conn,
                                w,
                                h,
                                protocol::AttachMode::Steal,
                                negotiated_caps,
                                &mut clients,
                                &mut panes,
                                &layout,
                                &settings,
                                &mut tw,
                                &mut th,
                                &mut drag,
                                zoomed_pane,
                                &mut update,
                            );
                        }
                    }
                    _ => {
                        // Unknown first message or disconnected → ignore
                    }
                }
            }
        }

        // ── Process client events from all clients ──
        let mut detach_ids: Vec<u64> = Vec::new();
        let mut disconnect_ids: Vec<u64> = Vec::new();
        let mut kill_requested = false;
        let mut detach_all = false; // Set by Ctrl+B d via process_key
        let mut tab_action = TabAction::None;
        let mut size_changed = false;
        let current_tab_names = tab_mgr.tab_names(&tab_name);

        for client in &mut clients {
            let client_mode = client.mode;
            let client_id = client.id;
            loop {
                match client.event_rx.try_recv() {
                    Ok(ClientMsg::Event(event)) => {
                        // Readonly clients cannot send input
                        if client_mode == protocol::AttachMode::Readonly {
                            continue;
                        }
                        process_event(
                            event,
                            &mut mode,
                            &mut layout,
                            &mut panes,
                            &mut active,
                            &mut settings,
                            &mut update,
                            &mut drag,
                            &mut zoomed_pane,
                            &mut last_click,
                            &mut broadcast,
                            &mut last_active,
                            &mut selection_anchor,
                            &mut text_selection,
                            &default_shell,
                            tw,
                            th,
                            effective_scrollback,
                            &border_cache,
                            &mut detach_all,
                            &mut tab_action,
                            &current_tab_names,
                            prefix_key,
                        );
                    }
                    Ok(ClientMsg::Resize(w, h)) => {
                        client.tw = w;
                        client.th = h;
                        size_changed = true;
                    }
                    Ok(ClientMsg::Detach) => {
                        detach_ids.push(client_id);
                        break;
                    }
                    Ok(ClientMsg::Disconnected) => {
                        disconnect_ids.push(client_id);
                        break;
                    }
                    Ok(ClientMsg::Panicked(reason)) => {
                        eprintln!(
                            "ezpn: client reader thread panicked (id={}): {}",
                            client_id, reason
                        );
                        disconnect_ids.push(client_id);
                        break;
                    }
                    Ok(ClientMsg::Kill) => {
                        kill_requested = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnect_ids.push(client_id);
                        break;
                    }
                }
            }
            if kill_requested {
                break;
            }
        }

        if kill_requested {
            for pane in panes.values_mut() {
                pane.kill();
            }
            for c in &mut clients {
                let _ = c.outbound_tx.try_send(OutboundMsg::Exit);
            }
            session::cleanup(session_name);
            ipc::cleanup();
            return Ok(());
        }

        // Handle per-client resize
        if size_changed {
            let (ew, eh) = effective_size(&clients);
            if ew != tw || eh != th {
                tw = ew;
                th = eh;
                drag = None;
                crate::app::lifecycle::resize_all(&mut panes, &layout, tw, th, &settings);
                if let Some(zpid) = zoomed_pane {
                    crate::app::render_ctl::resize_zoomed_pane(&mut panes, zpid, tw, th, &settings);
                }
                update.mark_all(&layout);
                update.border_dirty = true;
            }
        }

        // Pre-fill rename buffer with current tab name (one-shot on transition)
        if let InputMode::RenameTab { ref mut buffer } = mode {
            // Sentinel: "\0" means "just entered, needs pre-fill"
            if buffer == "\0" {
                // [perf:cold] clone here: pre-fills the rename text input on
                // mode entry. Fires at most once per rename gesture.
                *buffer = tab_name.clone();
            }
        }

        // detach_all (from Ctrl+B d): in steal mode, detach all clients.
        // In shared/readonly mode, only detach writable clients (the ones that
        // could have triggered the detach).
        if detach_all {
            for c in &clients {
                if c.mode != protocol::AttachMode::Readonly {
                    detach_ids.push(c.id);
                }
            }
        }

        // Handle detach/disconnect — auto-save when last client leaves
        let had_clients = !clients.is_empty();
        for id in &detach_ids {
            if let Some(pos) = clients.iter().position(|c| c.id == *id) {
                let _ = clients[pos].outbound_tx.try_send(OutboundMsg::Detached);
                clients.remove(pos);
            }
        }
        for id in &disconnect_ids {
            if let Some(pos) = clients.iter().position(|c| c.id == *id) {
                clients.remove(pos);
            }
        }
        // Recompute effective size after any client changes
        if !detach_ids.is_empty() || !disconnect_ids.is_empty() {
            let (ew, eh) = effective_size(&clients);
            if ew != tw || eh != th {
                tw = ew;
                th = eh;
                drag = None;
                crate::app::lifecycle::resize_all(&mut panes, &layout, tw, th, &settings);
                if let Some(zpid) = zoomed_pane {
                    crate::app::render_ctl::resize_zoomed_pane(&mut panes, zpid, tw, th, &settings);
                }
                update.mark_all(&layout);
                update.border_dirty = true;
            }
        }

        if had_clients
            && clients.is_empty()
            && (!detach_ids.is_empty() || !disconnect_ids.is_empty())
        {
            // All clients gone — auto-save and reset input state
            let snapshot = capture_workspace(
                &tab_mgr,
                &tab_name,
                &layout,
                &panes,
                active,
                zoomed_pane,
                broadcast,
                &restart_policies,
                &default_shell,
                settings.border_style,
                settings.show_status_bar,
                settings.show_tab_bar,
                effective_scrollback,
                persist_scrollback,
            );
            workspace::auto_save(session_name, &snapshot);
            mode = InputMode::Normal;
            drag = None;
            selection_anchor = None;
            text_selection = None;
            last_click = None;
        }

        // ── Handle tab actions ──
        match tab_action {
            TabAction::NewTab => {
                // Save current tab state
                let current_tab = Tab::new(
                    std::mem::take(&mut tab_name),
                    std::mem::replace(&mut layout, Layout::from_grid(1, 1)),
                    std::mem::take(&mut panes),
                    active,
                );
                // Transfer per-tab state
                let mut saved = current_tab;
                saved.restart_policies = std::mem::take(&mut restart_policies);
                saved.restart_state = std::mem::take(&mut restart_state);
                saved.zoomed_pane = zoomed_pane.take();
                saved.broadcast = broadcast;

                tab_name = tab_mgr.create_tab(saved);
                // Create new tab with a single shell pane
                layout = Layout::from_grid(1, 1);
                let inner = crate::app::render_ctl::make_inner(tw, th, settings.show_status_bar);
                let rects = layout.pane_rects(&inner);
                let (&pid, rect) = rects.iter().next().unwrap();
                match crate::app::lifecycle::spawn_pane(
                    &default_shell,
                    &PaneLaunch::Shell,
                    rect.w.max(1),
                    rect.h.max(1),
                    effective_scrollback,
                ) {
                    Ok(p) => {
                        panes.insert(pid, p);
                        active = pid;
                        broadcast = false;
                        restart_policies.clear();
                        restart_state.clear();
                        drag = None;
                        selection_anchor = None;
                        text_selection = None;
                        last_click = None;
                        last_active = active;
                        prev_active = active;
                        mode = InputMode::Normal;
                        border_cache = Some(render::build_border_cache_with_style(
                            &layout,
                            settings.show_status_bar,
                            tw,
                            th,
                            settings.border_style,
                        ));
                        update.mark_all(&layout);
                        update.border_dirty = true;
                        tab_names_dirty = true;
                    }
                    Err(_) => {
                        // Spawn failed — revert: close this empty tab and restore previous
                        if let Some(restored) = tab_mgr.close_active() {
                            tab_name = restored.name;
                            layout = restored.layout;
                            panes = restored.panes;
                            active = restored.active_pane;
                            restart_policies = restored.restart_policies;
                            restart_state = restored.restart_state;
                            zoomed_pane = restored.zoomed_pane;
                            broadcast = restored.broadcast;
                            crate::app::lifecycle::resize_all(
                                &mut panes, &layout, tw, th, &settings,
                            );
                            border_cache = Some(render::build_border_cache_with_style(
                                &layout,
                                settings.show_status_bar,
                                tw,
                                th,
                                settings.border_style,
                            ));
                            update.mark_all(&layout);
                            update.border_dirty = true;
                            tab_names_dirty = true;
                        }
                    }
                }
            }
            TabAction::NextTab | TabAction::PrevTab | TabAction::GoToTab(_) => {
                let target = match tab_action {
                    TabAction::NextTab => tab_mgr.next_idx(),
                    TabAction::PrevTab => tab_mgr.prev_idx(),
                    TabAction::GoToTab(idx) => idx,
                    _ => unreachable!(),
                };
                if target != tab_mgr.active_idx && target < tab_mgr.count {
                    let current_tab = Tab {
                        name: std::mem::take(&mut tab_name),
                        layout: std::mem::replace(&mut layout, Layout::from_grid(1, 1)),
                        panes: std::mem::take(&mut panes),
                        active_pane: active,
                        restart_policies: std::mem::take(&mut restart_policies),
                        restart_state: std::mem::take(&mut restart_state),
                        zoomed_pane: zoomed_pane.take(),
                        broadcast,
                    };
                    if let Some(new_tab) = tab_mgr.switch_to(target, current_tab) {
                        tab_name = new_tab.name;
                        layout = new_tab.layout;
                        panes = new_tab.panes;
                        active = new_tab.active_pane;
                        restart_policies = new_tab.restart_policies;
                        restart_state = new_tab.restart_state;
                        zoomed_pane = new_tab.zoomed_pane;
                        broadcast = new_tab.broadcast;
                        // Reset per-interaction state on tab switch
                        drag = None;
                        selection_anchor = None;
                        text_selection = None;
                        last_click = None;
                        last_active = active;
                        prev_active = active;
                        mode = InputMode::Normal;
                        crate::app::lifecycle::resize_all(&mut panes, &layout, tw, th, &settings);
                        border_cache = Some(render::build_border_cache_with_style(
                            &layout,
                            settings.show_status_bar,
                            tw,
                            th,
                            settings.border_style,
                        ));
                        update.mark_all(&layout);
                        update.border_dirty = true;
                        tab_names_dirty = true;
                    }
                }
            }
            TabAction::CloseTab => {
                if tab_mgr.count > 1 {
                    crate::app::lifecycle::kill_all_panes(&mut panes);
                    if let Some(new_tab) = tab_mgr.close_active() {
                        tab_name = new_tab.name;
                        layout = new_tab.layout;
                        panes = new_tab.panes;
                        active = new_tab.active_pane;
                        restart_policies = new_tab.restart_policies;
                        restart_state = new_tab.restart_state;
                        zoomed_pane = new_tab.zoomed_pane;
                        broadcast = new_tab.broadcast;
                        drag = None;
                        selection_anchor = None;
                        text_selection = None;
                        last_click = None;
                        last_active = active;
                        prev_active = active;
                        mode = InputMode::Normal;
                        crate::app::lifecycle::resize_all(&mut panes, &layout, tw, th, &settings);
                        border_cache = Some(render::build_border_cache_with_style(
                            &layout,
                            settings.show_status_bar,
                            tw,
                            th,
                            settings.border_style,
                        ));
                        update.mark_all(&layout);
                        update.border_dirty = true;
                        tab_names_dirty = true;
                    }
                }
            }
            TabAction::Rename(new_name) => {
                tab_name = new_name;
                update.full_redraw = true;
                tab_names_dirty = true;
            }
            TabAction::KillSession => {
                // Kill all panes in all tabs
                crate::app::lifecycle::kill_all_panes(&mut panes);
                tab_mgr.kill_all_inactive();
                for c in &mut clients {
                    let _ = c.outbound_tx.try_send(OutboundMsg::Exit);
                }
                session::cleanup(session_name);
                ipc::cleanup();
                return Ok(());
            }
            TabAction::None => {}
        }

        // ── Handle IPC commands ──
        if let Some(ref rx) = ipc_rx {
            while let Ok((cmd, resp_tx)) = rx.try_recv() {
                // Intercept Save/Load to use full-session snapshots (with all tabs)
                if let ipc::IpcRequest::Save { ref path } = cmd {
                    let snapshot = capture_workspace(
                        &tab_mgr,
                        &tab_name,
                        &layout,
                        &panes,
                        active,
                        zoomed_pane,
                        broadcast,
                        &restart_policies,
                        &default_shell,
                        settings.border_style,
                        settings.show_status_bar,
                        settings.show_tab_bar,
                        effective_scrollback,
                        persist_scrollback,
                    );
                    let response = match workspace::save_snapshot(path, &snapshot) {
                        Ok(()) => ipc::IpcResponse::success(format!("saved {}", path)),
                        Err(error) => ipc::IpcResponse::error(error.to_string()),
                    };
                    let _ = resp_tx.send(response);
                    continue;
                }

                // Intercept Load for full-session restore (all tabs)
                if let ipc::IpcRequest::Load { ref path } = cmd {
                    let load_result: anyhow::Result<()> = (|| {
                        let snapshot = workspace::load_snapshot(path)?;
                        let snap_scrollback = snapshot.scrollback;

                        // Kill current session
                        crate::app::lifecycle::kill_all_panes(&mut panes);
                        tab_mgr.kill_all_inactive();

                        // [perf:cold] clone here: IPC `Load` runs out of band
                        // (user-initiated `ezpn-ctl load`); cloning the shell
                        // string and theme are negligible vs. the spawn storm
                        // that follows.
                        default_shell = snapshot.shell.clone();
                        // [perf:cold] clone here: theme preserved across the
                        // Settings rebuild on workspace load.
                        let preserved_theme = settings.theme.clone();
                        settings = Settings::with_theme(snapshot.border_style, preserved_theme);
                        settings.show_status_bar = snapshot.show_status_bar;
                        settings.show_tab_bar = snapshot.show_tab_bar;

                        // Spawn all tabs in order
                        let mut all_tabs: Vec<Tab> = Vec::new();
                        for tab_snap in &snapshot.tabs {
                            let tp = crate::app::lifecycle::spawn_snapshot_panes(
                                &tab_snap.layout,
                                tab_snap,
                                &default_shell,
                                tw,
                                th,
                                &settings,
                                snap_scrollback,
                            )?;
                            let mut tr = HashMap::new();
                            for ps in &tab_snap.panes {
                                if ps.restart != project::RestartPolicy::Never {
                                    // [perf:cold] clone here: per-pane restart
                                    // policy on IPC `Load` (user-initiated).
                                    tr.insert(ps.id, ps.restart.clone());
                                }
                            }
                            let mut tab = Tab::new(
                                // [perf:cold] clone here: tab name on Load.
                                tab_snap.name.clone(),
                                // [perf:cold] clone here: tab layout on Load.
                                // TODO(perf): same Arc<Layout> conversion as
                                // the cold-start path would help here too.
                                tab_snap.layout.clone(),
                                tp,
                                tab_snap.active_pane,
                            );
                            tab.restart_policies = tr;
                            tab.zoomed_pane = tab_snap.zoomed_pane;
                            tab.broadcast = tab_snap.broadcast;
                            all_tabs.push(tab);
                        }

                        let (new_mgr, active_tab) =
                            TabManager::from_tabs(all_tabs, snapshot.active_tab);
                        tab_mgr = new_mgr;
                        tab_name = active_tab.name;
                        layout = active_tab.layout;
                        panes = active_tab.panes;
                        active = active_tab.active_pane;
                        restart_policies = active_tab.restart_policies;
                        restart_state = active_tab.restart_state;
                        zoomed_pane = active_tab.zoomed_pane;
                        broadcast = active_tab.broadcast;
                        tab_names_dirty = true;
                        Ok(())
                    })();

                    match load_result {
                        Ok(()) => {
                            update.mark_all(&layout);
                            update.border_dirty = true;
                            let _ =
                                resp_tx.send(ipc::IpcResponse::success(format!("loaded {}", path)));
                        }
                        Err(e) => {
                            let _ = resp_tx.send(ipc::IpcResponse::error(e.to_string()));
                        }
                    }
                    continue;
                }

                let (response, mut ipc_update) = crate::app::input_dispatch::handle_ipc_command(
                    cmd,
                    &mut layout,
                    &mut panes,
                    &mut active,
                    &mut default_shell,
                    tw,
                    th,
                    &mut settings,
                    effective_scrollback,
                );
                update.merge(&mut ipc_update);
                let _ = resp_tx.send(response);
            }
        }

        // ── Render and send to client ──
        if update.border_dirty {
            border_cache = Some(render::build_border_cache_with_style(
                &layout,
                settings.show_status_bar,
                tw,
                th,
                settings.border_style,
            ));
        }

        if zoomed_pane.is_some() {
            zoomed_pane = Some(active);
            crate::app::render_ctl::resize_zoomed_pane(&mut panes, active, tw, th, &settings);
        }

        let render_needed = update.needs_render();
        if render_needed && !clients.is_empty() {
            if let Some(ref cache) = border_cache {
                if tab_names_dirty {
                    tab_names_cache = tab_mgr.tab_names(&tab_name);
                    tab_names_dirty = false;
                }

                let sel_for_render = text_selection.as_ref().map(|s| {
                    let (sr, sc, er, ec) = s.normalized();
                    (s.pane_id, sr, sc, er, ec)
                });
                let needs_selection_chars =
                    zoomed_pane.is_none() && settings.show_status_bar && text_selection.is_some();
                let render_targets = crate::app::render_ctl::collect_render_targets(
                    &panes,
                    &update.dirty_panes,
                    update.full_redraw,
                    zoomed_pane,
                    needs_selection_chars
                        .then(|| text_selection.as_ref().map(|s| s.pane_id))
                        .flatten(),
                );
                crate::app::render_ctl::sync_render_targets(&mut panes, &render_targets);
                let selection_chars = if needs_selection_chars {
                    crate::app::render_ctl::selection_char_count_from_synced(&panes, sel_for_render)
                } else {
                    0
                };

                // Render once (smallest-client policy: all clients see the same frame)
                render_buf.clear();
                let render_result = render_frame_to_buf(
                    &mut render_buf,
                    &panes,
                    &layout,
                    active,
                    &settings,
                    tw,
                    th,
                    drag.is_some(),
                    cache,
                    &update.dirty_panes,
                    update.full_redraw,
                    &mode,
                    broadcast,
                    sel_for_render,
                    selection_chars,
                    zoomed_pane,
                    &default_shell,
                    &tab_names_cache,
                );

                crate::app::render_ctl::reset_render_targets(&mut panes, &render_targets);

                // Broadcast frame to all clients; remove failed ones
                if render_result.is_ok() && !render_buf.is_empty() {
                    // Per SPEC 01 §4.1: outbound writes go through the bounded
                    // per-client queue; `try_send` returning Full means the
                    // writer thread is wedged behind a slow peer — treat the
                    // client as dead. The writer will also independently emit
                    // `ClientMsg::Disconnected` after MAX_WOULDBLOCKS timeouts.
                    clients.retain(|c| {
                        match c
                            .outbound_tx
                            .try_send(OutboundMsg::Frame(render_buf.clone()))
                        {
                            Ok(()) => true,
                            Err(mpsc::TrySendError::Full(_))
                            | Err(mpsc::TrySendError::Disconnected(_)) => {
                                let _ = c.outbound_tx.try_send(OutboundMsg::Detached);
                                false
                            }
                        }
                    });
                }
            }
        }

        // ── POSIX signal handling (issue #11) ──
        if let Some(sh) = sig_handlers.as_mut() {
            for sig in sh.drain() {
                match sig {
                    Sig::Child => {
                        // Reap any pane whose child exited externally (e.g. user
                        // killed it from another terminal). update_alive() is a
                        // no-op for already-dead panes, so iterating all is cheap.
                        let mut any_changed = false;
                        for pane in panes.values_mut() {
                            if pane.update_alive().is_some() {
                                any_changed = true;
                            }
                        }
                        if any_changed {
                            update.full_redraw = true;
                        }
                    }
                    Sig::Terminate => {
                        // Graceful shutdown: persist the live workspace snapshot
                        // before tearing down so reattach restores layout/commands.
                        let snapshot = capture_workspace(
                            &tab_mgr,
                            &tab_name,
                            &layout,
                            &panes,
                            active,
                            zoomed_pane,
                            broadcast,
                            &restart_policies,
                            &default_shell,
                            settings.border_style,
                            settings.show_status_bar,
                            settings.show_tab_bar,
                            effective_scrollback,
                            persist_scrollback,
                        );
                        workspace::auto_save(session_name, &snapshot);
                        for pane in panes.values_mut() {
                            pane.kill();
                        }
                        for c in &mut clients {
                            let _ = c.outbound_tx.try_send(OutboundMsg::Exit);
                        }
                        session::cleanup(session_name);
                        ipc::cleanup();
                        return Ok(());
                    }
                }
            }
        }

        // Block until any event source wakes us, or timeout.
        // With clients: 2ms when we just rendered (active I/O), 8ms idle.
        // Headless: 20ms (responsive to PING probes for session discovery).
        let rendered_this_frame = render_needed;
        let timeout_ms = if clients.is_empty() {
            20
        } else if rendered_this_frame {
            2
        } else {
            8
        };
        let _ = wake_rx.recv_timeout(Duration::from_millis(timeout_ms));
        // Drain accumulated wake signals
        while wake_rx.try_recv().is_ok() {}
    }

    session::cleanup(session_name);
    ipc::cleanup();
    Ok(())
}
