//! Server daemon that manages PTYs, layout, and state.
//!
//! Accepts multiple clients with attach modes (steal/shared/readonly).
//! Renders frames to a buffer and streams them to all connected clients.
//! Goes headless when no client is attached.
//!
//! ## Module layout (#60)
//! - [`actions`] — command-palette / IPC layout-mutation handlers.
//! - [`connection`] — per-client connection lifecycle (accept, framing,
//!   reader thread, IPC harden + bind helper).
//! - [`input_modes`] — `InputMode` state machine + `process_event` /
//!   `process_key` (keyboard → state transitions). Companion mouse
//!   handler lives in [`mouse`].
//! - [`mouse`] — mouse-event handler, peeled out of `input_modes` to
//!   keep both files within the #60 LOC budget.
//! - [`render_glue`] — `render_frame_to_buf`, the in-server frame
//!   composer that wraps `crate::render::*` for client broadcast.
//!
//! Submodules use `pub(super)` to keep the public surface unchanged
//! (`server::run` is the only export). Crate-root helpers re-exported
//! via `main.rs` are reached via `crate::do_split`, `crate::resize_all`,
//! etc.

mod actions;
mod connection;
mod input_modes;
mod mouse;
mod render_glue;

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config;
use crate::ipc;
use crate::layout::Layout;
use crate::pane::PaneLaunch;
use crate::project;
use crate::protocol;
use crate::render::{self, BorderCache};
use crate::session;
use crate::settings::Settings;
use crate::workspace::{self, WorkspaceSnapshot};

use connection::{accept_client, bind_path_socket, effective_size, ClientMsg, ConnectedClient};
use input_modes::{process_event, DragState, InputMode, TextSelection};

pub(crate) use input_modes::TabAction;

use super::RenderUpdate;

