//! Server daemon that manages PTYs, layout, and state.
//!
//! Accepts multiple clients with attach modes (steal/shared/readonly).
//! Renders frames to a buffer and streams them to all connected clients.
//! Goes headless when no client is attached.

use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::{cursor, queue, terminal};

use crate::config;
use crate::ipc;
use crate::layout::{Direction, Layout, NavDir, Rect, SepHit};
use crate::pane::{Pane, PaneLaunch};
use crate::project;
use crate::protocol;
use crate::render::{self, BorderCache};
use crate::session;
use crate::settings::{Settings, SettingsAction};
use crate::signals::{Signal as Sig, SignalHandlers};
use crate::workspace::{self, WorkspaceSnapshot};

/// Input state machine for prefix key support.
#[allow(dead_code)]
enum InputMode {
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
struct TextSelection {
    pane_id: usize,
    start_row: u16,
    start_col: u16,
    end_row: u16,
    end_col: u16,
}

impl TextSelection {
    fn normalized(&self) -> (u16, u16, u16, u16) {
        if self.start_row < self.end_row
            || (self.start_row == self.end_row && self.start_col <= self.end_col)
        {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
        }
    }
}

struct DragState {
    path: Vec<bool>,
    direction: Direction,
    area: Rect,
}

impl DragState {
    fn from_hit(hit: SepHit) -> Self {
        Self {
            path: hit.path,
            direction: hit.direction,
            area: hit.area,
        }
    }

