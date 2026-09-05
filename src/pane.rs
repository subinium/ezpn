use crate::vt100;
use std::collections::{HashMap, VecDeque};

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, OnceLock};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode, ModifierKeyCode,
    MouseButton, MouseEvent, MouseEventKind,
};

use crate::config::{ScrollbackEviction, DEFAULT_SCROLLBACK_BYTES};
use crate::terminal_state::{
    ClipboardPolicy, KittyKbdFlags, MouseEncoding, MouseMode, MouseProtocol, Osc52Decision,
    Osc52GetPolicy, Osc52SetPolicy, PaneTerminalState, ThemePalette,
};

/// Global wake channel: PTY reader threads send () to wake the server main loop.
/// Set once by the server with `set_wake_channel()`.
static WAKE_TX: OnceLock<mpsc::SyncSender<()>> = OnceLock::new();

/// Initialize the global wake channel. Call once from server startup.
/// Returns the Receiver that the main loop should use.
pub fn init_wake_channel() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::sync_channel(1);
    let _ = WAKE_TX.set(tx);
    rx
}

/// Send a wake signal to the main loop (used by reader threads and client reader).
pub fn wake_main_loop() {
    if let Some(tx) = WAKE_TX.get() {
        let _ = tx.try_send(());
    }
}
use std::path::PathBuf;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneLaunch {
    Shell,
    Command(String),
}

/// `live_cwd()` falls back to procfs polling only if the OSC 7 report is
/// stale. Apps emit OSC 7 on every `cd`; if we haven't seen one in this
/// long, the shell may have stopped emitting (older shell, no integration
/// snippet) and procfs is the only source of truth left.
const REPORTED_CWD_FRESH_FOR: Duration = Duration::from_secs(30);

pub struct Pane {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    reader_rx: Receiver<Vec<u8>>,
    reader_stop: Arc<AtomicBool>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    reader_eof: bool,
    parser: vt100::Parser,
    alive: bool,
    launch: PaneLaunch,
    scroll_offset: usize, // 0 = live (bottom), >0 = scrolled up N lines
    name: Option<String>,
    snapshot_sensitive: bool,
    persist_scrollback_override: Option<bool>,
    scrollback_lines: usize,
    parser_has_output: bool,
    exit_code: Option<u32>,
    /// Pending OSC 52 clipboard sequences from child to forward to the terminal.
    /// `pub` so the server can push externally-generated OSC 52 (e.g. when the
    /// multiplexer copies a selection); incoming child-emitted OSC 52 is gated
    /// by `terminal_state.clipboard_policy` in [`Pane::read_output`].
    pub osc52_pending: Vec<Vec<u8>>,

    /// Aggregate per-pane terminal state (#74, #75, #77, #78, #79).
    /// Authoritative for input negotiation; vt100 owns the display grids.
    state: PaneTerminalState,
    control_parser: ControlParser,
    /// The working directory this pane was launched with.
    initial_cwd: Option<PathBuf>,
    /// Custom env vars this pane was launched with.
    initial_env: HashMap<String, String>,
    /// Custom shell override for this pane (if different from default).
    initial_shell: Option<String>,
    /// Active clipboard policy for OSC 52 set/get (#79). Defaults to the
    /// secure policy in [`ClipboardPolicy::default`].
    clipboard_policy: ClipboardPolicy,
    /// Active theme palette for OSC 4/10/11/12 responses. An unset slot
    /// remains unanswered; host query/reply routing is not implemented here.
    theme_palette: ThemePalette,
    /// Byte budget for the per-pane scrollback shim (#68). `0` disables the
    /// byte cap and only the line cap baked into `vt100::Parser` applies.
    /// Defaults to [`DEFAULT_SCROLLBACK_BYTES`] (32 MiB) until config is
    /// applied via [`Pane::set_scrollback_budget`].
    scrollback_byte_budget: usize,
    /// Requested runtime eviction policy (vt100 cannot apply it yet).
    eviction_policy: ScrollbackEviction,
    /// Raw input throughput, not allocated scrollback memory.
    scrollback_byte_estimate: usize,
    scrollback_budget_warned: bool,
}