/// Run the server daemon. This function does not return until all panes die
/// or the server is killed.
pub fn run(session_name: &str, args: &[String]) -> anyhow::Result<()> {
    // Wave 1 foundations — observability + signal handling installed before
    // any I/O so the rest of startup is captured in logs and signals never
    // race with bind.
    let _log_guard = super::observability::init(session_name);
    tracing::info!(session = session_name, "ezpn daemon starting");

    #[cfg(unix)]
    let signal_state = super::signals::install()
        .map_err(|e| anyhow::anyhow!("signal handler install failed: {e}"))?;

    let config = super::parse_args_from(args)?;

    // Load config file defaults
    let file_config = config::load_config();
    let effective_scrollback = file_config.scrollback;
    let mut default_shell = if config.has_shell_override {
        config.shell.clone()
    } else {
        file_config.shell
    };
    let effective_border = if config.has_border_override {
        config.border
    } else {
        file_config.border
    };
    let mut settings = Settings::new(effective_border);
    settings.show_status_bar = file_config.show_status_bar;
    settings.show_tab_bar = file_config.show_tab_bar;
    // Hot-reload (#64) needs prefix_key mutable + a snapshot of the on-disk
    // config bound to `settings` so non-reloadable diffs work.
    let mut prefix_key = file_config.prefix_key;
    settings.bind_runtime(config::load_config());

    // Auto-restart state
    let mut restart_policies: HashMap<usize, project::RestartPolicy> = HashMap::new();

    // Build layout and spawn panes (same logic as direct mode)
    let (mut layout, mut panes, mut active, snapshot_extra) = super::build_initial_state(
        &config,
        &mut default_shell,
        &mut settings,
        &mut restart_policies,
        effective_scrollback,
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
                let tab_panes = super::spawn_snapshot_panes(
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
                        tab_restart.insert(ps.id, ps.restart.clone());
                    }
                }
                let mut tab = Tab::new(
                    tab_snap.name.clone(),
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
            super::kill_all_panes(&mut panes);

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
    // Two bind modes (issue #65):
    //   * `Path` (default) — pathname-based Unix socket under
    //     `$XDG_RUNTIME_DIR` / `/tmp`. Hardened by `bind_path_socket`
    //     with parent-dir checks, umask 0o077 across the bind, and
    //     `chmod 0o600` re-stat after.
    //   * `Abstract` — Linux-only abstract namespace at
    //     `\0ezpn-<uid>-<session>`. No filesystem entry, so the
    //     directory checks and chmod step do not apply. On non-Linux
    //     we log a warning and fall back to `Path`.
    let sock_path = session::socket_path(session_name);
    let use_abstract = matches!(config.socket_kind, super::SocketKind::Abstract);
    let listener = if use_abstract {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let name = crate::socket_security::abstract_socket_name(session_name);
            tracing::info!(socket = "abstract", name = %name, "binding abstract namespace socket");
            let l = crate::socket_security::bind_abstract(&name)?;
            l.set_nonblocking(true)?;
            l
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            tracing::warn!("abstract namespace sockets are Linux-only; falling back to path bind");
            bind_path_socket(&sock_path)?
        }
    } else {
        bind_path_socket(&sock_path)?
    };

    let mut clients: Vec<ConnectedClient> = Vec::new();
    let mut border_cache: Option<BorderCache> = None;
    let mut render_buf: Vec<u8> = Vec::with_capacity(64 * 1024); // Reusable render buffer

    // Transient one-line message shown by the command palette on success
    // (`display-message`) or on parse/dispatch error. Cleared automatically
    // after `FLASH_TTL`; #58 specifies a 2-second window.
    let mut flash_message: Option<(String, Instant)> = None;
    const FLASH_TTL: Duration = Duration::from_secs(2);

    let mut prev_active = active;

    loop {
        // ── Signal-driven lifecycle ──
        #[cfg(unix)]
        {
            use std::sync::atomic::Ordering;

            // SIGTERM → graceful shutdown: notify clients, save snapshot, exit.
            // Bound on save attempts is left to the caller's own Drop chains;
            // explicit IPC bound is wired in a follow-up commit.
            if signal_state.sigterm.load(Ordering::Relaxed) {
                tracing::info!("SIGTERM received — graceful shutdown");
                for c in &mut clients {
                    let _ = protocol::write_msg(&mut c.writer, protocol::S_DETACHED, &[]);
                }
                for pane in panes.values_mut() {
                    pane.kill();
                }
                session::cleanup(session_name);
                ipc::cleanup();
                return Ok(());
            }

            // SIGUSR1 → dump session diagnostic JSON (one-shot per signal).
            if signal_state.sigusr1.swap(false, Ordering::Relaxed) {
                tracing::info!("SIGUSR1 received — dumping session state");
                #[derive(serde::Serialize)]
                struct DumpProbe<'a> {
                    session: &'a str,
                    pane_count: usize,
                    active_pane: usize,
                    broadcast: bool,
                    zoomed: Option<usize>,
                }
                let probe = DumpProbe {
                    session: session_name,
                    pane_count: panes.len(),
                    active_pane: active,
                    broadcast,
                    zoomed: zoomed_pane,
                };
                match super::signals::dump_session_state(&probe) {
                    Ok(p) => tracing::info!(path = %p.display(), "dumped session state"),
                    Err(e) => tracing::warn!("dump failed: {e}"),
                }
            }

            // SIGCHLD → flag reset; per-pane try_wait already runs every
            // iteration via read_output, so the explicit reap loop is a no-op
            // here. The flag exists as future-proofing for an explicit
            // waitpid(WNOHANG) sweep in a follow-up commit.
            signal_state.sigchld.swap(false, Ordering::Relaxed);

            // SIGHUP / Ctrl+B r → config reload (#64). Both triggers funnel
            // into the same atomic-apply path; the prefix-mode 'r' handler
            // sets `settings.reload_request`, SIGHUP sets the signal flag,
            // and we drain both here so a single iteration handles whichever
            // arrived first. The redraw side-effects are picked up
            // immediately below via `settings.reload_dirty`, since `update`
            // isn't constructed yet at this point in the loop.
            let sighup = signal_state.sighup.swap(false, Ordering::Relaxed);
            let prefix_r = std::mem::take(&mut settings.reload_request);
            if sighup || prefix_r {
                let trigger = if sighup { "SIGHUP" } else { "Ctrl+B r" };
                tracing::info!(target: "config_reload", trigger, "config reload requested");
                let path = crate::settings::config_path();
                match settings.reload_config(&path) {
                    crate::settings::ReloadOutcome::Reloaded {
                        non_reloadable_changed,
                    } => {
                        // Apply reloadable fields that live as separate
                        // locals (visual fields are already on `settings`).
                        prefix_key = settings.config().prefix_key;

                        if non_reloadable_changed.is_empty() {
                            settings.set_flash("config reloaded", crate::settings::FlashKind::Info);
                        } else {
                            let joined = non_reloadable_changed.join(", ");
                            settings.set_flash(
                                format!("config reloaded (restart for: {joined})"),
                                crate::settings::FlashKind::Info,
                            );
                        }
                    }
                    crate::settings::ReloadOutcome::Error(msg) => {
                        tracing::warn!(target: "config_reload", "{msg}");
                        settings.set_flash(
                            format!("config error: {msg}"),
                            crate::settings::FlashKind::Error,
                        );
                    }
                }
            }
        }

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

        // ── Pick up post-reload (#64) redraw side-effects. Set by
        // `Settings::reload_config` when border / status-bar / tab-bar
        // changed; consumed once per frame so the SIGHUP block (above)
        // doesn't have to touch render state directly.
        if std::mem::take(&mut settings.reload_dirty) {
            super::resize_all(&mut panes, &layout, tw, th, &settings);
            if let Some(zpid) = zoomed_pane {
                super::resize_zoomed_pane(&mut panes, zpid, tw, th, &settings);
            }
            update.mark_all(&layout);
            update.border_dirty = true;
            update.full_redraw = true;
        }

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
                        let _ = protocol::write_msg(&mut c.writer, protocol::S_OUTPUT, seq);
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
                            p.launch().clone(),
                            p.name().map(String::from),
                            p.initial_shell().map(String::from),
                        )
                    })
                    .unwrap_or((PaneLaunch::Shell, None, None));
                let effective_shell = pane_shell.as_deref().unwrap_or(&default_shell);
                if super::replace_pane(
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
                super::kill_all_panes(&mut panes);
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
                    super::resize_all(&mut panes, &layout, tw, th, &settings);
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
                    let _ = protocol::write_msg(&mut c.writer, protocol::S_EXIT, &[]);
                }
                break;
            }
        }

        // Unzoom if zoomed pane no longer exists
        if let Some(zpid) = zoomed_pane {
            if !panes.contains_key(&zpid) {
                zoomed_pane = None;
                super::resize_all(&mut panes, &layout, tw, th, &settings);
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
            // Defense-in-depth (issue #65): refuse cross-UID connections
            // even if a third party managed to chmod the socket open
            // between our bind and our chmod. Logged as a structured
            // audit line so operators can detect probing.
            let our_uid = unsafe { libc::getuid() };
            match crate::socket_security::peer_uid(&conn) {
                Ok(peer) if peer == our_uid => {}
                Ok(peer) => {
                    tracing::warn!(
                        event = "ipc_peer_uid_mismatch",
                        peer_uid = peer,
                        expected_uid = our_uid,
                        "refusing cross-uid connection"
                    );
                    drop(conn);
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        event = "ipc_peer_uid_error",
                        error = %e,
                        "could not read peer credentials, refusing connection"
                    );
                    drop(conn);
                    continue;
                }
            }
            // Short timeout for handshake — fail-close if setting fails
            if conn
                .set_read_timeout(Some(Duration::from_millis(100)))
                .is_err()
            {
                drop(conn);
            } else {
                match protocol::read_msg(&mut &conn) {
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
                            let _ = protocol::write_msg(&mut c.writer, protocol::S_EXIT, &[]);
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
                            &mut flash_message,
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
                let _ = protocol::write_msg(&mut c.writer, protocol::S_EXIT, &[]);
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
                super::resize_all(&mut panes, &layout, tw, th, &settings);
                if let Some(zpid) = zoomed_pane {
                    super::resize_zoomed_pane(&mut panes, zpid, tw, th, &settings);
                }
                update.mark_all(&layout);
                update.border_dirty = true;
            }
        }

        // Pre-fill rename buffer with current tab name (one-shot on transition)
        if let InputMode::RenameTab { ref mut buffer } = mode {
            // Sentinel: "\0" means "just entered, needs pre-fill"
            if buffer == "\0" {
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
                let _ = protocol::write_msg(&mut clients[pos].writer, protocol::S_DETACHED, &[]);
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
                super::resize_all(&mut panes, &layout, tw, th, &settings);
                if let Some(zpid) = zoomed_pane {
                    super::resize_zoomed_pane(&mut panes, zpid, tw, th, &settings);
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
            let snapshot = WorkspaceSnapshot::from_live(
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
                let inner = super::make_inner(tw, th, settings.show_status_bar);
                let rects = layout.pane_rects(&inner);
                let (&pid, rect) = rects.iter().next().unwrap();
                match super::spawn_pane(
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
                            super::resize_all(&mut panes, &layout, tw, th, &settings);
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
                        super::resize_all(&mut panes, &layout, tw, th, &settings);
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
                    super::kill_all_panes(&mut panes);
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
                        super::resize_all(&mut panes, &layout, tw, th, &settings);
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
                super::kill_all_panes(&mut panes);
                tab_mgr.kill_all_inactive();
                for c in &mut clients {
                    let _ = protocol::write_msg(&mut c.writer, protocol::S_EXIT, &[]);
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
                    let snapshot = WorkspaceSnapshot::from_live(
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
                        super::kill_all_panes(&mut panes);
                        tab_mgr.kill_all_inactive();

                        default_shell = snapshot.shell.clone();
                        settings = Settings::new(snapshot.border_style);
                        settings.show_status_bar = snapshot.show_status_bar;
                        settings.show_tab_bar = snapshot.show_tab_bar;

                        // Spawn all tabs in order
                        let mut all_tabs: Vec<Tab> = Vec::new();
                        for tab_snap in &snapshot.tabs {
                            let tp = super::spawn_snapshot_panes(
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
                                    tr.insert(ps.id, ps.restart.clone());
                                }
                            }
                            let mut tab = Tab::new(
                                tab_snap.name.clone(),
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

                let (response, mut ipc_update) = super::handle_ipc_command(
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
            super::resize_zoomed_pane(&mut panes, active, tw, th, &settings);
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
                let render_targets = super::collect_render_targets(
                    &panes,
                    &update.dirty_panes,
                    update.full_redraw,
                    zoomed_pane,
                    needs_selection_chars
                        .then(|| text_selection.as_ref().map(|s| s.pane_id))
                        .flatten(),
                );
                super::sync_render_targets(&mut panes, &render_targets);
                let selection_chars = if needs_selection_chars {
                    super::selection_char_count_from_synced(&panes, sel_for_render)
                } else {
                    0
                };

                // Expire stale flash messages before render so the status bar
                // self-clears even when the user isn't typing (#58).
                if let Some((_, t)) = &flash_message {
                    if t.elapsed() >= FLASH_TTL {
                        flash_message = None;
                        update.full_redraw = true;
                    }
                }
                let flash_text: Option<&str> = flash_message.as_ref().map(|(s, _)| s.as_str());

                // Render once (smallest-client policy: all clients see the same frame)
                render_buf.clear();
                let render_result = render_glue::render_frame_to_buf(
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
                    flash_text,
                );

                super::reset_render_targets(&mut panes, &render_targets);

                // Broadcast frame to all clients; remove failed ones
                if render_result.is_ok() && !render_buf.is_empty() {
                    clients.retain_mut(|c| {
                        if protocol::write_msg(&mut c.writer, protocol::S_OUTPUT, &render_buf)
                            .is_err()
                        {
                            // Try to send detach ack before dropping
                            let _ = protocol::write_msg(&mut c.writer, protocol::S_DETACHED, &[]);
                            false
                        } else {
                            true
                        }
                    });
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