    fn calc_ratio(&self, mx: u16, my: u16) -> f32 {
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

use super::RenderUpdate;

/// Client message from the reader thread.
enum ClientMsg {
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
struct ConnectedClient {
    id: u64,
    writer: std::io::BufWriter<UnixStream>,
    event_rx: mpsc::Receiver<ClientMsg>,
    mode: protocol::AttachMode,
    /// Capability bits negotiated during the C_HELLO handshake. Zero for
    /// legacy clients that connected without a Hello — those are treated
    /// as having no extended capabilities.
    #[allow(dead_code)]
    caps: u32,
    tw: u16,
    th: u16,
}

impl Drop for ConnectedClient {
    fn drop(&mut self) {
        // Shutdown the underlying socket to force the reader thread to exit.
        let _ = self.writer.get_ref().shutdown(std::net::Shutdown::Both);
    }
}

/// Compute the effective terminal size from all active clients.
/// Uses smallest-client policy (like tmux).
fn effective_size(clients: &[ConnectedClient]) -> (u16, u16) {
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

/// Run the server daemon. This function does not return until all panes die
/// or the server is killed.
pub fn run(session_name: &str, args: &[String]) -> anyhow::Result<()> {
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

    // Create session socket and listen for client connections
    let sock_path = session::socket_path(session_name);
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;
    listener.set_nonblocking(true)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
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

/// Convert a `catch_unwind` payload into a printable reason string.
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "unknown panic payload".to_string()
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
fn accept_client(
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
            let _ = protocol::write_msg(&mut c.writer, protocol::S_DETACHED, &[]);
        }
        clients.clear();
    }

    // Set up the new client
    if let Ok(read_conn) = conn.try_clone() {
        conn.set_read_timeout(None).ok();
        let (msg_tx, msg_rx) = mpsc::channel();
        let panic_tx = msg_tx.clone();
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
        let client_id = NEXT_CLIENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        clients.push(ConnectedClient {
            id: client_id,
            writer: std::io::BufWriter::new(conn),
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
        super::resize_all(panes, layout, *tw, *th, settings);
        if let Some(zpid) = zoomed_pane {
            super::resize_zoomed_pane(panes, zpid, *tw, *th, settings);
        }
    }

    // Force full redraw for new client
    update.mark_all(layout);
    update.border_dirty = true;
}

/// Render a full frame to a byte buffer (instead of stdout).
#[allow(clippy::too_many_arguments)]
fn render_frame_to_buf(
    buf: &mut Vec<u8>,
    panes: &HashMap<usize, Pane>,
    layout: &Layout,
    active: usize,
    settings: &Settings,
    tw: u16,
    th: u16,
    dragging: bool,
    border_cache: &BorderCache,
    dirty_panes: &HashSet<usize>,
    full_redraw: bool,
    mode: &InputMode,
    broadcast: bool,
    selection: render::PaneSelection,
    selection_chars: usize,
    zoomed_pane: Option<usize>,
    default_shell: &str,
    tab_names: &[(usize, String, bool)],
) -> anyhow::Result<()> {
    let mode_label = match mode {
        InputMode::Prefix { .. } => "PREFIX",
        InputMode::CopyMode(ref cm) => cm.mode_label(),
        InputMode::QuitConfirm => "KILL SESSION? y/n",
        InputMode::CloseConfirm => "CLOSE PANE? y/n",
        InputMode::CloseTabConfirm => "CLOSE TAB? y/n",
        InputMode::ResizeMode => "RESIZE",
        InputMode::PaneSelect => "SELECT",
        InputMode::HelpOverlay => "",
        InputMode::RenameTab { .. } => "RENAME",
        InputMode::CommandPalette { .. } => ":",
        InputMode::Normal if broadcast => "BROADCAST",
        InputMode::Normal => "",
    };

    if let Some(zpid) = zoomed_pane {
        queue!(buf, terminal::BeginSynchronizedUpdate)?;
        let pane_order = border_cache.pane_order();
        let pane_idx = pane_order.iter().position(|&id| id == zpid).unwrap_or(0);
        let label = panes
            .get(&zpid)
            .map(|p| p.launch_label(default_shell))
            .unwrap_or_default();
        if let Some(pane) = panes.get(&zpid) {
            render::render_zoomed_pane(
                buf,
                pane,
                pane_idx,
                &label,
                settings.border_style,
                tw,
                th,
                settings.show_status_bar,
            )?;
        }
        if settings.show_status_bar {
            let zoom_label = if mode_label.is_empty() {
                "ZOOM"
            } else {
                mode_label
            };
            let pane_name = panes.get(&zpid).and_then(|p| p.name()).unwrap_or("");
            render::draw_status_bar_full(
                buf,
                tw,
                th,
                pane_idx,
                pane_order.len(),
                zoom_label,
                pane_name,
                0,
            )?;
        }
        queue!(buf, terminal::EndSynchronizedUpdate)?;
    } else {
        queue!(buf, terminal::BeginSynchronizedUpdate)?;
        render::render_panes(
            buf,
            panes,
            layout,
            active,
            settings.border_style,
            settings.show_status_bar,
            tw,
            th,
            dragging,
            border_cache,
            dirty_panes,
            full_redraw,
            selection,
            broadcast,
        )?;
        let is_text_input = matches!(
            mode,
            InputMode::RenameTab { .. } | InputMode::CommandPalette { .. }
        );
        // Status bar (skip if text input mode will draw over it)
        if !is_text_input
            && settings.show_status_bar
            && (!mode_label.is_empty() || selection_chars > 0)
        {
            let pane_order = border_cache.pane_order();
            let active_idx = pane_order.iter().position(|&id| id == active).unwrap_or(0);
            let pane_name = panes.get(&active).and_then(|p| p.name()).unwrap_or("");
            render::draw_status_bar_full(
                buf,
                tw,
                th,
                active_idx,
                pane_order.len(),
                mode_label,
                pane_name,
                selection_chars,
            )?;
        }
        if settings.visible {
            settings.render_overlay(buf, tw, th, broadcast)?;
            queue!(buf, cursor::Hide)?;
        }
        queue!(buf, terminal::EndSynchronizedUpdate)?;
    }

    // Tab bar (only when multiple tabs exist and show_tab_bar is enabled)
    if tab_names.len() > 1 && settings.show_tab_bar {
        render::draw_tab_bar(buf, tw, th, tab_names, settings.show_status_bar)?;
    }

    // Overlays
    if matches!(mode, InputMode::HelpOverlay) {
        render::draw_help_overlay(buf, tw, th)?;
    }
    if matches!(mode, InputMode::PaneSelect) {
        let inner = super::make_inner(tw, th, settings.show_status_bar);
        render::draw_pane_numbers(buf, layout, &inner)?;
    }

    // Text input overlay — drawn LAST so it's on top of status bar
    match mode {
        InputMode::RenameTab { buffer } => {
            render::draw_text_input(buf, tw, th, "Rename tab: ", buffer)?;
        }
        InputMode::CommandPalette { buffer } => {
            render::draw_text_input(buf, tw, th, ":", buffer)?;
        }
        _ => {}
    }

    // Ensure cursor is hidden at the end — prevents blinking on status/tab bar
    queue!(buf, cursor::Hide)?;

    Ok(())
}

/// Process a single crossterm Event (shared between direct and server modes).
#[allow(clippy::too_many_arguments, unused_variables)]
fn process_event(
    event: Event,
    mode: &mut InputMode,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    settings: &mut Settings,
    update: &mut RenderUpdate,
    drag: &mut Option<DragState>,
    zoomed_pane: &mut Option<usize>,
    last_click: &mut Option<(Instant, u16, u16)>,
    broadcast: &mut bool,
    last_active: &mut usize,
    selection_anchor: &mut Option<(usize, u16, u16)>,
    text_selection: &mut Option<TextSelection>,
    default_shell: &str,
    tw: u16,
    th: u16,
    scrollback: usize,
    border_cache: &Option<BorderCache>,
    detach_requested: &mut bool,
    tab_action: &mut TabAction,
    tab_names: &[(usize, String, bool)],
    prefix_key: char,
) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            process_key(
                key,
                mode,
                layout,
                panes,
                active,
                settings,
                update,
                zoomed_pane,
                broadcast,
                last_active,
                default_shell,
                tw,
                th,
                scrollback,
                border_cache,
                detach_requested,
                tab_action,
                prefix_key,
            );
        }
        Event::Mouse(mouse) => {
            if let Some(ref cache) = border_cache {
                let inner = cache.inner().clone();
                process_mouse(
                    mouse,
                    mode,
                    layout,
                    panes,
                    active,
                    settings,
                    update,
                    drag,
                    zoomed_pane,
                    last_click,
                    broadcast,
                    selection_anchor,
                    text_selection,
                    default_shell,
                    tw,
                    th,
                    scrollback,
                    cache,
                    &inner,
                    tab_action,
                    tab_names,
                );
            }
        }
        Event::Resize(w, h) => {
            // Handled separately via C_RESIZE message
            let _ = (w, h);
        }
        Event::FocusGained => {
            // Forward focus to active pane (only if it requested focus events)
            if let Some(pane) = panes.get_mut(active) {
                if pane.is_alive() && pane.wants_focus() {
                    pane.write_bytes(b"\x1b[I");
                }
            }
        }
        Event::FocusLost => {
            if let Some(pane) = panes.get_mut(active) {
                if pane.is_alive() && pane.wants_focus() {
                    pane.write_bytes(b"\x1b[O");
                }
            }
        }
        Event::Paste(text) => {
            // Forward paste to active pane, with bracketed paste wrapping if enabled
            if let Some(pane) = panes.get_mut(active) {
                if pane.is_alive() {
                    if pane.bracketed_paste() {
                        pane.write_bytes(b"\x1b[200~");
                        pane.write_bytes(text.as_bytes());
                        pane.write_bytes(b"\x1b[201~");
                    } else {
                        pane.write_bytes(text.as_bytes());
                    }
                }
            }
        }
        _ => {}
    }
}

/// Process a key event. This is the core input handler shared between modes.
#[allow(clippy::too_many_arguments, unused_variables)]
fn process_key(
    key: KeyEvent,
    mode: &mut InputMode,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    settings: &mut Settings,
    update: &mut RenderUpdate,
    zoomed_pane: &mut Option<usize>,
    broadcast: &mut bool,
    last_active: &mut usize,
    default_shell: &str,
    tw: u16,
    th: u16,
    scrollback: usize,
    border_cache: &Option<BorderCache>,
    detach_requested: &mut bool,
    tab_action: &mut TabAction,
    prefix_key: char,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // ── Quit confirmation ──
    if matches!(mode, InputMode::QuitConfirm) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                // Kill entire session (all tabs)
                *tab_action = TabAction::KillSession;
            }
            _ => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
        }
        return;
    }