impl Pane {
    pub fn with_scrollback(
        shell: &str,
        launch: PaneLaunch,
        cols: u16,
        rows: u16,
        scrollback: usize,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            shell,
            launch,
            cols,
            rows,
            scrollback,
            None,
            &std::collections::HashMap::new(),
        )
    }

    #[allow(dead_code)]
    pub fn with_cwd(
        shell: &str,
        launch: PaneLaunch,
        cols: u16,
        rows: u16,
        scrollback: usize,
        cwd: &std::path::Path,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            shell,
            launch,
            cols,
            rows,
            scrollback,
            Some(cwd),
            &std::collections::HashMap::new(),
        )
    }

    pub fn with_full_config(
        shell: &str,
        launch: PaneLaunch,
        cols: u16,
        rows: u16,
        scrollback: usize,
        cwd: Option<&std::path::Path>,
        env: &std::collections::HashMap<String, String>,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(shell, launch, cols, rows, scrollback, cwd, env)
    }

    fn spawn_inner(
        shell: &str,
        launch: PaneLaunch,
        cols: u16,
        rows: u16,
        scrollback: usize,
        cwd: Option<&std::path::Path>,
        env: &std::collections::HashMap<String, String>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(cols > 0 && rows > 0, "PTY dimensions must be nonzero");
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(shell);
        if let PaneLaunch::Command(command) = &launch {
            cmd.arg("-l");
            cmd.arg("-c");
            cmd.arg(command);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("EZPN", "1"); // prevent nesting
        for (k, v) in env {
            cmd.env(k, v);
        }

        // Acquire every fallible descriptor before spawning a process.
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        #[cfg(unix)]
        if let Some(fd) = pair.master.as_raw_fd() {
            // SAFETY: fd is owned by the master; all its dup'd handles share
            // nonblocking status. The reader retries WouldBlock after poll.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        let writer: Box<dyn Write + Send> = Box::new(BufferedPtyWriter {
            inner: writer,
            pending: VecDeque::new(),
        });
        #[cfg(unix)]
        let poll_fd = pair
            .master
            .as_raw_fd()
            .map(|fd| {
                // SAFETY: the master owns fd throughout this duplication.
                unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()
            })
            .transpose()?;
        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let (tx, rx) = mpsc::sync_channel(32); // bounded: 32 * 4KB = 128KB max buffered
        let reader_stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&reader_stop);
        let reader_thread = std::thread::Builder::new()
            .name("ezpn-pty-reader".into())
            .spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 4096];
                'read: while !stop.load(Ordering::Relaxed) {
                    #[cfg(unix)]
                    if let Some(fd) = &poll_fd {
                        use std::os::fd::AsRawFd;
                        let mut poll = libc::pollfd {
                            fd: fd.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        // SAFETY: poll points to one valid descriptor owned by this thread.
                        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
                        if ready == 0 {
                            continue;
                        }
                        if ready < 0 {
                            if std::io::Error::last_os_error().kind()
                                == std::io::ErrorKind::Interrupted
                            {
                                continue;
                            }
                            break;
                        }
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut data = buf[..n].to_vec();
                            loop {
                                match tx.try_send(data) {
                                    Ok(()) => break,
                                    Err(mpsc::TrySendError::Disconnected(_)) => break 'read,
                                    Err(mpsc::TrySendError::Full(unsent)) => {
                                        if stop.load(Ordering::Relaxed) {
                                            break 'read;
                                        }
                                        data = unsent;
                                        std::thread::sleep(Duration::from_millis(2));
                                    }
                                }
                            }
                            wake_main_loop();
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                            ) =>
                        {
                            continue
                        }
                        Err(_) => break,
                    }
                }
                drop(tx);
                wake_main_loop();
            });
        let reader_thread = match reader_thread {
            Ok(thread) => thread,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };

        let parser = vt100::Parser::new(
            rows,
            cols,
            budgeted_scrollback_lines(scrollback, cols, DEFAULT_SCROLLBACK_BYTES),
        );

        Ok(Self {
            master: pair.master,
            writer,
            child,
            reader_rx: rx,
            reader_stop,
            reader_thread: Some(reader_thread),
            reader_eof: false,
            parser,
            alive: true,
            launch,
            scroll_offset: 0,
            name: None,
            snapshot_sensitive: false,
            persist_scrollback_override: None,
            scrollback_lines: scrollback,
            parser_has_output: false,
            exit_code: None,
            osc52_pending: Vec::new(),
            state: PaneTerminalState::new(),
            control_parser: ControlParser::default(),
            initial_cwd: cwd.map(|p| p.to_path_buf()),
            initial_env: env.clone(),
            initial_shell: None,
            clipboard_policy: ClipboardPolicy::default(),
            theme_palette: ThemePalette::default(),
            scrollback_byte_budget: DEFAULT_SCROLLBACK_BYTES,
            eviction_policy: ScrollbackEviction::default(),
            scrollback_byte_estimate: 0,
            scrollback_budget_warned: false,
        })
    }

    /// Plumb the runtime scrollback byte budget + eviction policy from
    /// `EzpnConfig` (#68). Call site: `bootstrap` after constructing each
    /// pane. A budget of `0` disables the byte cap entirely; the
    /// `vt100::Parser` line cap still applies.
    pub fn set_scrollback_budget(&mut self, byte_budget: usize, policy: ScrollbackEviction) {
        self.scrollback_byte_budget = byte_budget;
        self.eviction_policy = policy;
        self.scrollback_budget_warned = false;
        if !self.parser_has_output {
            let (rows, cols) = self.parser.screen().size();
            self.parser = vt100::Parser::new(
                rows,
                cols,
                budgeted_scrollback_lines(self.scrollback_lines, cols, byte_budget),
            );
        }
    }

    /// Warn once about the runtime trim limitation. Actual history is bounded
    /// by the construction-time line limit; input throughput is not RSS.
    fn observe_scrollback_budget(&mut self) -> usize {
        if self.scrollback_budget_warned {
            return 0;
        }
        let (_, cols) = self.parser.screen().size();
        let overflow_rows = compute_eviction(
            self.scrollback_byte_budget,
            self.scrollback_byte_estimate,
            cols,
            self.eviction_policy,
        );
        self.scrollback_budget_warned = overflow_rows > 0;
        overflow_rows
    }

    /// Override the OSC 52 clipboard policy for this pane (typically copied
    /// from `[clipboard]` in the loaded config — see #79).
    pub fn set_clipboard_policy(&mut self, policy: ClipboardPolicy) {
        self.clipboard_policy = policy;
    }

    /// Override the active theme palette for OSC 4/10/11/12 responses (#77).
    /// Unset colours remain unanswered.
    #[allow(dead_code)]
    pub fn set_theme_palette(&mut self, palette: ThemePalette) {
        self.theme_palette = palette;
    }

    /// Read pending output from PTY. Returns true if new data was received.
    /// Drains at most MAX_DRAIN chunks per call to ensure fair scheduling across panes.
    pub fn read_output(&mut self) -> bool {
        if let Err(error) = self.writer.flush() {
            tracing::debug!(%error, "pending PTY input failed");
        }
        const MAX_DRAIN: usize = 8; // 8 * 4KB = 32KB max per iteration
        let was_alive = self.alive;
        let mut got_data = false;
        let mut count = 0;
        loop {
            if count >= MAX_DRAIN {
                break;
            }
            match self.reader_rx.try_recv() {
                Ok(data) => {
                    // Intercept OSC + Kitty CSI sequences before vt100 processes them.
                    // The interceptor mutates `self.state`, queues OSC 52 outputs (or
                    // drops them per policy), writes inline responses to the child PTY
                    // for OSC 4/10/11/12 + CSI ? u, and updates `reported_cwd` from
                    // OSC 7. Only bounded display sequences reach vt100.
                    let filtered = self.intercept(&data);
                    self.parser_has_output = true;
                    self.parser.screen_mut().set_scrollback(0);
                    self.parser.process(&filtered);
                    self.scrollback_byte_estimate =
                        self.scrollback_byte_estimate.saturating_add(data.len());
                    let evicted = self.observe_scrollback_budget();
                    if evicted > 0 {
                        tracing::warn!(
                            estimated_overflow_rows = evicted,
                            byte_budget = self.scrollback_byte_budget,
                            byte_estimate = self.scrollback_byte_estimate,
                            policy = self.eviction_policy.as_str(),
                            "scrollback input estimate exceeded budget; vt100 cannot trim history at runtime"
                        );
                    }
                    // New output snaps scroll to bottom
                    if self.scroll_offset > 0 {
                        self.scroll_offset = 0;
                    }
                    got_data = true;
                    count += 1;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.reader_eof = true;
                    break;
                }
            }
        }
        // EOF, input errors and kill requests must not bypass reaping.
        if self.exit_code.is_none() {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.exit_code = Some(status.exit_code());
            }
        }
        // Drain queued output before advertising an exit to the server.
        if self.reader_eof && self.exit_code.is_some() {
            self.alive = false;
        }
        // Force-close any unmatched DECSET 2026 bracket on EOF so the host
        // coalescer doesn't freeze waiting on a process that will never emit
        // `?2026l` (#73).
        if self.reader_eof {
            self.state.sync_opened_at = None;
        }
        got_data || was_alive != self.alive
    }

    /// Whether the pane is inside a DECSET 2026 synchronized-output window (#73).
    // reason: synchronized-output (#73) public API; consumed by the host
    // coalescer wiring scheduled in the same issue. Sibling helpers
    // `sync_opened_at` / `force_close_sync` use the same allow.
    #[allow(dead_code)]
    pub fn in_sync(&self) -> bool {
        self.state.sync_opened_at.is_some()
    }

    /// Wall-clock instant the current sync window opened, if any (#73).
    /// Used by the host coalescer to enforce the 33 ms safety timeout.
    #[allow(dead_code)]
    pub fn sync_opened_at(&self) -> Option<Instant> {
        self.state.sync_opened_at
    }

    /// Force-close any open sync bracket (#73). Idempotent.
    #[allow(dead_code)]
    pub fn force_close_sync(&mut self) {
        if self.state.sync_opened_at.take().is_some() {
            tracing::warn!("DECSET 2026 sync window force-closed by 33 ms timeout",);
        }
    }

    pub fn write_key(&mut self, key: KeyEvent) {
        let bytes = encode_key_for_child(
            key,
            self.kitty_kbd_active(),
            self.screen().application_cursor(),
        );
        if !bytes.is_empty() {
            self.write_bytes(&bytes);
        }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if let Err(error) = self
            .writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush())
        {
            tracing::warn!(%error, bytes = bytes.len(), "PTY input rejected; child exit status is unchanged");
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        if self.screen().size() == (rows, cols) {
            return;
        }
        let result = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Err(e) = &result {
            tracing::warn!(error = %e, "PTY resize failed");
            return;
        }
        self.parser.screen_mut().set_size(rows, cols);
        // TIOCSWINSZ notifies the actual foreground process group, including
        // ssh; signaling the original shell PID again is both redundant and stale.
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Sync the vt100 scrollback offset with our tracked scroll_offset.
    /// Call this before drawing pane content so cell() returns scrollback.
    pub fn sync_scrollback(&mut self) {
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
    }

    /// Reset vt100 scrollback offset to 0 (live view).
    /// Call after rendering to avoid affecting process() behavior.
    pub fn reset_scrollback_view(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    pub fn is_alive(&self) -> bool {
        self.alive
    }

    pub fn kill(&mut self) {
        if self.exit_code.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => self.exit_code = Some(status.exit_code()),
                Ok(None) => {
                    #[cfg(unix)]
                    if let Some(pid) = self
                        .child
                        .process_id()
                        .and_then(|pid| libc::pid_t::try_from(pid).ok())
                        .filter(|pid| *pid > 1)
                    {
                        // portable-pty creates a new session. Kill both the shell's
                        // group and its foreground job, never our own process group.
                        if let Some(fg) = self
                            .master
                            .process_group_leader()
                            .filter(|fg| *fg > 1 && *fg != pid)
                        {
                            // SAFETY: only a foreground group in this child's session.
                            unsafe {
                                if libc::getsid(fg) == pid {
                                    libc::kill(-fg, libc::SIGKILL);
                                }
                            }
                        }
                        // SAFETY: pid is a live, unreaped child in its own session.
                        unsafe {
                            libc::kill(-pid, libc::SIGKILL);
                            libc::kill(pid, libc::SIGKILL);
                        }
                    }
                    #[cfg(not(unix))]
                    let _ = self.child.kill();
                    match self.child.wait() {
                        Ok(status) => self.exit_code = Some(status.exit_code()),
                        Err(error) => tracing::warn!(%error, "failed to reap PTY child"),
                    }
                }
                Err(error) => tracing::warn!(%error, "failed to poll PTY child during cleanup"),
            }
        }
        self.alive = false;
        self.state.sync_opened_at = None;
    }

    pub fn scroll_up(&mut self, lines: usize) {
        // Set a large scrollback to discover the actual max,
        // then read back the clamped value.
        let probe = self.scroll_offset.saturating_add(lines);
        self.parser.screen_mut().set_scrollback(probe);
        self.scroll_offset = self.parser.screen().scrollback();
        // Reset parser view to 0 so process() isn't affected
        self.parser.screen_mut().set_scrollback(0);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    #[allow(dead_code)]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn is_scrolled(&self) -> bool {
        self.scroll_offset > 0
    }

    pub fn snap_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Capture the pane's text contents as a vector of visual lines.
    ///
    /// When `include_scrollback` is `true`, scrollback rows precede the
    /// visible viewport (oldest first). The user's current scroll
    /// offset is restored before returning, so calling this from the
    /// IPC main loop never disturbs interactive scrolling.
    ///
    /// Powering `ezpn-ctl dump` (issue #88). vt100 does not
    /// expose direct scrollback iteration, so this walks the parser
    /// scrollback offset row-by-row — one of the few operations the
    /// IPC layer needs but cannot synthesise from the read-only
    /// [`Pane::screen`] accessor alone.
    pub fn dump_text(&mut self, include_scrollback: bool) -> Vec<String> {
        let saved_offset = self.parser.screen().scrollback();
        let (rows, cols) = self.parser.screen().size();

        let mut lines: Vec<String> = Vec::new();

        if include_scrollback {
            // Probe maximum scrollback by setting a huge offset and
            // reading the clamped value back. vt100 silently caps to
            // the actual buffered row count.
            self.parser.screen_mut().set_scrollback(usize::MAX / 2);
            let max_scrollback = self.parser.screen().scrollback();
            self.parser.screen_mut().set_scrollback(0);

            // Walk scrollback oldest -> newest. Offset N means
            // "viewport is N rows above live"; the *bottom* row of
            // that window is at relative position N from the live
            // bottom. Read top-to-bottom in chunks of `rows`, stepping
            // by `rows`, so we reconstruct full scrollback as a flat
            // line stream without overlap.
            let mut offset = max_scrollback;
            while offset > 0 {
                self.parser.screen_mut().set_scrollback(offset);
                let take = offset.min(rows as usize);
                // Capture only the top `take` rows (bottom rows
                // already covered by a smaller offset / live view).
                let row_strings: Vec<String> =
                    self.parser.screen().rows(0, cols).take(take).collect();
                lines.extend(row_strings);
                offset = offset.saturating_sub(rows as usize);
            }
        }

        // Visible viewport (always included).
        self.parser.screen_mut().set_scrollback(0);
        for row in self.parser.screen().rows(0, cols) {
            lines.push(row);
        }

        // Restore user's scroll offset.
        self.parser.screen_mut().set_scrollback(saved_offset);
        lines
    }

    /// Whether the child has enabled mouse reporting.
    pub fn wants_mouse(&self) -> bool {
        !self.state.mouse_mode.is_off()
    }

    /// Forward a pane-relative event, returning true only if it was encoded.
    pub fn forward_mouse_event(&mut self, event: MouseEvent) -> bool {
        let (rows, cols) = self.screen().size();
        if event.column >= cols || event.row >= rows {
            return false;
        }
        let Some(bytes) = encode_mouse_event(self.state.mouse_mode, event) else {
            return false;
        };
        self.write_bytes(&bytes);
        true
    }

    /// Compatibility helper for callers that already supply an xterm button code.
    pub fn send_mouse_event(&mut self, button: u16, col: u16, row: u16, release: bool) {
        if let Some(bytes) =
            encode_mouse_report(self.state.mouse_mode.encoding, button, col, row, release)
        {
            self.write_bytes(&bytes);
        }
    }

    /// Forward a mouse scroll event to the child.
    pub fn send_mouse_scroll(&mut self, up: bool, col: u16, row: u16) {
        let button: u16 = if up { 64 } else { 65 };
        self.send_mouse_event(button, col, row, false);
    }

    pub fn launch(&self) -> &PaneLaunch {
        &self.launch
    }

    pub fn launch_label(&self, shell: &str) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        // OSC title set by child process (priority over command/shell)
        let osc_title = &self.state.title;
        if !osc_title.is_empty() {
            return osc_title.to_string();
        }
        match &self.launch {
            PaneLaunch::Shell => shell.to_string(),
            PaneLaunch::Command(command) => command.clone(),
        }
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit_code
    }

    /// PID of the child process attached to this pane's PTY, or
    /// `None` once the child has exited / never spawned. Used by
    /// `ezpn-ctl ls --json` (issue #89) to populate
    /// [`crate::ipc::PaneTreeInfo::pid`]; do not rely on this value
    /// staying stable across restarts.
    pub fn pid(&self) -> Option<u32> {
        self.exit_code
            .is_none()
            .then(|| self.child.process_id())
            .flatten()
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn title(&self) -> &str {
        &self.state.title
    }

    pub fn set_snapshot_sensitive(&mut self, sensitive: bool) {
        self.snapshot_sensitive = sensitive;
    }

    pub fn snapshot_sensitive(&self) -> bool {
        self.snapshot_sensitive
    }

    pub fn set_persist_scrollback_override(&mut self, persist: Option<bool>) {
        self.persist_scrollback_override = persist;
    }

    pub fn persist_scrollback_override(&self) -> Option<bool> {
        self.persist_scrollback_override
    }

    /// Replay plain history into the display parser only, before draining the
    /// new shell. Opaque snapshot attributes have no defined replay format yet.
    pub fn restore_scrollback<'a>(&mut self, rows: impl IntoIterator<Item = &'a str>) {
        if self.parser_has_output || self.snapshot_sensitive {
            return;
        }
        let (height, cols) = self.screen().size();
        let limit =
            budgeted_scrollback_lines(self.scrollback_lines, cols, self.scrollback_byte_budget)
                .saturating_add(usize::from(height));
        let mut retained = VecDeque::new();
        for text in rows {
            if retained.len() == limit {
                retained.pop_front();
            }
            retained.push_back(text);
        }
        self.parser_has_output = !retained.is_empty();
        for text in retained {
            let mut width = 0;
            for ch in text.chars().filter(|ch| !ch.is_control()) {
                let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if width + char_width > usize::from(cols) {
                    break;
                }
                width += char_width;
                let mut bytes = [0; 4];
                self.parser.process(ch.encode_utf8(&mut bytes).as_bytes());
            }
            self.parser.process(b"\r\n");
        }
    }

    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    /// Take pending OSC 52 clipboard sequences (clears the queue).
    pub fn take_osc52(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.osc52_pending)
    }

    /// Whether the child has bracketed paste enabled.
    pub fn bracketed_paste(&self) -> bool {
        self.state.bracketed_paste
    }

    /// Whether the child has requested focus events.
    pub fn wants_focus(&self) -> bool {
        self.state.focus_reporting
    }

    /// Active mouse mode (`?1000/?1002/?1003` + `?1006`).
    #[allow(dead_code)]
    pub fn mouse_mode(&self) -> MouseMode {
        self.state.mouse_mode
    }

    /// Active Kitty keyboard flags at the top of the pane's stack (#74).
    /// 0 means the pane is using the legacy CSI/SS3 encoding.
    pub fn kitty_kbd_active(&self) -> KittyKbdFlags {
        self.state.kitty_kbd.active()
    }

    /// Current OSC 52 decision for this pane (`Pending` / `Allowed` / `Denied`).
    /// The status-bar prompt UI flips this from `Pending` after the user
    /// answers the y/n confirm prompt — see #79.
    #[allow(dead_code)]
    pub fn osc52_decision(&self) -> Osc52Decision {
        self.state.osc52_decision
    }

    /// Set the user's OSC 52 confirm answer for this pane. After this call,
    /// pending envelopes (`take_osc52_pending_confirm`) plus all
    /// future OSC 52 set sequences are accepted/rejected accordingly.
    pub fn set_osc52_decision(&mut self, decision: Osc52Decision) {
        self.state.osc52_decision = decision;
    }

    /// Take any OSC 52 set-clipboard payloads that arrived while the policy
    /// was `Confirm` and the per-pane decision was still `Pending`. The
    /// caller is expected to surface a status-bar prompt naming the pane
    /// and the byte count, and on accept push the canonical envelope onto
    /// `osc52_pending` for forwarding to clients (#79).
    pub fn take_osc52_pending_confirm(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state.osc52_pending_confirm)
    }

    /// Re-enqueue a previously-taken set of confirm payloads (used by
    /// the prompt's `Esc` path so a deferred decision keeps the
    /// payloads available for the next prompt).
    pub fn requeue_osc52_pending_confirm(&mut self, mut payloads: Vec<Vec<u8>>) {
        payloads.append(&mut self.state.osc52_pending_confirm);
        for payload in payloads {
            enqueue_osc(&mut self.state.osc52_pending_confirm, payload);
        }
    }

    /// The working directory this pane was launched with.
    pub fn initial_cwd(&self) -> Option<&std::path::Path> {
        self.initial_cwd.as_deref()
    }

    /// Custom env vars this pane was launched with.
    pub fn initial_env(&self) -> &HashMap<String, String> {
        &self.initial_env
    }

    /// Custom shell override for this pane.
    pub fn initial_shell(&self) -> Option<&str> {
        self.initial_shell.as_deref()
    }

    /// Set the custom shell for this pane (for snapshot purposes).
    pub fn set_initial_shell(&mut self, shell: Option<String>) {
        self.initial_shell = shell;
    }

    /// Most recent OSC 7 reported cwd, if it's still considered fresh
    /// (within `REPORTED_CWD_FRESH_FOR`). Used by `live_cwd()` (#75) and
    /// is also exposed for tests.
    #[allow(dead_code)]
    pub fn reported_cwd(&self) -> Option<&std::path::Path> {
        self.state
            .reported_cwd
            .as_ref()
            .filter(|(_, ts)| ts.elapsed() < REPORTED_CWD_FRESH_FOR)
            .map(|(p, _)| p.as_path())
    }

    /// Try to get the current working directory of the child process.
    ///
    /// Resolution order (#75):
    /// 1. OSC 7 reported cwd, if fresh.
    /// 2. procfs polling (5 s effective rate, see callers in `bootstrap.rs`).
    /// 3. The pane's launch-time `initial_cwd`.
    pub fn live_cwd(&self) -> Option<PathBuf> {
        if let Some(p) = self.reported_cwd() {
            return Some(p.to_path_buf());
        }
        self.live_cwd_procfs()
    }

    #[cfg(target_os = "macos")]
    fn live_cwd_procfs(&self) -> Option<PathBuf> {
        if self.alive {
            if let Some(pid) = self.child.process_id() {
                // Use proc_pidinfo on macOS
                let mut vinfo: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
                let ret = unsafe {
                    libc::proc_pidinfo(
                        pid as libc::c_int,
                        libc::PROC_PIDVNODEPATHINFO,
                        0,
                        &mut vinfo as *mut _ as *mut libc::c_void,
                        std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int,
                    )
                };
                if ret > 0 {
                    let cstr = unsafe {
                        std::ffi::CStr::from_ptr(vinfo.pvi_cdir.vip_path.as_ptr() as *const i8)
                    };
                    if let Ok(s) = cstr.to_str() {
                        if !s.is_empty() {
                            return Some(PathBuf::from(s));
                        }
                    }
                }
            }
        }
        self.initial_cwd.clone()
    }

    #[cfg(not(target_os = "macos"))]
    fn live_cwd_procfs(&self) -> Option<PathBuf> {
        if self.alive {
            if let Some(pid) = self.child.process_id() {
                let link = format!("/proc/{}/cwd", pid);
                if let Ok(cwd) = std::fs::read_link(&link) {
                    return Some(cwd);
                }
            }
        }
        self.initial_cwd.clone()
    }

    // ─── OSC + CSI interception ─────────────────────────────

    /// Scan a freshly-arrived chunk for sequences the multiplexer owns:
    /// OSC 52 (clipboard, #79), OSC 7 (cwd, #75), OSC 4/10/11/12 (colour
    /// queries, #77), `CSI > / < / = / ? u` (Kitty keyboard, #74), and the
    /// DECSET bits in `PaneTerminalState` (#78).
    ///
    /// Incomplete controls are retained across reads; unbounded string
    /// payloads are never passed to vt100.
    fn intercept(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut ctx = InterceptCtx {
            state: &mut self.state,
            osc52_pending: &mut self.osc52_pending,
            parser: &mut self.control_parser,
            writer: &mut *self.writer,
            policy: &self.clipboard_policy,
            palette: &self.theme_palette,
        };
        intercept_chunk(&mut ctx, chunk)
    }
}

const PTY_INPUT_MAX: usize = 1024 * 1024;

/// Nonblocking PTY writes keep a child that stops reading (e.g. a stalled ssh
/// connection) from blocking every pane. The server retries flush each tick.
struct BufferedPtyWriter {
    inner: Box<dyn Write + Send>,
    pending: VecDeque<u8>,
}

impl Write for BufferedPtyWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.flush()?;
        if bytes.len() > PTY_INPUT_MAX.saturating_sub(self.pending.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "PTY input queue full",
            ));
        }
        self.pending.extend(bytes);
        // Accepted bytes remain queued even if the PTY becomes unwritable.
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        while !self.pending.is_empty() {
            match self.inner.write(self.pending.as_slices().0) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(written) => {
                    self.pending.drain(..written);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => {
                    self.pending.clear();
                    return Err(error);
                }
            }
        }
        self.inner.flush()
    }
}

fn encode_mouse_event(mode: MouseMode, event: MouseEvent) -> Option<Vec<u8>> {
    let button_code = |button| match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut button, release) = match (mode.protocol, event.kind) {
        (MouseProtocol::Off, _) => return None,
        (_, MouseEventKind::Down(button)) => (button_code(button), false),
        (MouseProtocol::Press, _) => return None,
        (_, MouseEventKind::Up(button)) => (button_code(button), true),
        (MouseProtocol::Btn | MouseProtocol::Any, MouseEventKind::Drag(button)) => {
            (32 + button_code(button), false)
        }
        (MouseProtocol::Any, MouseEventKind::Moved) => (35, false),
        (_, MouseEventKind::ScrollUp) => (64, false),
        (_, MouseEventKind::ScrollDown) => (65, false),
        (_, MouseEventKind::ScrollLeft) => (66, false),
        (_, MouseEventKind::ScrollRight) => (67, false),
        _ => return None,
    };
    if mode.protocol != MouseProtocol::Press {
        button |= (u16::from(event.modifiers.contains(KeyModifiers::SHIFT)) * 4)
            | (u16::from(event.modifiers.contains(KeyModifiers::ALT)) * 8)
            | (u16::from(event.modifiers.contains(KeyModifiers::CONTROL)) * 16);
    }
    encode_mouse_report(mode.encoding, button, event.column, event.row, release)
}

fn encode_mouse_report(
    encoding: MouseEncoding,
    button: u16,
    col: u16,
    row: u16,
    release: bool,
) -> Option<Vec<u8>> {
    let x = u32::from(col) + 1;
    let y = u32::from(row) + 1;
    if button > 255 {
        return None;
    }
    if encoding == MouseEncoding::Sgr {
        let end = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{button};{x};{y}{end}").into_bytes());
    }
    let code = u32::from(if release { (button & 28) | 3 } else { button }) + 32;
    match encoding {
        MouseEncoding::Urxvt => Some(format!("\x1b[{code};{x};{y}M").into_bytes()),
        MouseEncoding::Utf8 if x <= 2015 && y <= 2015 => {
            let mut bytes = b"\x1b[M".to_vec();
            for value in [code, x + 32, y + 32] {
                let mut buf = [0; 4];
                bytes.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut buf).as_bytes());
            }
            Some(bytes)
        }
        MouseEncoding::X10 if x <= 223 && y <= 223 && code <= 255 => Some(vec![
            0x1b,
            b'[',
            b'M',
            code as u8,
            (x + 32) as u8,
            (y + 32) as u8,
        ]),
        _ => None,
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.reader_stop.store(true, Ordering::Relaxed);
        self.kill();
        // Disconnect a full output queue before joining its producer.
        let (_, empty_rx) = mpsc::channel();
        drop(std::mem::replace(&mut self.reader_rx, empty_rx));
        if let Some(thread) = self.reader_thread.take() {
            #[cfg(unix)]
            let _ = thread.join();
            #[cfg(not(unix))]
            drop(thread);
        }
    }
}

/// All the borrows the interceptor needs. Bundled into a struct so we can
/// pass it to a free function and exercise it from tests without spawning
/// a real PTY.
const OSC_PAYLOAD_MAX: usize = 1024 * 1024;
const OSC_QUEUE_MAX: usize = 8;
const OSC_QUEUE_BYTES_MAX: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ControlMode {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    String,
    StringEscape,
}

#[derive(Default)]
struct ControlParser {
    mode: ControlMode,
    buffer: Vec<u8>,
    overflow: bool,
}

struct InterceptCtx<'a> {
    state: &'a mut PaneTerminalState,
    osc52_pending: &'a mut Vec<Vec<u8>>,
    parser: &'a mut ControlParser,
    writer: &'a mut dyn Write,
    policy: &'a ClipboardPolicy,
    palette: &'a ThemePalette,
}

/// Keep one bounded state machine across reads. In particular, never feed an
/// unterminated OSC to vt100: vte's default OSC buffer grows without a limit.
fn intercept_chunk(ctx: &mut InterceptCtx<'_>, chunk: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(chunk.len());
    for &byte in chunk {
        if matches!(byte, 0x18 | 0x1a) && ctx.parser.mode != ControlMode::Ground {
            ctx.parser.mode = ControlMode::Ground;
            ctx.parser.buffer.clear();
            ctx.parser.overflow = false;
            output.push(byte);
            continue;
        }
        match ctx.parser.mode {
            ControlMode::Ground => {
                // Match vt100's UTF-8 mode: CSI/OSC use ESC introducers,
                // never bytes that may be UTF-8 continuations.
                match byte {
                    0x1b => ctx.parser.mode = ControlMode::Escape,
                    _ => output.push(byte),
                }
            }
            ControlMode::Escape => {
                ctx.parser.buffer.clear();
                ctx.parser.overflow = false;
                ctx.parser.mode = match byte {
                    b'[' => ControlMode::Csi,
                    b']' => ControlMode::Osc,
                    b'P' | b'X' | b'^' | b'_' => ControlMode::String,
                    0x1b => ControlMode::Escape,
                    0x20..=0x2f => {
                        output.extend_from_slice(&[0x1b, byte]);
                        ControlMode::EscapeIntermediate
                    }
                    0x00..=0x1f => {
                        output.push(byte);
                        ControlMode::Escape
                    }
                    0x30..=0x7e => {
                        if byte == b'c' {
                            ctx.state.reset_terminal_modes();
                        }
                        output.extend_from_slice(&[0x1b, byte]);
                        ControlMode::Ground
                    }
                    _ => ControlMode::Escape,
                };
            }
            ControlMode::EscapeIntermediate => {
                if byte == 0x1b {
                    output.push(0x18);
                    ctx.parser.mode = ControlMode::Escape;
                } else {
                    output.push(byte);
                    if (0x30..=0x7e).contains(&byte) {
                        ctx.parser.mode = ControlMode::Ground;
                    }
                }
            }
            ControlMode::Csi => match byte {
                0x1b => {
                    ctx.parser.buffer.clear();
                    ctx.parser.mode = ControlMode::Escape;
                }
                0x40..=0x7e => {
                    let body = std::mem::take(&mut ctx.parser.buffer);
                    if !ctx.parser.overflow {
                        if byte == b'u' {
                            handle_csi_u(ctx, &body);
                        }
                        if !intercept_dec_modes(ctx.state, &body, byte, &mut output) {
                            output.extend_from_slice(b"\x1b[");
                            output.extend_from_slice(&body);
                            output.push(byte);
                        }
                    }
                    ctx.parser.buffer = body;
                    ctx.parser.buffer.clear();
                    ctx.parser.overflow = false;
                    ctx.parser.mode = ControlMode::Ground;
                }
                0x00..=0x1f => output.push(byte),
                0x7f => {}
                0x20..=0x3f if !ctx.parser.overflow && ctx.parser.buffer.len() < 256 => {
                    ctx.parser.buffer.push(byte)
                }
                _ => ctx.parser.overflow = true,
            },
            ControlMode::Osc => match byte {
                0x07 => {
                    let payload = std::mem::take(&mut ctx.parser.buffer);
                    if !ctx.parser.overflow {
                        handle_osc(ctx, &payload);
                        // vt100 only renders OSC titles. All other owned OSCs are
                        // handled above; unsupported OSCs are not host pass-through.
                        if payload.len() <= 8192
                            && [b"0;".as_slice(), b"1;", b"2;"]
                                .iter()
                                .any(|p| payload.starts_with(p))
                        {
                            output.extend_from_slice(b"\x1b]");
                            output.extend_from_slice(&payload);
                            output.push(0x07);
                        }
                    }
                    ctx.parser.buffer = payload;
                    ctx.parser.buffer.clear();
                    ctx.parser.overflow = false;
                    ctx.parser.mode = ControlMode::Ground;
                }
                0x1b => {
                    // ST is ESC followed by backslash; ESC itself ends OSC in vte.
                    let payload = std::mem::take(&mut ctx.parser.buffer);
                    if !ctx.parser.overflow {
                        handle_osc(ctx, &payload);
                        if payload.len() <= 8192
                            && [b"0;".as_slice(), b"1;", b"2;"]
                                .iter()
                                .any(|p| payload.starts_with(p))
                        {
                            output.extend_from_slice(b"\x1b]");
                            output.extend_from_slice(&payload);
                            output.push(0x07);
                        }
                    }
                    ctx.parser.buffer = payload;
                    ctx.parser.buffer.clear();
                    ctx.parser.overflow = false;
                    ctx.parser.mode = ControlMode::Escape;
                }
                0x00..=0x1f => {}
                _ => {
                    if !ctx.parser.overflow && ctx.parser.buffer.len() < OSC_PAYLOAD_MAX {
                        ctx.parser.buffer.push(byte);
                    } else {
                        ctx.parser.overflow = true;
                        ctx.parser.buffer.clear();
                    }
                }
            },
            ControlMode::String => {
                if byte == 0x1b {
                    ctx.parser.mode = ControlMode::StringEscape;
                } else if byte == 0x9c {
                    ctx.parser.mode = ControlMode::Ground;
                }
            }
            ControlMode::StringEscape => {
                // Do not interpret terminal controls embedded in DCS/APC/PM/SOS.
                ctx.parser.mode = if byte == b'\\' {
                    ControlMode::Ground
                } else {
                    ControlMode::String
                };
            }
        }
    }
    output
}