    // ── Close tab (window) confirmation ──
    if matches!(mode, InputMode::CloseTabConfirm) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                *tab_action = TabAction::CloseTab;
                *mode = InputMode::Normal;
            }
            _ => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
        }
        return;
    }

    // ── Close pane confirmation ──
    if matches!(mode, InputMode::CloseConfirm) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let target = *active;
                super::close_pane(layout, panes, active, target);
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
                *mode = InputMode::Normal;
            }
            _ => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
        }
        return;
    }

    // ── Help overlay ──
    if matches!(mode, InputMode::HelpOverlay) {
        *mode = InputMode::Normal;
        update.full_redraw = true;
        return;
    }

    // ── Pane select ──
    if matches!(mode, InputMode::PaneSelect) {
        let ids = layout.pane_ids();
        if let KeyCode::Char(c @ '0'..='9') = key.code {
            let idx = match c {
                '1'..='9' => c as usize - '1' as usize,
                '0' => 9,
                _ => unreachable!(),
            };
            if let Some(&target) = ids.get(idx) {
                if panes.contains_key(&target) {
                    *active = target;
                }
            }
        }
        *mode = InputMode::Normal;
        update.full_redraw = true;
        return;
    }

    // ── Rename tab mode ──
    if let InputMode::RenameTab { buffer } = mode {
        match key.code {
            KeyCode::Char(c) if !ctrl => {
                buffer.push(c);
                update.full_redraw = true;
            }
            KeyCode::Backspace => {
                buffer.pop();
                update.full_redraw = true;
            }
            KeyCode::Enter => {
                if !buffer.is_empty() {
                    *tab_action = TabAction::Rename(std::mem::take(buffer));
                }
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
            KeyCode::Esc => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
            _ => {}
        }
        return;
    }

    // ── Command palette mode ──
    if let InputMode::CommandPalette { buffer } = mode {
        match key.code {
            KeyCode::Char(c) if !ctrl => {
                buffer.push(c);
                update.full_redraw = true;
            }
            KeyCode::Backspace => {
                buffer.pop();
                update.full_redraw = true;
            }
            KeyCode::Enter => {
                let cmd = std::mem::take(buffer);
                *mode = InputMode::Normal;
                // Parse and execute command
                execute_command(
                    &cmd,
                    layout,
                    panes,
                    active,
                    settings,
                    update,
                    default_shell,
                    tw,
                    th,
                    scrollback,
                    zoomed_pane,
                    broadcast,
                    tab_action,
                );
            }
            KeyCode::Esc => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
            _ => {}
        }
        return;
    }

    // ── Resize mode ──
    if matches!(mode, InputMode::ResizeMode) {
        match key.code {
            KeyCode::Left | KeyCode::Char('h')
                if layout.resize_pane(*active, NavDir::Left, 0.05) =>
            {
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Right | KeyCode::Char('l')
                if layout.resize_pane(*active, NavDir::Right, 0.05) =>
            {
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') if layout.resize_pane(*active, NavDir::Up, 0.05) => {
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j')
                if layout.resize_pane(*active, NavDir::Down, 0.05) =>
            {
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
            _ => {}
        }
        return;
    }

    // ── Copy mode (vi keys, selection, search) ──
    if let InputMode::CopyMode(ref mut cm_state) = mode {
        if let Some(pane) = panes.get_mut(active) {
            // Handle scrolling first (before screen access)
            match key.code {
                KeyCode::Char('k') | KeyCode::Up if cm_state.cursor_row == 0 => {
                    pane.scroll_up(1);
                }
                KeyCode::Char('j') | KeyCode::Down
                    if cm_state.cursor_row >= cm_state.pane_rows.saturating_sub(1) =>
                {
                    pane.scroll_down(1);
                }
                KeyCode::Char('g') if !ctrl => {
                    pane.scroll_up(usize::MAX);
                }
                KeyCode::Char('G') => {
                    pane.snap_to_bottom();
                }
                KeyCode::Char('u') if ctrl => {
                    pane.scroll_up((cm_state.pane_rows / 2) as usize);
                }
                KeyCode::Char('d') if ctrl => {
                    pane.scroll_down((cm_state.pane_rows / 2) as usize);
                }
                KeyCode::PageUp => {
                    pane.scroll_up(cm_state.pane_rows as usize);
                }
                KeyCode::PageDown => {
                    pane.scroll_down(cm_state.pane_rows as usize);
                }
                _ => {}
            }

            // Process key through copy mode state machine
            pane.sync_scrollback();
            let action = crate::copy_mode::handle_key(
                key,
                cm_state,
                pane.screen(),
                &mut |_| {}, // scrolling handled above
                &mut |_| {},
            );
            pane.reset_scrollback_view();

            match action {
                crate::copy_mode::CopyAction::CopyAndExit(text) => {
                    // OSC 52 clipboard copy
                    let encoded = super::base64_encode(text.as_bytes());
                    let osc = format!("\x1b]52;c;{}\x07", encoded);
                    pane.osc52_pending.push(osc.into_bytes());
                    pane.snap_to_bottom();
                    *mode = InputMode::Normal;
                }
                crate::copy_mode::CopyAction::Exit => {
                    pane.snap_to_bottom();
                    *mode = InputMode::Normal;
                }
                _ => {}
            }
            update.dirty_panes.insert(*active);
        }
        return;
    }

    // ── Prefix mode ──
    if matches!(mode, InputMode::Prefix { .. }) {
        update.full_redraw = true;
        let mut next_mode = InputMode::Normal;
        match key.code {
            // Split
            KeyCode::Char('%') => {
                let _ = super::do_split(
                    layout,
                    panes,
                    *active,
                    Direction::Horizontal,
                    default_shell,
                    tw,
                    th,
                    settings,
                    scrollback,
                );
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Char('"') => {
                let _ = super::do_split(
                    layout,
                    panes,
                    *active,
                    Direction::Vertical,
                    default_shell,
                    tw,
                    th,
                    settings,
                    scrollback,
                );
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // Navigate
            KeyCode::Char('o') => {
                *active = layout.next_pane(*active);
            }
            KeyCode::Left => {
                let i = super::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Left, &i) {
                    *active = n;
                }
            }
            KeyCode::Right => {
                let i = super::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Right, &i) {
                    *active = n;
                }
            }
            KeyCode::Up => {
                let i = super::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Up, &i) {
                    *active = n;
                }
            }
            KeyCode::Down => {
                let i = super::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Down, &i) {
                    *active = n;
                }
            }
            // Close pane (with confirmation, tmux-style)
            KeyCode::Char('x') => {
                next_mode = InputMode::CloseConfirm;
            }
            // Equalize
            KeyCode::Char('E') => {
                layout.equalize();
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // Scroll mode
            KeyCode::Char('[') => {
                // Enter copy mode — need pane dimensions
                if let Some(pane) = panes.get(active) {
                    let screen = pane.screen();
                    let (rows, cols) = screen.size();
                    next_mode =
                        InputMode::CopyMode(crate::copy_mode::CopyModeState::new(rows, cols));
                }
            }
            // Detach (tmux d)
            KeyCode::Char('d') => {
                *detach_requested = true;
            }
            // Toggle status bar
            KeyCode::Char('s') => {
                settings.show_status_bar = !settings.show_status_bar;
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // Zoom toggle
            KeyCode::Char('z') => {
                if zoomed_pane.is_some() {
                    *zoomed_pane = None;
                    super::resize_all(panes, layout, tw, th, settings);
                    update.mark_all(layout);
                    update.border_dirty = true;
                } else {
                    *zoomed_pane = Some(*active);
                    super::resize_zoomed_pane(panes, *active, tw, th, settings);
                }
            }
            // Resize mode
            KeyCode::Char('R') => {
                next_mode = InputMode::ResizeMode;
            }
            // Pane select
            KeyCode::Char('q') => {
                next_mode = InputMode::PaneSelect;
            }
            // Help
            KeyCode::Char('?') => {
                next_mode = InputMode::HelpOverlay;
            }
            // Swap
            KeyCode::Char('{') => {
                let prev = layout.prev_pane(*active);
                if prev != *active {
                    layout.swap_panes(*active, prev);
                    update.mark_all(layout);
                    update.border_dirty = true;
                }
            }
            KeyCode::Char('}') => {
                let next = layout.next_pane(*active);
                if next != *active {
                    layout.swap_panes(*active, next);
                    update.mark_all(layout);
                    update.border_dirty = true;
                }
            }
            // Broadcast toggle
            KeyCode::Char('B') => {
                *broadcast = !*broadcast;
                update.full_redraw = true;
            }
            // Last pane
            KeyCode::Char(';') if panes.contains_key(last_active) => {
                *active = *last_active;
                update.full_redraw = true;
            }
            // Equalize (space)
            KeyCode::Char(' ') => {
                layout.equalize();
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // New tab (tmux c = new window)
            KeyCode::Char('c') => {
                *tab_action = TabAction::NewTab;
            }
            // Next tab (tmux n)
            KeyCode::Char('n') => {
                *tab_action = TabAction::NextTab;
            }
            // Previous tab (tmux p)
            KeyCode::Char('p') => {
                *tab_action = TabAction::PrevTab;
            }
            // Close tab (with confirmation)
            KeyCode::Char('&') => {
                next_mode = InputMode::CloseTabConfirm;
            }
            // Rename tab (tmux ,) — pre-fill with current tab name
            KeyCode::Char(',') => {
                // tab_name is not accessible here directly, use empty for now
                // The actual pre-fill happens in the render where the prompt shows current name
                next_mode = InputMode::RenameTab {
                    buffer: "\0".to_string(), // sentinel: will be pre-filled by main loop
                };
            }
            // Command palette (tmux :)
            KeyCode::Char(':') => {
                next_mode = InputMode::CommandPalette {
                    buffer: String::new(),
                };
            }
            // Tab jump by number (tmux 0-9 for windows)
            KeyCode::Char(digit @ '0'..='9') => {
                let idx = if digit == '0' {
                    9
                } else {
                    (digit as usize) - ('1' as usize)
                };
                *tab_action = TabAction::GoToTab(idx);
            }
            _ => {}
        }
        *mode = next_mode;
        return;
    }

    // ── Normal mode ──
    if key.code == KeyCode::Char(prefix_key) && ctrl {
        *mode = InputMode::Prefix {
            entered_at: Instant::now(),
        };
        update.full_redraw = true;
    } else if (key.code == KeyCode::Char('g') && ctrl) || key.code == KeyCode::F(1) {
        settings.toggle();
        update.full_redraw = true;
    } else if ctrl
        && (key.code == KeyCode::Char('\\')
            || key.code == KeyCode::Char('q')
            || key.code == KeyCode::Char('w'))
    {
        // Confirm before killing session
        *mode = InputMode::QuitConfirm;
        update.full_redraw = true;
    } else if settings.visible {
        let prev_border = settings.border_style;
        let prev_status = settings.show_status_bar;
        let prev_tab_bar = settings.show_tab_bar;
        let action = settings.handle_key(key);
        if action == SettingsAction::BroadcastToggle {
            *broadcast = !*broadcast;
        }
        if settings.border_style != prev_border {
            update.full_redraw = true;
        }
        if settings.show_status_bar != prev_status || settings.show_tab_bar != prev_tab_bar {
            super::resize_all(panes, layout, tw, th, settings);
            update.border_dirty = true;
            update.mark_all(layout);
        }
        update.full_redraw = true;
    } else if key.code == KeyCode::Char('d') && ctrl {
        let _ = super::do_split(
            layout,
            panes,
            *active,
            Direction::Horizontal,
            default_shell,
            tw,
            th,
            settings,
            scrollback,
        );
        update.mark_all(layout);
        update.border_dirty = true;
    } else if key.code == KeyCode::Char('e') && ctrl {
        let _ = super::do_split(
            layout,
            panes,
            *active,
            Direction::Vertical,
            default_shell,
            tw,
            th,
            settings,
            scrollback,
        );
        update.mark_all(layout);
        update.border_dirty = true;
    } else if ctrl && (key.code == KeyCode::Char(']') || key.code == KeyCode::Char('n')) {
        *active = layout.next_pane(*active);
        update.full_redraw = true;
    } else if key.code == KeyCode::F(2) {
        layout.equalize();
        super::resize_all(panes, layout, tw, th, settings);
        update.mark_all(layout);
        update.border_dirty = true;
    } else if alt {
        let inner = super::make_inner(tw, th, settings.show_status_bar);
        let nav = match key.code {
            KeyCode::Left => Some(NavDir::Left),
            KeyCode::Right => Some(NavDir::Right),
            KeyCode::Up => Some(NavDir::Up),
            KeyCode::Down => Some(NavDir::Down),
            _ => None,
        };
        if let Some(dir) = nav {
            if let Some(next) = layout.navigate(*active, dir, &inner) {
                *active = next;
                update.full_redraw = true;
            }
        } else if *broadcast {
            for pane in panes.values_mut() {
                if pane.is_alive() {
                    pane.write_key(key);
                }
            }
        } else if let Some(pane) = panes.get_mut(active) {
            if pane.is_alive() {
                pane.write_key(key);
            }
        }
    } else if key.code == KeyCode::Enter && panes.get(active).is_some_and(|p| !p.is_alive()) {
        let (launch, old_name, pane_shell) = panes
            .get(active)
            .map(|p| {
                (
                    p.launch().clone(),
                    p.name().map(String::from),
                    p.initial_shell().map(String::from),
                )
            })
            .unwrap_or((PaneLaunch::Shell, None, None));
        let eff_shell = pane_shell.as_deref().unwrap_or(default_shell);
        if super::replace_pane(
            panes, layout, *active, launch, eff_shell, tw, th, settings, scrollback,
        )
        .is_ok()
        {
            if let Some(pane) = panes.get_mut(active) {
                pane.set_name(old_name);
                if let Some(ref s) = pane_shell {
                    pane.set_initial_shell(Some(s.clone()));
                }
            }
        }
        update.dirty_panes.insert(*active);
    } else if *broadcast {
        for pane in panes.values_mut() {
            if pane.is_alive() {
                pane.write_key(key);
            }
        }
    } else if let Some(pane) = panes.get_mut(active) {
        if pane.is_alive() {
            pane.write_key(key);
        }
    }
}

/// Execute a command from the command palette.
#[allow(clippy::too_many_arguments)]
fn execute_command(
    cmd: &str,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    settings: &mut Settings,
    update: &mut super::RenderUpdate,
    default_shell: &str,
    tw: u16,
    th: u16,
    scrollback: usize,
    zoomed_pane: &mut Option<usize>,
    broadcast: &mut bool,
    tab_action: &mut TabAction,
) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    match parts.first().copied() {
        Some("split-window") | Some("split") => {
            let dir = if parts.get(1) == Some(&"-v") || parts.get(1) == Some(&"v") {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            let _ = super::do_split(
                layout,
                panes,
                *active,
                dir,
                default_shell,
                tw,
                th,
                settings,
                scrollback,
            );
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Some("new-window") | Some("new-tab") => {
            *tab_action = TabAction::NewTab;
        }
        Some("next-window") | Some("next-tab") => {
            *tab_action = TabAction::NextTab;
        }
        Some("prev-window") | Some("prev-tab") | Some("previous-window") => {
            *tab_action = TabAction::PrevTab;
        }
        Some("kill-pane") | Some("close-pane") => {
            let target = *active;
            super::close_pane(layout, panes, active, target);
            super::resize_all(panes, layout, tw, th, settings);
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Some("kill-window") | Some("close-tab") => {
            *tab_action = TabAction::CloseTab;
        }
        Some("rename-window") | Some("rename-tab") => {
            if let Some(name) = parts.get(1..).map(|s| s.join(" ")) {
                if !name.is_empty() {
                    *tab_action = TabAction::Rename(name);
                }
            }
        }
        Some("select-layout") | Some("layout") => {
            if let Some(spec) = parts.get(1) {
                if let Ok(new_layout) = Layout::from_spec(spec) {
                    if let Ok(new_panes) = super::spawn_layout_panes(
                        &new_layout,
                        HashMap::new(),
                        default_shell,
                        tw,
                        th,
                        settings,
                        scrollback,
                    ) {
                        super::kill_all_panes(panes);
                        *layout = new_layout;
                        *panes = new_panes;
                        *active = *layout.pane_ids().first().unwrap_or(&0);
                        update.mark_all(layout);
                        update.border_dirty = true;
                    }
                }
            }
        }
        Some("equalize") | Some("even") => {
            layout.equalize();
            super::resize_all(panes, layout, tw, th, settings);
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Some("zoom") => {
            if zoomed_pane.is_some() {
                *zoomed_pane = None;
                super::resize_all(panes, layout, tw, th, settings);
            } else {
                *zoomed_pane = Some(*active);
                super::resize_zoomed_pane(panes, *active, tw, th, settings);
            }
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Some("broadcast") => {
            *broadcast = !*broadcast;
            update.full_redraw = true;
        }
        _ => {
            // Unknown command — silently ignore
        }
    }
    update.full_redraw = true;
}

/// Process a mouse event.
#[allow(clippy::too_many_arguments, unused_variables)]
fn process_mouse(
    mouse: crossterm::event::MouseEvent,
    _mode: &mut InputMode,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    settings: &mut Settings,
    update: &mut RenderUpdate,
    drag: &mut Option<DragState>,
    zoomed_pane: &mut Option<usize>,
    last_click: &mut Option<(Instant, u16, u16)>,
    broadcast: &mut bool,
    selection_anchor: &mut Option<(usize, u16, u16)>,
    text_selection: &mut Option<TextSelection>,
    default_shell: &str,
    tw: u16,
    th: u16,
    scrollback: usize,
    border_cache: &BorderCache,
    inner: &Rect,
    tab_action: &mut TabAction,
    tab_names: &[(usize, String, bool)],
) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Tab bar: single click = switch tab, double click = rename tab
            if tab_names.len() > 1 {
                let tab_y = render::tab_bar_y(th, settings.show_status_bar);
                if mouse.row == tab_y {
                    if let Some(idx) = render::tab_bar_hit(mouse.column, tab_names, tw) {
                        let now = Instant::now();
                        let is_double = last_click
                            .map(|(t, lx, ly)| {
                                now.duration_since(t) < Duration::from_millis(400)
                                    && lx == mouse.column
                                    && ly == mouse.row
                            })
                            .unwrap_or(false);
                        *last_click = Some((now, mouse.column, mouse.row));

                        if is_double {
                            // Double-click on tab → rename mode
                            // First switch to that tab if not active
                            if idx != tab_names.iter().position(|(_, _, a)| *a).unwrap_or(0) {
                                *tab_action = TabAction::GoToTab(idx);
                            }
                            // Enter rename mode — sentinel will be pre-filled by main loop
                            *_mode = InputMode::RenameTab {
                                buffer: "\0".to_string(),
                            };
                            update.full_redraw = true;
                        } else {
                            *tab_action = TabAction::GoToTab(idx);
                        }
                        return;
                    }
                }
            }

            if settings.visible {
                let prev_border = settings.border_style;
                let prev_status = settings.show_status_bar;
                let prev_tab_bar = settings.show_tab_bar;
                let action = settings.handle_click(mouse.column, mouse.row, tw, th);
                if action == SettingsAction::BroadcastToggle {
                    *broadcast = !*broadcast;
                }
                if settings.border_style != prev_border {
                    update.full_redraw = true;
                }
                if settings.show_status_bar != prev_status || settings.show_tab_bar != prev_tab_bar
                {
                    super::resize_all(panes, layout, tw, th, settings);
                    update.border_dirty = true;
                    update.mark_all(layout);
                }
                if action == SettingsAction::Changed
                    || action == SettingsAction::Close
                    || action == SettingsAction::BroadcastToggle
                {
                    update.full_redraw = true;
                }
            } else if let Some(action) =
                render::title_button_hit(mouse.column, mouse.row, layout, inner)
            {
                match action {
                    render::TitleAction::Close(pid) => {
                        super::close_pane(layout, panes, active, pid);
                        super::resize_all(panes, layout, tw, th, settings);
                    }
                    render::TitleAction::SplitH(pid) => {
                        let _ = super::do_split(
                            layout,
                            panes,
                            pid,
                            Direction::Vertical,
                            default_shell,
                            tw,
                            th,
                            settings,
                            scrollback,
                        );
                    }
                    render::TitleAction::SplitV(pid) => {
                        let _ = super::do_split(
                            layout,
                            panes,
                            pid,
                            Direction::Horizontal,
                            default_shell,
                            tw,
                            th,
                            settings,
                            scrollback,
                        );
                    }
                }
                update.mark_all(layout);
                update.border_dirty = true;
            } else if let Some(hit) = layout.find_separator_at(mouse.column, mouse.row, inner) {
                *drag = Some(DragState::from_hit(hit));
                update.full_redraw = true;
            } else if let Some(pid) = layout.find_at(mouse.column, mouse.row, inner) {
                let now = Instant::now();
                let is_double = last_click
                    .map(|(t, lx, ly)| {
                        now.duration_since(t) < Duration::from_millis(400)
                            && lx == mouse.column
                            && ly == mouse.row
                    })
                    .unwrap_or(false);
                *last_click = Some((now, mouse.column, mouse.row));

                if is_double && panes.contains_key(&pid) {
                    if zoomed_pane.is_some() {
                        *zoomed_pane = None;
                        super::resize_all(panes, layout, tw, th, settings);
                    } else {
                        *zoomed_pane = Some(pid);
                        super::resize_zoomed_pane(panes, pid, tw, th, settings);
                    }
                    *active = pid;
                    update.mark_all(layout);
                    update.border_dirty = true;
                } else if pid != *active && panes.contains_key(&pid) {
                    *active = pid;
                    update.full_redraw = true;
                }
                if !is_double {
                    if let Some(pane) = panes.get_mut(&pid) {
                        if pane.wants_mouse() {
                            if let Some(rect) = border_cache.pane_rects().get(&pid) {
                                let rel_col = mouse.column.saturating_sub(rect.x);
                                let rel_row = mouse.row.saturating_sub(rect.y);
                                pane.send_mouse_event(0, rel_col, rel_row, false);
                            }
                        } else if pid == *active {
                            if let Some(rect) = border_cache.pane_rects().get(&pid) {
                                let rel_col = mouse.column.saturating_sub(rect.x);
                                let rel_row = mouse.row.saturating_sub(rect.y);
                                *selection_anchor = Some((pid, rel_col, rel_row));
                                if text_selection.is_some() {
                                    *text_selection = None;
                                    update.dirty_panes.insert(pid);
                                }
                            }
                        }
                    }
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(ref ds) = drag {
                let new_ratio = ds.calc_ratio(mouse.column, mouse.row);
                layout.set_ratio_at_path(&ds.path, new_ratio);
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            } else if let Some((pid, anchor_col, anchor_row)) = *selection_anchor {
                if let Some(rect) = border_cache.pane_rects().get(&pid) {
                    let rel_col = mouse
                        .column
                        .saturating_sub(rect.x)
                        .min(rect.w.saturating_sub(1));
                    let rel_row = mouse
                        .row
                        .saturating_sub(rect.y)
                        .min(rect.h.saturating_sub(1));
                    *text_selection = Some(TextSelection {
                        pane_id: pid,
                        start_row: anchor_row,
                        start_col: anchor_col,
                        end_row: rel_row,
                        end_col: rel_col,
                    });
                    update.dirty_panes.insert(pid);
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if drag.take().is_some() {
                super::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            } else if let Some(ref sel) = text_selection {
                // Copy selected text to clipboard via OSC 52
                // Note: in server mode, the OSC 52 goes through the output buffer to the client
                if let Some(pane) = panes.get_mut(&sel.pane_id) {
                    pane.sync_scrollback();
                    let text = super::extract_selected_text(
                        pane.screen(),
                        sel.pane_id,
                        sel.start_row,
                        sel.start_col,
                        sel.end_row,
                        sel.end_col,
                    );
                    pane.reset_scrollback_view();
                    if !text.is_empty() {
                        let encoded = super::base64_encode(text.as_bytes());
                        let osc = format!("\x1b]52;c;{}\x07", encoded);
                        pane.osc52_pending.push(osc.into_bytes());
                    }
                }
                let pid = sel.pane_id;
                *text_selection = None;
                *selection_anchor = None;
                update.dirty_panes.insert(pid);
            } else {
                *selection_anchor = None;
                if let Some(pane) = panes.get_mut(active) {
                    if pane.wants_mouse() {
                        if let Some(rect) = border_cache.pane_rects().get(active) {
                            let rel_col = mouse.column.saturating_sub(rect.x);
                            let rel_row = mouse.row.saturating_sub(rect.y);
                            pane.send_mouse_event(0, rel_col, rel_row, true);
                        }
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => {
            let target = layout
                .find_at(mouse.column, mouse.row, inner)
                .unwrap_or(*active);
            if let Some(pane) = panes.get_mut(&target) {
                if pane.is_alive() {
                    if pane.wants_mouse() {
                        if let Some(rect) = border_cache.pane_rects().get(&target) {
                            let rel_col = mouse.column.saturating_sub(rect.x);
                            let rel_row = mouse.row.saturating_sub(rect.y);
                            for _ in 0..3 {
                                pane.send_mouse_scroll(true, rel_col, rel_row);
                            }
                        }
                    } else {
                        pane.scroll_up(3);
                        update.dirty_panes.insert(target);
                    }
                }
            }
        }
        MouseEventKind::ScrollDown => {
            let target = layout
                .find_at(mouse.column, mouse.row, inner)
                .unwrap_or(*active);
            if let Some(pane) = panes.get_mut(&target) {
                if pane.is_alive() {
                    if pane.wants_mouse() {
                        if let Some(rect) = border_cache.pane_rects().get(&target) {
                            let rel_col = mouse.column.saturating_sub(rect.x);
                            let rel_row = mouse.row.saturating_sub(rect.y);
                            for _ in 0..3 {
                                pane.send_mouse_scroll(false, rel_col, rel_row);
                            }
                        }
                    } else {
                        pane.scroll_down(3);
                        update.dirty_panes.insert(target);
                    }
                }
            }
        }
        _ => {}
    }
}