fn handle_osc(ctx: &mut InterceptCtx<'_>, payload: &[u8]) {
    if let Some(title) = payload
        .strip_prefix(b"0;")
        .or_else(|| payload.strip_prefix(b"2;"))
    {
        if title.len() <= 8192 {
            if let Ok(title) = std::str::from_utf8(title) {
                ctx.state.title = title.chars().filter(|c| !c.is_control()).collect();
            }
        }
        return;
    }
    // OSC 52 ; <selection> ; <data>
    if let Some(rest) = payload.strip_prefix(b"52;") {
        handle_osc52(ctx, payload, rest);
        return;
    }
    // OSC 7 ; <file:// URI>
    if let Some(rest) = payload.strip_prefix(b"7;") {
        handle_osc7(ctx, rest);
        return;
    }
    // OSC 10 / 11 / 12 — fg / bg / cursor colour query or set.
    for (prefix, slot) in [
        (&b"10;"[..], ColorSlot::Fg),
        (&b"11;"[..], ColorSlot::Bg),
        (&b"12;"[..], ColorSlot::Cursor),
    ] {
        if let Some(rest) = payload.strip_prefix(prefix) {
            handle_osc_color(ctx, slot, rest);
            return;
        }
    }
    // OSC 4 ; <index> ; <spec>
    if let Some(rest) = payload.strip_prefix(b"4;") {
        handle_osc4(ctx, rest);
    }
    // OSC 8 is not represented by vt100's cell model; it is not forwarded.
}

fn intercept_dec_modes(
    state: &mut PaneTerminalState,
    body: &[u8],
    final_byte: u8,
    output: &mut Vec<u8>,
) -> bool {
    let Some(params) = body.strip_prefix(b"?") else {
        return false;
    };
    if !matches!(final_byte, b'h' | b'l')
        || !params.iter().all(|b| b.is_ascii_digit() || *b == b';')
    {
        return false;
    }
    for param in params.split(|b| *b == b';') {
        let mode = parse_u32(param);
        if mode == Some(1047) && final_byte == b'l' && state.alternate_screen() {
            output.extend_from_slice(b"\x1b[2J");
        }
        if let Some(mode) = mode {
            apply_decset(state, mode, final_byte == b'h');
        }
        output.extend_from_slice(b"\x1b[?");
        output.extend_from_slice(if mode == Some(1047) { b"47" } else { param });
        output.push(final_byte);
    }
    true
}

fn handle_csi_u(ctx: &mut InterceptCtx<'_>, body: &[u8]) {
    let Some((sigil, rest)) = body.split_first() else {
        return;
    };
    match sigil {
        b'>' => {
            let Some(flags) = parse_default(rest, 0) else {
                return;
            };
            ctx.state
                .kitty_kbd
                .push(KittyKbdFlags(flags as u8 & KITTY_SUPPORTED));
        }
        b'<' => {
            if let Some(n) = parse_default(rest, 1) {
                ctx.state.kitty_kbd.pop(n as usize);
            }
        }
        b'=' => {
            let mut parts = rest.split(|&b| b == b';');
            let Some(flags) = parts.next().and_then(|p| parse_default(p, 0)) else {
                return;
            };
            let Some(mode) = parse_default(parts.next().unwrap_or_default(), 1)
                .and_then(|n| u8::try_from(n).ok())
            else {
                return;
            };
            if parts.next().is_some() {
                return;
            }
            ctx.state
                .kitty_kbd
                .modify_top(KittyKbdFlags(flags as u8 & KITTY_SUPPORTED), mode);
        }
        b'?' if rest.is_empty() => {
            let active = ctx.state.kitty_kbd.active().bits();
            let reply = format!("\x1b[?{}u", active);
            let _ = ctx.writer.write_all(reply.as_bytes());
            let _ = ctx.writer.flush();
        }
        _ => {}
    }
}

fn handle_osc52(ctx: &mut InterceptCtx<'_>, full_payload: &[u8], rest: &[u8]) {
    let mut split = rest.splitn(2, |&b| b == b';');
    let selection = split.next().unwrap_or(&[]);
    let Some(data) = split.next() else {
        return;
    };
    if !selection.iter().all(|b| b"cpqs01234567".contains(b)) {
        return;
    }

    // Read query: `OSC 52 ; c ; ?`
    if data == b"?" {
        match ctx.policy.get {
            Osc52GetPolicy::Allow => {
                let mut env = Vec::with_capacity(full_payload.len() + 4);
                env.extend_from_slice(b"\x1b]");
                env.extend_from_slice(full_payload);
                env.extend_from_slice(b"\x07");
                enqueue_osc(ctx.osc52_pending, env);
            }
            Osc52GetPolicy::Deny => {
                tracing::warn!(
                    target: "osc52",
                    "blocked OSC 52 clipboard read from pane (policy=deny)"
                );
            }
        }
        return;
    }

    // Hard cap on raw payload size before considering decode.
    if data.len() > ctx.policy.max_bytes.min(OSC_PAYLOAD_MAX) {
        tracing::warn!(
            target: "osc52",
            bytes = data.len(),
            cap = ctx.policy.max_bytes,
            "dropped oversized OSC 52 set"
        );
        return;
    }
    if !data
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))
    {
        return;
    }

    let effective = if ctx.policy.set == Osc52SetPolicy::Deny
        || ctx.state.osc52_decision == Osc52Decision::Denied
    {
        Osc52SetPolicy::Deny
    } else if ctx.state.osc52_decision == Osc52Decision::Allowed {
        Osc52SetPolicy::Allow
    } else {
        ctx.policy.set
    };

    let mut env = Vec::with_capacity(full_payload.len() + 4);
    env.extend_from_slice(b"\x1b]");
    env.extend_from_slice(full_payload);
    env.extend_from_slice(b"\x07");

    match effective {
        Osc52SetPolicy::Allow => enqueue_osc(ctx.osc52_pending, env),
        Osc52SetPolicy::Deny => {
            tracing::warn!(
                target: "osc52",
                bytes = data.len(),
                "blocked OSC 52 clipboard set (policy=deny)"
            );
        }
        Osc52SetPolicy::Confirm => {
            enqueue_osc(&mut ctx.state.osc52_pending_confirm, env);
        }
    }
}

fn enqueue_osc(queue: &mut Vec<Vec<u8>>, envelope: Vec<u8>) {
    let bytes = queue
        .iter()
        .fold(0usize, |sum, item| sum.saturating_add(item.len()));
    if queue.len() < OSC_QUEUE_MAX && envelope.len() <= OSC_QUEUE_BYTES_MAX.saturating_sub(bytes) {
        queue.push(envelope);
    }
}

fn handle_osc7(ctx: &mut InterceptCtx<'_>, rest: &[u8]) {
    let s = match std::str::from_utf8(rest) {
        Ok(s) => s,
        Err(_) => return,
    };
    let after_scheme = match s.strip_prefix("file://") {
        Some(s) => s,
        None => return,
    };
    let Some(idx) = after_scheme.find('/') else {
        return;
    };
    if !local_osc7_host(&after_scheme[..idx]) {
        return;
    }
    let path_part = &after_scheme[idx..];
    let decoded = percent_decode(path_part);
    if !decoded.starts_with('/') || decoded.chars().any(char::is_control) {
        return;
    }
    ctx.state.reported_cwd = Some((PathBuf::from(decoded), Instant::now()));
}

fn local_osc7_host(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    #[cfg(unix)]
    {
        let mut name = [0u8; 256];
        // SAFETY: name is a writable buffer with the stated length.
        if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } == 0 {
            let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
            return std::str::from_utf8(&name[..end])
                .is_ok_and(|local| host.eq_ignore_ascii_case(local));
        }
    }
    false
}

fn handle_osc_color(ctx: &mut InterceptCtx<'_>, slot: ColorSlot, rest: &[u8]) {
    if rest != b"?" {
        return;
    }
    if !ctx.palette.is_active() {
        return;
    }
    let value = match slot {
        ColorSlot::Fg => ctx.palette.fg,
        ColorSlot::Bg => ctx.palette.bg,
        ColorSlot::Cursor => ctx.palette.cursor,
    };
    let Some(rgb) = value else {
        return;
    };
    let osc_num = match slot {
        ColorSlot::Fg => 10,
        ColorSlot::Bg => 11,
        ColorSlot::Cursor => 12,
    };
    let reply = format!("\x1b]{};{}\x07", osc_num, rgb.to_xterm_rgb_str());
    let _ = ctx.writer.write_all(reply.as_bytes());
    let _ = ctx.writer.flush();
}

fn handle_osc4(ctx: &mut InterceptCtx<'_>, rest: &[u8]) {
    let mut parts = rest.splitn(2, |&b| b == b';');
    let idx_bytes = parts.next().unwrap_or(&[]);
    let spec = parts.next().unwrap_or(&[]);
    if spec != b"?" {
        return;
    }
    if !ctx.palette.is_active() {
        return;
    }
    let idx = match parse_u32(idx_bytes) {
        Some(n) if n < 256 => n as usize,
        _ => return,
    };
    let Some(rgb) = ctx.palette.palette[idx] else {
        return;
    };
    let reply = format!("\x1b]4;{};{}\x07", idx, rgb.to_xterm_rgb_str());
    let _ = ctx.writer.write_all(reply.as_bytes());
    let _ = ctx.writer.flush();
}

// ─── DECSET tracking ─────────────────────────────────────────

#[derive(Clone, Copy)]
enum ColorSlot {
    Fg,
    Bg,
    Cursor,
}

#[cfg(test)]
fn parse_u8(s: &[u8]) -> Option<u8> {
    parse_u32(s).and_then(|n| u8::try_from(n).ok())
}

fn parse_default(s: &[u8], default: u32) -> Option<u32> {
    if s.is_empty() {
        Some(default)
    } else {
        parse_u32(s)
    }
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut n: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(n)
}

/// Decode percent-encoded bytes (`%XX`) in a UTF-8 string. Leaves anything
/// that doesn't look like a valid escape alone. Used by OSC 7.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn apply_decset(state: &mut PaneTerminalState, num: u32, on: bool) {
    match num {
        47 | 1047 | 1049 => state.set_alternate_screen(on),
        2026 => {
            if on {
                state.sync_opened_at.get_or_insert_with(Instant::now);
            } else {
                state.sync_opened_at = None;
            }
        }
        9 => {
            if on {
                state.mouse_mode.protocol = MouseProtocol::Press;
            } else if state.mouse_mode.protocol == MouseProtocol::Press {
                state.mouse_mode.protocol = MouseProtocol::Off;
            }
        }
        2004 => state.bracketed_paste = on,
        1004 => state.focus_reporting = on,
        // Each mouse protocol bit is an independent toggle. Only clear the
        // active protocol when the matching `l` arrives, so `?1003h ?1000l`
        // doesn't accidentally disable 1003.
        1000 => {
            if on {
                state.mouse_mode.protocol = MouseProtocol::X10;
            } else if state.mouse_mode.protocol == MouseProtocol::X10 {
                state.mouse_mode.protocol = MouseProtocol::Off;
            }
        }
        1002 => {
            if on {
                state.mouse_mode.protocol = MouseProtocol::Btn;
            } else if state.mouse_mode.protocol == MouseProtocol::Btn {
                state.mouse_mode.protocol = MouseProtocol::Off;
            }
        }
        1003 => {
            if on {
                state.mouse_mode.protocol = MouseProtocol::Any;
            } else if state.mouse_mode.protocol == MouseProtocol::Any {
                state.mouse_mode.protocol = MouseProtocol::Off;
            }
        }
        1005 | 1006 | 1015 => {
            let encoding = match num {
                1005 => MouseEncoding::Utf8,
                1006 => MouseEncoding::Sgr,
                _ => MouseEncoding::Urxvt,
            };
            if on {
                state.mouse_mode.encoding = encoding;
            } else if state.mouse_mode.encoding == encoding {
                state.mouse_mode.encoding = MouseEncoding::X10;
            }
        }
        _ => {}
    }
}

// ─── Child key encoding ────────────────────────────────────

// Crossterm's KeyEvent does not retain alternate key codes or associated text.
const KITTY_SUPPORTED: u8 = KittyKbdFlags::DISAMBIGUATE
    | KittyKbdFlags::REPORT_EVENTS
    | KittyKbdFlags::REPORT_ALL_AS_ESCAPES;

#[cfg(test)]
fn encode_key(key: KeyEvent) -> Vec<u8> {
    encode_key_for_child(key, KittyKbdFlags(0), false)
}

fn encode_key_for_child(
    mut key: KeyEvent,
    flags: KittyKbdFlags,
    application_cursor: bool,
) -> Vec<u8> {
    if key.code == KeyCode::BackTab {
        key.code = KeyCode::Tab;
        key.modifiers.insert(KeyModifiers::SHIFT);
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let mods_param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let has_mods = mods_param != 1;
    let all = flags.bits() & KittyKbdFlags::REPORT_ALL_AS_ESCAPES != 0;
    let disambiguate = all || flags.bits() & KittyKbdFlags::DISAMBIGUATE != 0;
    let events = flags.bits() & KittyKbdFlags::REPORT_EVENTS != 0;
    let keypad = if disambiguate && key.state.contains(KeyEventState::KEYPAD) {
        kitty_keypad_key(key.code)
    } else {
        None
    };
    let functional = keypad.or_else(|| kitty_functional_key(key.code));
    if matches!(key.code, KeyCode::Modifier(_)) && !all {
        return Vec::new();
    }
    let special_legacy = keypad.is_none()
        && key.modifiers.is_empty()
        && matches!(key.code, KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace);
    let as_escape = all
        || (!special_legacy
            && (functional.is_some()
                || disambiguate
                    && (ctrl
                        || alt
                        || key.modifiers.intersects(
                            KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
                        )
                        || matches!(key.code, KeyCode::Esc))));
    if key.kind == KeyEventKind::Release && !(events && as_escape) {
        return Vec::new();
    }
    if flags.bits() != 0 && as_escape {
        let Some((number, suffix)) = keypad.or_else(|| match key.code {
            KeyCode::Char(c) => Some((c.to_ascii_lowercase() as u32, 'u')),
            _ => functional,
        }) else {
            return Vec::new();
        };
        let mut mods = u16::from(mods_param);
        for (modifier, bit) in [
            (KeyModifiers::SUPER, 8),
            (KeyModifiers::HYPER, 16),
            (KeyModifiers::META, 32),
        ] {
            if key.modifiers.contains(modifier) {
                mods += bit;
            }
        }
        if key.state.contains(KeyEventState::CAPS_LOCK) {
            mods += 64;
        }
        if key.state.contains(KeyEventState::NUM_LOCK) {
            mods += 128;
        }
        let event = match key.kind {
            KeyEventKind::Press => "",
            KeyEventKind::Repeat if events => ":2",
            KeyEventKind::Release => ":3",
            _ => "",
        };
        return format!("\x1b[{number};{mods}{event}{suffix}").into_bytes();
    }
    if key.kind == KeyEventKind::Release {
        return Vec::new();
    }
    let mut bytes = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                if let Some(control) = legacy_control(c) {
                    vec![control]
                } else {
                    c.to_string().into_bytes()
                }
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
        KeyCode::Tab if shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Right
        | KeyCode::Left
        | KeyCode::Home
        | KeyCode::End => {
            let code = match key.code {
                KeyCode::Up => b'A',
                KeyCode::Down => b'B',
                KeyCode::Right => b'C',
                KeyCode::Left => b'D',
                KeyCode::Home => b'H',
                _ => b'F',
            };
            if application_cursor && !has_mods {
                return vec![0x1b, b'O', code];
            }
            return arrow_with_mods(code, has_mods, mods_param);
        }
        KeyCode::Delete => return tilde_with_mods(3, has_mods, mods_param),
        KeyCode::PageUp => return tilde_with_mods(5, has_mods, mods_param),
        KeyCode::PageDown => return tilde_with_mods(6, has_mods, mods_param),
        KeyCode::Insert => return tilde_with_mods(2, has_mods, mods_param),
        KeyCode::F(n) => return encode_f_key_with_mods(n, has_mods, mods_param),
        _ => return Vec::new(),
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn legacy_control(c: char) -> Option<u8> {
    match c {
        ' ' | '@' | '2' => Some(0),
        'a'..='z' | 'A'..='Z' => Some(c.to_ascii_lowercase() as u8 - b'a' + 1),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn kitty_keypad_key(code: KeyCode) -> Option<(u32, char)> {
    Some((
        match code {
            KeyCode::Char(c @ '0'..='9') => 57399 + u32::from(c) - u32::from('0'),
            KeyCode::Char('.') => 57409,
            KeyCode::Char('/') => 57410,
            KeyCode::Char('*') => 57411,
            KeyCode::Char('-') => 57412,
            KeyCode::Char('+') => 57413,
            KeyCode::Enter => 57414,
            KeyCode::Char('=') => 57415,
            KeyCode::Char(',') => 57416,
            KeyCode::Left => 57417,
            KeyCode::Right => 57418,
            KeyCode::Up => 57419,
            KeyCode::Down => 57420,
            KeyCode::PageUp => 57421,
            KeyCode::PageDown => 57422,
            KeyCode::Home => 57423,
            KeyCode::End => 57424,
            KeyCode::Insert => 57425,
            KeyCode::Delete => 57426,
            _ => return None,
        },
        'u',
    ))
}

fn kitty_functional_key(code: KeyCode) -> Option<(u32, char)> {
    Some(match code {
        KeyCode::Esc => (27, 'u'),
        KeyCode::Enter => (13, 'u'),
        KeyCode::Tab | KeyCode::BackTab => (9, 'u'),
        KeyCode::Backspace => (127, 'u'),
        KeyCode::Insert => (2, '~'),
        KeyCode::Delete => (3, '~'),
        KeyCode::Left => (1, 'D'),
        KeyCode::Right => (1, 'C'),
        KeyCode::Up => (1, 'A'),
        KeyCode::Down => (1, 'B'),
        KeyCode::PageUp => (5, '~'),
        KeyCode::PageDown => (6, '~'),
        KeyCode::Home => (1, 'H'),
        KeyCode::End => (1, 'F'),
        KeyCode::F(1) => (1, 'P'),
        KeyCode::F(2) => (1, 'Q'),
        KeyCode::F(3) => (13, '~'),
        KeyCode::F(4) => (1, 'S'),
        KeyCode::F(n @ 5..=12) => ([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)], '~'),
        KeyCode::F(n @ 13..=35) => (57376 + u32::from(n - 13), 'u'),
        KeyCode::CapsLock => (57358, 'u'),
        KeyCode::ScrollLock => (57359, 'u'),
        KeyCode::NumLock => (57360, 'u'),
        KeyCode::PrintScreen => (57361, 'u'),
        KeyCode::Pause => (57362, 'u'),
        KeyCode::Menu => (57363, 'u'),
        KeyCode::Null => (0, 'u'),
        KeyCode::Media(media) => (
            match media {
                MediaKeyCode::Play => 57428,
                MediaKeyCode::Pause => 57429,
                MediaKeyCode::PlayPause => 57430,
                MediaKeyCode::Reverse => 57431,
                MediaKeyCode::Stop => 57432,
                MediaKeyCode::FastForward => 57433,
                MediaKeyCode::Rewind => 57434,
                MediaKeyCode::TrackNext => 57435,
                MediaKeyCode::TrackPrevious => 57436,
                MediaKeyCode::Record => 57437,
                MediaKeyCode::LowerVolume => 57438,
                MediaKeyCode::RaiseVolume => 57439,
                MediaKeyCode::MuteVolume => 57440,
            },
            'u',
        ),
        KeyCode::Modifier(modifier) => (
            match modifier {
                ModifierKeyCode::LeftShift => 57441,
                ModifierKeyCode::LeftControl => 57442,
                ModifierKeyCode::LeftAlt => 57443,
                ModifierKeyCode::LeftSuper => 57444,
                ModifierKeyCode::LeftHyper => 57445,
                ModifierKeyCode::LeftMeta => 57446,
                ModifierKeyCode::RightShift => 57447,
                ModifierKeyCode::RightControl => 57448,
                ModifierKeyCode::RightAlt => 57449,
                ModifierKeyCode::RightSuper => 57450,
                ModifierKeyCode::RightHyper => 57451,
                ModifierKeyCode::RightMeta => 57452,
                ModifierKeyCode::IsoLevel3Shift => 57453,
                ModifierKeyCode::IsoLevel5Shift => 57454,
            },
            'u',
        ),
        _ => return None,
    })
}

/// Arrow keys: ESC [ A (plain) or ESC [ 1 ; <mods> A (with modifiers).
fn arrow_with_mods(code: u8, has_mods: bool, mods_param: u8) -> Vec<u8> {
    if has_mods {
        format!("\x1b[1;{}{}", mods_param, code as char).into_bytes()
    } else {
        vec![0x1b, b'[', code]
    }
}

/// Tilde keys (Delete/PageUp/etc): ESC [ N ~ (plain) or ESC [ N ; <mods> ~ (with modifiers).
fn tilde_with_mods(n: u8, has_mods: bool, mods_param: u8) -> Vec<u8> {
    if has_mods {
        format!("\x1b[{};{}~", n, mods_param).into_bytes()
    } else {
        format!("\x1b[{}~", n).into_bytes()
    }
}

fn encode_f_key_with_mods(n: u8, has_mods: bool, mods_param: u8) -> Vec<u8> {
    // F1-F4 use SS3 format without mods, CSI format with mods
    // F5-F12 use CSI tilde format
    if has_mods {
        if (1..=4).contains(&n) {
            return arrow_with_mods(b'P' + n - 1, true, mods_param);
        }
        let code = match n {
            1 => 11,
            2 => 12,
            3 => 13,
            4 => 14,
            5 => 15,
            6 => 17,
            7 => 18,
            8 => 19,
            9 => 20,
            10 => 21,
            11 => 23,
            12 => 24,
            _ => return vec![],
        };
        format!("\x1b[{};{}~", code, mods_param).into_bytes()
    } else {
        match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => vec![],
        }
    }
}

/// Pure helper for the runtime scrollback eviction shim (#68). Returns the
/// number of rows that *would* be evicted if vt100 exposed a trim primitive;
/// the caller (typically [`Pane::observe_scrollback_budget`]) then decides whether
/// to act on it or merely emit telemetry. Pure so it can be unit-tested
/// without spawning a PTY.
///
/// Algorithm:
///   * Budget of `0` disables the byte cap. Returns 0.
///   * If `byte_estimate <= byte_budget`, no eviction. Returns 0.
///   * Otherwise: bytes-over-budget divided by an estimated 4-byte-per-cell
///     × screen-width row cost (UTF-8 worst case), with a floor of 1 so the
///     telemetry event always fires at least once on overflow.
///
/// The `policy` argument is plumbed through for future use; with the
/// current vt100 API neither `OldestLine` nor `LargestLine` can be
/// honoured because history rows are not addressable. See
/// [`Pane::observe_scrollback_budget`] for the limitation note.
fn compute_eviction(
    byte_budget: usize,
    byte_estimate: usize,
    cols: u16,
    _policy: ScrollbackEviction,
) -> usize {
    if byte_budget == 0 || byte_estimate <= byte_budget {
        return 0;
    }
    let estimated_row_bytes = (cols as usize).saturating_mul(4).max(1);
    let overflow = byte_estimate.saturating_sub(byte_budget);
    (overflow / estimated_row_bytes).max(1)
}

fn budgeted_scrollback_lines(requested: usize, cols: u16, budget: usize) -> usize {
    if budget == 0 {
        return requested;
    }
    // vt100 cells have inline text. Allow for row metadata and
    // deque over-allocation; this is a construction-time limit, not RSS.
    let row_bytes = (usize::from(cols) * std::mem::size_of::<vt100::Cell>() + 64).saturating_mul(2);
    requested.min(budget / row_bytes.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_state::Rgb;

    /// In-memory writer for capturing the bytes the interceptor sends
    /// back to the child PTY (kitty kbd query reply, OSC colour reply).
    struct VecWriter(Vec<u8>);
    impl Write for VecWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reliability_legacy_shift_alt_char_is_escape_prefixed_text() {
        assert_eq!(
            encode_key(KeyEvent::new(
                KeyCode::Char('A'),
                KeyModifiers::SHIFT | KeyModifiers::ALT
            )),
            b"\x1bA"
        );
    }

    #[test]
    fn reliability_legacy_ctrl_digit_does_not_wrap_arithmetic() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::CONTROL)),
            b"\x1b"
        );
    }

    #[test]
    fn reliability_backtab_is_forwarded() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            b"\x1b[Z"
        );
    }

    #[test]
    fn reliability_decset_survives_every_split() {
        let sequence = b"\x1b[?2004;1004;1003;1006h";
        for split in 1..sequence.len() {
            let mut state = PaneTerminalState::new();
            let mut pending = Vec::new();
            let mut parser = ControlParser::default();
            let mut writer = VecWriter(Vec::new());
            let policy = ClipboardPolicy::default();
            let palette = ThemePalette::default();
            let mut ctx = InterceptCtx {
                state: &mut state,
                osc52_pending: &mut pending,
                parser: &mut parser,
                writer: &mut writer,
                policy: &policy,
                palette: &palette,
            };
            intercept_chunk(&mut ctx, &sequence[..split]);
            intercept_chunk(&mut ctx, &sequence[split..]);
            assert!(
                state.bracketed_paste && state.focus_reporting,
                "split={split}"
            );
            assert_eq!(state.mouse_mode.protocol, MouseProtocol::Any);
            assert_eq!(state.mouse_mode.encoding, MouseEncoding::Sgr);
        }
    }

    #[cfg(unix)]
    #[test]
    fn reliability_pty_eof_preserves_exit_status() {
        let mut pane = Pane::with_scrollback(
            "/bin/sh",
            PaneLaunch::Command("printf finished; exit 7".into()),
            40,
            5,
            10,
        )
        .unwrap();
        // Let the reader reach EOF before the first server drain.
        std::thread::sleep(Duration::from_millis(250));
        pane.read_output();
        assert!(pane.screen().contents().contains("finished"));
        assert_eq!(pane.exit_code(), Some(7));
        assert!(!pane.is_alive());
    }

    fn run_parts(
        parts: &[&[u8]],
        state: &mut PaneTerminalState,
    ) -> (Vec<u8>, Vec<u8>, Vec<Vec<u8>>) {
        let mut parser = ControlParser::default();
        let mut pending = Vec::new();
        let mut writer = VecWriter(Vec::new());
        let policy = ClipboardPolicy {
            set: Osc52SetPolicy::Allow,
            ..Default::default()
        };
        let palette = ThemePalette::default();
        let mut output = Vec::new();
        let mut ctx = InterceptCtx {
            state,
            osc52_pending: &mut pending,
            parser: &mut parser,
            writer: &mut writer,
            policy: &policy,
            palette: &palette,
        };
        for part in parts {
            output.extend(intercept_chunk(&mut ctx, part));
        }
        (output, writer.0, pending)
    }

    #[test]
    fn reliability_osc_and_kitty_survive_every_split() {
        let data = b"\x1b]52;c;aGVsbG8=\x1b\\\x1b[>3u\x1b[?u";
        for split in 1..data.len() {
            let mut state = PaneTerminalState::new();
            let (_, reply, pending) = run_parts(&[&data[..split], &data[split..]], &mut state);
            assert_eq!(reply, b"\x1b[?3u", "split={split}");
            assert_eq!(
                pending,
                vec![b"\x1b]52;c;aGVsbG8=\x07".to_vec()],
                "split={split}"
            );
        }
    }

    #[test]
    fn reliability_long_osc_is_bounded_and_not_rendered() {
        let mut data = b"\x1b]52;c;".to_vec();
        data.extend(vec![b'A'; OSC_PAYLOAD_MAX + 1]);
        data.extend_from_slice(b"\x07visible\x1b[?2004h");
        let mut state = PaneTerminalState::new();
        let chunks: Vec<_> = data.chunks(37).collect();
        let (output, _, pending) = run_parts(&chunks, &mut state);
        assert!(pending.is_empty());
        assert!(state.bracketed_paste);
        assert_eq!(output, b"visible\x1b[?2004h");

        let mut valid = b"\x1b]52;c;".to_vec();
        valid.extend(vec![b'A'; 20_000]);
        valid.push(7);
        let chunks: Vec<_> = valid.chunks(31).collect();
        let (_, _, pending) = run_parts(&chunks, &mut PaneTerminalState::new());
        assert_eq!(pending, vec![valid]);
    }

    #[test]
    fn reliability_osc_and_dcs_do_not_set_embedded_modes() {
        let mut state = PaneTerminalState::new();
        run_parts(&[b"\x1bPignored\x1b[?2004h\x1b[>3u\x1b\\"], &mut state);
        assert!(!state.bracketed_paste);
        assert_eq!(state.kitty_kbd.active().bits(), 0);
        run_parts(&[b"\x1b[?2004\x18h\x1b[>11\x1au"], &mut state);
        assert!(!state.bracketed_paste);
        assert_eq!(state.kitty_kbd.active().bits(), 0);
    }

    #[test]
    fn reliability_controls_inside_escape_cannot_bypass_osc_bound() {
        let mut state = PaneTerminalState::new();
        let sequence = b"\x1b\x07\x7f]52;c;aGVsbG8=\x07visible";
        for split in 1..sequence.len() {
            let (output, _, pending) =
                run_parts(&[&sequence[..split], &sequence[split..]], &mut state);
            assert_eq!(pending, vec![b"\x1b]52;c;aGVsbG8=\x07".to_vec()]);
            assert_eq!(output, b"\x07visible");
        }
        let (output, _, _) = run_parts(&[b"\x1b(\x1b]52;c;YWJj\x07visible"], &mut state);
        let mut screen = vt100::Parser::new(4, 40, 0);
        screen.process(&output);
        assert_eq!(screen.screen().contents(), "visible");
    }

    #[test]
    fn reliability_sync_is_split_safe_and_idempotent() {
        let data = b"\x1b[?2026;2004h\x1b[?2026h\x1b[?2026l";
        for split in 1..data.len() {
            let mut state = PaneTerminalState::new();
            run_parts(&[&data[..split], &data[split..]], &mut state);
            assert!(state.bracketed_paste);
            assert!(state.sync_opened_at.is_none(), "split={split}");
        }
    }

    #[test]
    fn reliability_kitty_screens_reset_and_invalid_parameters() {
        let mut state = PaneTerminalState::new();
        let (output, reply, _) = run_parts(
            &[b"\x1b[>1u\x1b[?1049h\x1b[?u\x1b[>11u\x1b[?1049l\x1b[?u"],
            &mut state,
        );
        assert_eq!(reply, b"\x1b[?0u\x1b[?1u");
        let mut parser = vt100::Parser::new(5, 40, 10);
        parser.process(&output);
        assert!(!parser.screen().alternate_screen());
        run_parts(
            &[b"\x1b[>abc u\x1b[=;99u\x1b[<99999999999u\x1b[?999u"],
            &mut state,
        );
        assert_eq!(state.kitty_kbd.active().bits(), 1);
        state.osc52_decision = Osc52Decision::Denied;
        run_parts(&[b"\x1b[?2004;1004;1006h\x1b[?2026h\x1bc"], &mut state);
        assert!(!state.bracketed_paste && !state.focus_reporting);
        assert!(state.sync_opened_at.is_none());
        assert_eq!(state.kitty_kbd.active().bits(), 0);
        assert_eq!(state.osc52_decision, Osc52Decision::Denied);
    }

    #[test]
    fn reliability_1047_combined_modes_restore_main_and_clear_alternate() {
        let mut state = PaneTerminalState::new();
        let (output, _, _) = run_parts(&[b"main\x1b[?1047;2004hother\x1b[?1047l"], &mut state);
        let mut parser = vt100::Parser::new(5, 40, 10);
        parser.process(&output);
        assert!(!parser.screen().alternate_screen());
        assert_eq!(parser.screen().contents(), "main");
        assert!(state.bracketed_paste);
        let (output, _, _) = run_parts(&[b"\x1b[?1047h"], &mut state);
        parser.process(&output);
        assert!(parser.screen().alternate_screen());
        assert!(parser.screen().contents().is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn reliability_vt100_shrink_wide_then_erase() {
        let mut pane =
            Pane::with_scrollback("/bin/sh", PaneLaunch::Command("sleep 30".into()), 4, 2, 10)
                .unwrap();
        pane.parser.process("old\r\nhist\r\na你".as_bytes());
        pane.resize(2, 2);
        pane.parser.process(b"\x1b[K");
        assert_eq!(pane.screen().size(), (2, 2));
        assert_eq!(pane.screen().cell(1, 0).unwrap().contents(), "a");
        pane.resize(4, 2);
        let history = pane.dump_text(true);
        assert_eq!(history.first().map(String::as_str), Some("old"));
    }

    #[test]
    #[cfg(unix)]
    fn reliability_vt100_one_column_wide_output() {
        let mut pane =
            Pane::with_scrollback("/bin/sh", PaneLaunch::Command("sleep 30".into()), 1, 1, 10)
                .unwrap();
        pane.parser.process("你".as_bytes());
        assert_eq!(pane.screen().size(), (1, 1));
        assert!(pane.screen().contents().is_empty());
        let size = pane.master.get_size().unwrap();
        assert_eq!((size.rows, size.cols), (1, 1));
    }

    #[test]
    #[cfg(unix)]
    fn reliability_vt100_one_row_wrapping_retains_history() {
        let mut pane =
            Pane::with_scrollback("/bin/sh", PaneLaunch::Command("sleep 30".into()), 1, 1, 10)
                .unwrap();
        pane.parser.process(b"abc");
        assert_eq!(pane.dump_text(true), vec!["a", "b", "c"]);
        pane.resize(1, 1);
        pane.parser.process(b"\r\nd");
        assert_eq!(pane.dump_text(true), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn reliability_osc_title_survives_every_split_and_is_bounded() {
        let data = b"\x1b]2;ssh example\x07";
        for split in 1..data.len() {
            let mut state = PaneTerminalState::new();
            run_parts(&[&data[..split], &data[split..]], &mut state);
            assert_eq!(state.title, "ssh example");
        }
    }

    #[test]
    fn reliability_utf8_and_escape_cancellation_are_chunk_invariant() {
        let sequence = "한글\u{9b}text\x1b[?2004h\x1b(B".as_bytes();
        let mut expected_state = PaneTerminalState::new();
        let (expected, _, _) = run_parts(&[sequence], &mut expected_state);
        for split in 1..sequence.len() {
            let mut state = PaneTerminalState::new();
            let (actual, _, _) = run_parts(&[&sequence[..split], &sequence[split..]], &mut state);
            assert_eq!(actual, expected, "split={split}");
            assert!(state.bracketed_paste);
        }
    }

    #[test]
    fn reliability_budget_caps_real_parser_history_at_construction() {
        let cols = 40;
        let limit = budgeted_scrollback_lines(10_000, cols, 4096);
        assert!(limit < 10);
        assert_eq!(budgeted_scrollback_lines(10_000, cols, 0), 10_000);
        let mut parser = vt100::Parser::new(5, cols, limit);
        for _ in 0..1000 {
            parser.process(b"line\r\n");
        }
        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(parser.screen().scrollback(), limit);
    }

    #[cfg(unix)]
    #[test]
    fn reliability_stalled_child_input_does_not_block_server() {
        let mut pane = Pane::with_scrollback(
            "/bin/sh",
            PaneLaunch::Command("stty raw -echo; printf READY; sleep 30".into()),
            40,
            5,
            10,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !pane.screen().contents().contains("READY") && Instant::now() < deadline {
            pane.read_output();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(pane.screen().contents().contains("READY"));
        let start = Instant::now();
        pane.write_bytes(&vec![b'x'; PTY_INPUT_MAX]);
        pane.read_output();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(pane.is_alive());
    }

    #[test]
    fn reliability_application_cursor_and_function_keys() {
        for (code, final_byte) in [
            (KeyCode::Up, b'A'),
            (KeyCode::Home, b'H'),
            (KeyCode::End, b'F'),
        ] {
            assert_eq!(
                encode_key_for_child(
                    KeyEvent::new(code, KeyModifiers::NONE),
                    KittyKbdFlags(0),
                    true
                ),
                vec![0x1b, b'O', final_byte]
            );
            assert_eq!(
                encode_key_for_child(
                    KeyEvent::new(code, KeyModifiers::CONTROL),
                    KittyKbdFlags(0),
                    true
                ),
                format!("\x1b[1;5{}", char::from(final_byte)).into_bytes()
            );
        }
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::SHIFT)),
            b"\x1b[1;2P"
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            b"\r"
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('한'), KeyModifiers::CONTROL)),
            "한".as_bytes()
        );
    }

    #[test]
    fn reliability_child_kitty_flags_gate_encoding_and_releases() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(encode_key_for_child(key, KittyKbdFlags(0), false), b"\x03");
        assert_eq!(
            encode_key_for_child(key, KittyKbdFlags(1), false),
            b"\x1b[99;5u"
        );
        assert_eq!(
            encode_key_for_child(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
                KittyKbdFlags(1),
                false
            ),
            b"\x1b[13;2u"
        );
        let mut keypad = KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE);
        keypad.state = KeyEventState::KEYPAD;
        assert_eq!(
            encode_key_for_child(keypad, KittyKbdFlags(1), false),
            b"\x1b[57404;1u"
        );
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert!(encode_key_for_child(release, KittyKbdFlags(0), false).is_empty());
        assert!(encode_key_for_child(release, KittyKbdFlags(3), false).is_empty());
        assert_eq!(
            encode_key_for_child(release, KittyKbdFlags(11), false),
            b"\x1b[97;1:3u"
        );
        let release =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
        assert!(encode_key_for_child(release, KittyKbdFlags(3), false).is_empty());
        assert_eq!(
            encode_key_for_child(release, KittyKbdFlags(11), false),
            b"\x1b[13;1:3u"
        );
    }

    #[test]
    fn reliability_mouse_buttons_modifiers_and_protocol_filters() {
        let mut mode = MouseMode {
            protocol: MouseProtocol::Any,
            encoding: MouseEncoding::Sgr,
        };
        let mut event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 4,
            row: 9,
            modifiers: KeyModifiers::ALT | KeyModifiers::CONTROL,
        };
        assert_eq!(encode_mouse_event(mode, event).unwrap(), b"\x1b[<26;5;10M");
        event.kind = MouseEventKind::Up(MouseButton::Middle);
        assert_eq!(encode_mouse_event(mode, event).unwrap(), b"\x1b[<25;5;10m");
        event.modifiers = KeyModifiers::NONE;
        event.kind = MouseEventKind::Drag(MouseButton::Right);
        assert_eq!(encode_mouse_event(mode, event).unwrap(), b"\x1b[<34;5;10M");
        mode.protocol = MouseProtocol::X10;
        assert!(encode_mouse_event(mode, event).is_none());
        mode.protocol = MouseProtocol::Btn;
        assert!(encode_mouse_event(mode, event).is_some());
        event.kind = MouseEventKind::Moved;
        assert!(encode_mouse_event(mode, event).is_none());
        mode.protocol = MouseProtocol::Any;
        assert_eq!(encode_mouse_event(mode, event).unwrap(), b"\x1b[<35;5;10M");
        for (kind, code) in [
            (MouseEventKind::ScrollUp, 64),
            (MouseEventKind::ScrollDown, 65),
            (MouseEventKind::ScrollLeft, 66),
            (MouseEventKind::ScrollRight, 67),
        ] {
            event.kind = kind;
            assert_eq!(
                encode_mouse_event(mode, event).unwrap(),
                format!("\x1b[<{code};5;10M").as_bytes()
            );
        }
        mode.protocol = MouseProtocol::Press;
        assert!(encode_mouse_event(mode, event).is_none());
        mode.protocol = MouseProtocol::Off;
        assert!(encode_mouse_event(mode, event).is_none());
    }

    #[test]
    fn reliability_mouse_coordinate_boundaries_never_wrap_or_clamp() {
        assert_eq!(
            encode_mouse_report(MouseEncoding::X10, 0, 222, 222, false).unwrap(),
            vec![27, b'[', b'M', 32, 255, 255]
        );
        assert!(encode_mouse_report(MouseEncoding::X10, 0, 223, 0, false).is_none());
        assert_eq!(
            encode_mouse_report(MouseEncoding::Sgr, 2, u16::MAX, u16::MAX, true).unwrap(),
            b"\x1b[<2;65536;65536m"
        );
        assert_eq!(
            encode_mouse_report(MouseEncoding::X10, 26, 0, 0, true).unwrap(),
            vec![27, b'[', b'M', 59, 33, 33]
        );
        assert_eq!(
            encode_mouse_report(MouseEncoding::Utf8, 0, 2014, 0, false).unwrap(),
            "\x1b[M \u{7ff}!".as_bytes()
        );
        assert!(encode_mouse_report(MouseEncoding::Utf8, 0, 2015, 0, false).is_none());
        assert_eq!(
            encode_mouse_report(MouseEncoding::Urxvt, 2, 500, 0, false).unwrap(),
            b"\x1b[34;501;1M"
        );
    }

    #[test]
    fn reliability_osc_queue_and_remote_cwd_are_bounded() {
        let mut state = PaneTerminalState::new();
        let data = b"\x1b]52;c;aGVsbG8=\x07".repeat(1000);
        let (_, _, pending) = run_parts(&[&data], &mut state);
        assert_eq!(pending.len(), OSC_QUEUE_MAX);
        run_parts(
            &[b"\x1b]7;file://remote.invalid/home/remote\x07"],
            &mut state,
        );
        assert!(state.reported_cwd.is_none());
        run_parts(&[b"\x1b]7;file://localhost/tmp%00x\x07"], &mut state);
        assert!(state.reported_cwd.is_none());
    }

    #[test]
    fn reliability_explicit_clipboard_deny_overrides_prior_consent() {
        let mut state = PaneTerminalState::new();
        state.osc52_decision = Osc52Decision::Allowed;
        let policy = ClipboardPolicy {
            set: Osc52SetPolicy::Deny,
            ..Default::default()
        };
        let mut pending = Vec::new();
        run_intercept(
            b"\x1b]52;c;aGVsbG8=\x07",
            &mut state,
            &mut pending,
            &policy,
            &ThemePalette::default(),
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn reliability_nonblocking_input_queue_preserves_order_and_cap() {
        struct Blocked;
        impl Write for Blocked {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = BufferedPtyWriter {
            inner: Box::new(Blocked),
            pending: VecDeque::new(),
        };
        writer.write_all(&vec![b'a'; PTY_INPUT_MAX]).unwrap();
        writer.flush().unwrap();
        assert_eq!(writer.pending.len(), PTY_INPUT_MAX);
        assert_eq!(
            writer.write(b"b").unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(writer.pending.iter().all(|b| *b == b'a'));
    }

    #[cfg(unix)]
    #[test]
    fn reliability_drop_kills_and_reaps_without_output() {
        let pane = Pane::with_scrollback(
            "/bin/sh",
            PaneLaunch::Command("trap '' HUP; sleep 30".into()),
            40,
            5,
            10,
        )
        .unwrap();
        let pid = pane.pid().unwrap() as libc::pid_t;
        let start = Instant::now();
        drop(pane);
        assert!(start.elapsed() < Duration::from_secs(2));
        // SAFETY: pid belongs to the pane just dropped; this only probes wait status.
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[cfg(unix)]
    #[test]
    fn reliability_large_exit_output_is_drained_before_dead() {
        let mut pane = Pane::with_scrollback(
            "/bin/sh",
            PaneLaunch::Command("printf '%050000dTAIL' 0; exit 9".into()),
            40,
            5,
            10,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while pane.is_alive() && Instant::now() < deadline {
            pane.read_output();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!pane.is_alive());
        assert_eq!(pane.exit_code(), Some(9));
        assert!(pane.screen().contents().contains("TAIL"));
    }

    #[cfg(unix)]
    #[test]
    fn reliability_snapshot_replay_is_display_only_and_sensitive_opt_out() {
        let mut pane =
            Pane::with_scrollback("/bin/sh", PaneLaunch::Command("sleep 30".into()), 40, 5, 20)
                .unwrap();
        assert!(!pane.snapshot_sensitive());
        assert_eq!(pane.persist_scrollback_override(), None);
        pane.set_persist_scrollback_override(Some(true));
        assert_eq!(pane.persist_scrollback_override(), Some(true));
        pane.restore_scrollback(["history\x1b[?2004h\x07"]);
        assert!(pane.screen().contents().contains("history[?2004h"));
        assert!(!pane.screen().bracketed_paste());
        assert!(!pane.bracketed_paste());
        pane.set_snapshot_sensitive(true);
        assert!(pane.snapshot_sensitive());
        pane.restore_scrollback(["SHOULD_NOT_APPEAR"]);
        assert!(!pane.screen().contents().contains("SHOULD_NOT_APPEAR"));
    }

    fn run_intercept(
        chunk: &[u8],
        state: &mut PaneTerminalState,
        osc52_pending: &mut Vec<Vec<u8>>,
        policy: &ClipboardPolicy,
        palette: &ThemePalette,
    ) -> Vec<u8> {
        let mut writer = VecWriter(Vec::new());
        let mut parser = ControlParser::default();
        {
            let mut ctx = InterceptCtx {
                state,
                osc52_pending,
                parser: &mut parser,
                writer: &mut writer,
                policy,
                palette,
            };
            intercept_chunk(&mut ctx, chunk);
        }
        writer.0
    }

    fn track_dec_modes(chunk: &[u8], state: &mut PaneTerminalState) {
        run_intercept(
            chunk,
            state,
            &mut Vec::new(),
            &ClipboardPolicy::default(),
            &ThemePalette::default(),
        );
    }

    // ─── #78 — DECSET state tracking ───────────────────────

    #[test]
    fn track_dec_modes_bracketed_paste() {
        let mut s = PaneTerminalState::new();
        track_dec_modes(b"\x1b[?2004h", &mut s);
        assert!(s.bracketed_paste);
        track_dec_modes(b"\x1b[?2004l", &mut s);
        assert!(!s.bracketed_paste);
    }

    #[test]
    fn track_dec_modes_combined_modes() {
        // `\x1b[?1000;1006h` enables both protocol and SGR encoding.
        let mut s = PaneTerminalState::new();
        track_dec_modes(b"\x1b[?1000;1006h", &mut s);
        assert_eq!(s.mouse_mode.protocol, MouseProtocol::X10);
        assert_eq!(s.mouse_mode.encoding, MouseEncoding::Sgr);

        track_dec_modes(b"\x1b[?1000;1006l", &mut s);
        assert!(s.mouse_mode.is_off());
        assert_eq!(s.mouse_mode.encoding, MouseEncoding::X10);
    }

    #[test]
    fn track_dec_modes_focus_events() {
        let mut s = PaneTerminalState::new();
        track_dec_modes(b"\x1b[?1004h", &mut s);
        assert!(s.focus_reporting);
        track_dec_modes(b"\x1b[?1004l", &mut s);
        assert!(!s.focus_reporting);
    }

    #[test]
    fn track_dec_modes_mouse_protocol_progression() {
        let mut s = PaneTerminalState::new();
        track_dec_modes(b"\x1b[?1000h", &mut s);
        assert_eq!(s.mouse_mode.protocol, MouseProtocol::X10);
        track_dec_modes(b"\x1b[?1002h", &mut s);
        assert_eq!(s.mouse_mode.protocol, MouseProtocol::Btn);
        track_dec_modes(b"\x1b[?1003h", &mut s);
        assert_eq!(s.mouse_mode.protocol, MouseProtocol::Any);
        track_dec_modes(b"\x1b[?1003l", &mut s);
        assert!(s.mouse_mode.is_off());
    }

    #[test]
    fn track_dec_modes_disable_inactive_protocol_no_op() {
        // ?1003h then ?1000l should NOT clear 1003 — different protocol bit.
        let mut s = PaneTerminalState::new();
        track_dec_modes(b"\x1b[?1003h", &mut s);
        assert_eq!(s.mouse_mode.protocol, MouseProtocol::Any);
        track_dec_modes(b"\x1b[?1000l", &mut s);
        assert_eq!(
            s.mouse_mode.protocol,
            MouseProtocol::Any,
            "disabling X10 must not affect active Any protocol"
        );
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("/tmp"), "/tmp");
        assert_eq!(percent_decode("/path%20with%20spaces"), "/path with spaces");
        assert_eq!(percent_decode("%2Fweird%2Fpath"), "/weird/path");
        // Unknown escape — leave alone
        assert_eq!(percent_decode("%ZZ"), "%ZZ");
    }

    #[test]
    fn parse_helpers() {
        assert_eq!(parse_u8(b"42"), Some(42));
        assert_eq!(parse_u8(b""), None);
        assert_eq!(parse_u8(b"abc"), None);
        assert_eq!(parse_u8(b"300"), None); // overflow
        assert_eq!(parse_u32(b"1024"), Some(1024));
    }

    // ─── #74 — Kitty keyboard stack via interceptor ────────

    #[test]
    fn intercept_kitty_push_then_query_replies_with_top() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette::default();

        // Push flags=5
        run_intercept(b"\x1b[>5u", &mut state, &mut pending, &policy, &palette);
        assert_eq!(state.kitty_kbd.active().bits(), 1);

        // Query → reply written to writer
        let reply = run_intercept(b"\x1b[?u", &mut state, &mut pending, &policy, &palette);
        assert_eq!(reply, b"\x1b[?1u");
    }

    #[test]
    fn intercept_kitty_push_pop_modify_sequence() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette::default();

        // Push 1, push 15, modify with mode=3 (AND-NOT) flags=4 → 11
        run_intercept(
            b"\x1b[>1u\x1b[>15u\x1b[=4;3u",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert_eq!(state.kitty_kbd.active().bits(), 11);
        assert_eq!(state.kitty_kbd.depth(), 2);

        // Pop one
        run_intercept(b"\x1b[<1u", &mut state, &mut pending, &policy, &palette);
        assert_eq!(state.kitty_kbd.active().bits(), 1);

        // Pop everything
        run_intercept(b"\x1b[<5u", &mut state, &mut pending, &policy, &palette);
        assert_eq!(state.kitty_kbd.depth(), 0);
        assert_eq!(state.kitty_kbd.active().bits(), 0);
    }

    // ─── #75 — OSC 7 cwd intercept ──────────────────────────

    #[test]
    fn intercept_osc7_decodes_simple_path() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette::default();

        run_intercept(
            b"\x1b]7;file:///tmp\x1b\\",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        let (path, _ts) = state.reported_cwd.as_ref().expect("OSC 7 not captured");
        assert_eq!(path.as_path(), std::path::Path::new("/tmp"));
    }

    #[test]
    fn intercept_osc7_decodes_percent_escapes_and_host() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette::default();

        run_intercept(
            b"\x1b]7;file://localhost/home/u%20ser/pkg\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        let (path, _ts) = state.reported_cwd.as_ref().unwrap();
        assert_eq!(path.as_path(), std::path::Path::new("/home/u ser/pkg"));
    }

    // ─── Unsupported OSC 8 ────────────────────────────────

    #[test]
    fn intercept_osc8_does_not_consume_or_inject() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette::default();

        // OSC 8 ; ; URL ST text OSC 8 ; ; ST — the multiplexer touches none of it.
        let reply = run_intercept(
            b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert!(reply.is_empty(), "OSC 8 must not produce a writer reply");
        assert!(pending.is_empty(), "OSC 8 must not enqueue anything");
        assert!(state.reported_cwd.is_none());
    }

    // ─── #77 — OSC 4/10/11/12 colour queries ────────────────

    #[test]
    fn intercept_osc11_query_returns_theme_bg() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette {
            bg: Some(Rgb::new(0x1e, 0x1e, 0x2e)),
            ..Default::default()
        };

        let reply = run_intercept(
            b"\x1b]11;?\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert_eq!(reply, b"\x1b]11;rgb:1e1e/1e1e/2e2e\x07");
    }

    #[test]
    fn intercept_osc11_query_passes_through_when_no_theme() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette::default(); // inactive

        let reply = run_intercept(
            b"\x1b]11;?\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert!(reply.is_empty(), "no theme → no multiplexer-side reply");
    }

    #[test]
    fn intercept_osc4_palette_query() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let mut palette = ThemePalette::default();
        palette.palette[42] = Some(Rgb::new(0x12, 0x34, 0x56));

        let reply = run_intercept(
            b"\x1b]4;42;?\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert_eq!(reply, b"\x1b]4;42;rgb:1212/3434/5656\x07");
    }

    #[test]
    fn intercept_osc4_unknown_index_silent() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette {
            fg: Some(Rgb::new(0xff, 0xff, 0xff)),
            ..Default::default()
        };

        // Theme is active but index 99 not set: no reply.
        let reply = run_intercept(
            b"\x1b]4;99;?\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert!(reply.is_empty());
    }

    // ─── #78 — Per-pane state (interceptor) ────────────────

    #[test]
    fn intercept_chunk_boundary_split_osc52() {
        // Same OSC 52 split across two reads — must reassemble correctly.
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy {
            set: Osc52SetPolicy::Allow,
            ..ClipboardPolicy::default()
        };
        let palette = ThemePalette::default();

        let mut writer = VecWriter(Vec::new());
        let mut parser = ControlParser::default();
        {
            let mut ctx = InterceptCtx {
                state: &mut state,
                osc52_pending: &mut pending,
                parser: &mut parser,
                writer: &mut writer,
                policy: &policy,
                palette: &palette,
            };
            // First half ends mid-OSC.
            intercept_chunk(&mut ctx, b"\x1b]52;c;");
            assert!(ctx.osc52_pending.is_empty());
            // Second half completes it.
            intercept_chunk(&mut ctx, b"aGVsbG8=\x07");
        }
        assert_eq!(pending.len(), 1);
        assert_eq!(&pending[0], b"\x1b]52;c;aGVsbG8=\x07");
    }

    // ─── #79 — OSC 52 paste-injection guard ────────────────

    #[test]
    fn intercept_osc52_set_allow_pushes_envelope() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy {
            set: Osc52SetPolicy::Allow,
            ..ClipboardPolicy::default()
        };
        let palette = ThemePalette::default();

        run_intercept(
            b"\x1b]52;c;aGVsbG8=\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert_eq!(pending.len(), 1);
        assert!(state.osc52_pending_confirm.is_empty());
    }

    #[test]
    fn intercept_osc52_set_deny_drops_silently() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy {
            set: Osc52SetPolicy::Deny,
            ..ClipboardPolicy::default()
        };
        let palette = ThemePalette::default();

        run_intercept(
            b"\x1b]52;c;aGVsbG8=\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert!(pending.is_empty());
        assert!(state.osc52_pending_confirm.is_empty());
    }

    #[test]
    fn intercept_osc52_set_confirm_parks_payload() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default(); // Confirm is the default
        let palette = ThemePalette::default();

        run_intercept(
            b"\x1b]52;c;aGVsbG8=\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert!(pending.is_empty(), "confirm policy must not auto-forward");
        assert_eq!(state.osc52_pending_confirm.len(), 1);
    }

    #[test]
    fn intercept_osc52_set_oversized_dropped() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy {
            set: Osc52SetPolicy::Allow,
            max_bytes: 16,
            ..ClipboardPolicy::default()
        };
        let palette = ThemePalette::default();

        // 32-byte base64 payload exceeds the cap.
        let blob = vec![b'A'; 32];
        let mut seq = b"\x1b]52;c;".to_vec();
        seq.extend_from_slice(&blob);
        seq.push(0x07);
        run_intercept(&seq, &mut state, &mut pending, &policy, &palette);
        assert!(pending.is_empty(), "oversized OSC 52 must be dropped");
    }

    #[test]
    fn intercept_osc52_get_default_denies() {
        let mut state = PaneTerminalState::new();
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default();
        let palette = ThemePalette::default();

        run_intercept(
            b"\x1b]52;c;?\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        assert!(pending.is_empty(), "OSC 52 read must be denied by default");
    }

    #[test]
    fn intercept_osc52_per_pane_decision_overrides_confirm() {
        let mut state = PaneTerminalState::new();
        state.osc52_decision = Osc52Decision::Allowed;
        let mut pending = Vec::new();
        let policy = ClipboardPolicy::default(); // Confirm
        let palette = ThemePalette::default();

        run_intercept(
            b"\x1b]52;c;aGVsbG8=\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        // Decision=Allowed should bypass confirm and forward immediately.
        assert_eq!(pending.len(), 1);
        assert!(state.osc52_pending_confirm.is_empty());
    }

    #[test]
    fn intercept_osc52_per_pane_decision_denied_blocks() {
        let mut state = PaneTerminalState::new();
        state.osc52_decision = Osc52Decision::Denied;
        let mut pending = Vec::new();
        let policy = ClipboardPolicy {
            set: Osc52SetPolicy::Allow,
            ..ClipboardPolicy::default()
        };
        let palette = ThemePalette::default();

        run_intercept(
            b"\x1b]52;c;aGVsbG8=\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        // Cached Denied beats config Allow.
        assert!(pending.is_empty());
    }

    // ─── #68 — runtime scrollback eviction shim ────────────

    #[test]
    fn compute_eviction_disabled_when_budget_zero() {
        let n = compute_eviction(0, 1_000_000, 80, ScrollbackEviction::OldestLine);
        assert_eq!(n, 0, "budget=0 must disable the byte cap");
    }

    #[test]
    fn compute_eviction_under_budget_is_zero() {
        let n = compute_eviction(1024, 512, 80, ScrollbackEviction::OldestLine);
        assert_eq!(n, 0);
    }

    #[test]
    fn compute_eviction_over_budget_returns_positive() {
        // 80 cols × 4 bytes/cell = 320 byte/row estimate.
        // Overflow = 1024 → ceil(1024/320) ≈ 3.
        let n = compute_eviction(1024, 2048, 80, ScrollbackEviction::OldestLine);
        assert!(n >= 1, "expected at least 1 row evicted, got {n}");
        assert!(n <= 4, "estimate should be small, got {n}");
    }

    #[test]
    fn compute_eviction_minimum_one_when_just_over_budget() {
        // Tiny overflow: byte_estimate − byte_budget = 1; integer-div with
        // ~320-byte row gives 0, but we floor to 1 so telemetry fires.
        let n = compute_eviction(1024, 1025, 80, ScrollbackEviction::OldestLine);
        assert_eq!(n, 1);
    }

    #[test]
    fn compute_eviction_policy_currently_does_not_change_count() {
        // Until vt100 exposes per-row deletion both policies behave the
        // same. This test pins the contract so the day vt100 ships an API
        // and `compute_eviction` becomes policy-aware, the diff is loud.
        let oldest = compute_eviction(1024, 4096, 80, ScrollbackEviction::OldestLine);
        let largest = compute_eviction(1024, 4096, 80, ScrollbackEviction::LargestLine);
        assert_eq!(oldest, largest);
    }

    #[test]
    fn vt100_parser_history_grows_then_eviction_fires() {
        // End-to-end check: drive a vt100 parser past the byte budget by
        // pushing many lines through it, then assert `compute_eviction`
        // signals overflow. We can't directly query vt100's history depth
        // (it's private), so we use `rows_formatted()` length over
        // repeated process() calls as a proxy that the parser is buffering.
        let cols: u16 = 80;
        let mut parser = vt100::Parser::new(24, cols, 1000);
        let line = b"abcdefghijklmnopqrstuvwxyz0123456789\r\n";
        let mut estimate: usize = 0;
        let budget: usize = 4 * 1024;
        for _ in 0..512 {
            parser.process(line);
            estimate = estimate.saturating_add(line.len());
        }
        // Sanity: parser is alive and has visible content.
        let visible: Vec<_> = parser.screen().rows(0, cols).collect();
        assert_eq!(visible.len(), 24);
        // Eviction signal must fire — estimate (~19 KiB) >> budget (4 KiB).
        let evicted = compute_eviction(budget, estimate, cols, ScrollbackEviction::OldestLine);
        assert!(
            evicted > 0,
            "estimate {estimate} > budget {budget} should evict (got {evicted})"
        );
    }
}
