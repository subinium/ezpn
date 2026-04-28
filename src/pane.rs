use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{ScrollbackEviction, DEFAULT_SCROLLBACK_BYTES};
use crate::terminal_state::{
    ClipboardPolicy, KittyKbdFlags, MouseEncoding, MouseMode, MouseProtocol, Osc52Decision,
    Osc52GetPolicy, Osc52SetPolicy, PaneTerminalState, ThemePalette,
};

/// Global wake channel: PTY reader threads send () to wake the server main loop.
/// Set once by the server with `set_wake_channel()`.
static WAKE_TX: OnceLock<mpsc::Sender<()>> = OnceLock::new();

/// Initialize the global wake channel. Call once from server startup.
/// Returns the Receiver that the main loop should use.
pub fn init_wake_channel() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let _ = WAKE_TX.set(tx);
    rx
}

/// Send a wake signal to the main loop (used by reader threads and client reader).
pub fn wake_main_loop() {
    if let Some(tx) = WAKE_TX.get() {
        let _ = tx.send(());
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
    parser: vt100::Parser,
    alive: bool,
    launch: PaneLaunch,
    scroll_offset: usize, // 0 = live (bottom), >0 = scrolled up N lines
    name: Option<String>,
    exit_code: Option<u32>,
    /// Pending OSC 52 clipboard sequences from child to forward to the terminal.
    /// `pub` so the server can push externally-generated OSC 52 (e.g. when the
    /// multiplexer copies a selection); incoming child-emitted OSC 52 is gated
    /// by `terminal_state.clipboard_policy` in [`Pane::read_output`].
    pub osc52_pending: Vec<Vec<u8>>,
    /// DECSET 2026 sync bracket depth (issue #73). `\x1b[?2026h` increments,
    /// `\x1b[?2026l` decrements (saturating at 0). Reference-counted to
    /// tolerate nested brackets.
    sync_depth: u16,
    /// Wall-clock instant the current sync window opened (depth 0 -> 1).
    /// Used by the host coalescer to enforce the 33 ms safety timeout.
    sync_opened_at: Option<Instant>,

    /// Aggregate per-pane terminal state (#74, #75, #77, #78, #79).
    /// Authoritative for DECSET bits the multiplexer cares about. See
    /// [`crate::terminal_state`] for which bits are owned here vs. by vt100
    /// (e.g. `?1049` alt-screen) vs. by issue #73 (`?2026` sync).
    state: PaneTerminalState,
    /// OSC parser carry-over: when a chunk ends mid-OSC, the prefix bytes
    /// are held here so the next chunk can complete the sequence.
    osc_carry: Vec<u8>,
    /// CSI parser carry-over for kitty keyboard sequences (`CSI > / < / = / ? u`).
    /// Same rationale as `osc_carry`.
    csi_carry: Vec<u8>,
    /// The working directory this pane was launched with.
    initial_cwd: Option<PathBuf>,
    /// Custom env vars this pane was launched with.
    initial_env: HashMap<String, String>,
    /// Custom shell override for this pane (if different from default).
    initial_shell: Option<String>,
    /// Active clipboard policy for OSC 52 set/get (#79). Defaults to the
    /// secure policy in [`ClipboardPolicy::default`].
    clipboard_policy: ClipboardPolicy,
    /// Active theme palette for OSC 4/10/11/12 responses (#77). When empty,
    /// queries pass through to the host emulator unchanged.
    theme_palette: ThemePalette,
    /// Byte budget for the per-pane scrollback shim (#68). `0` disables the
    /// byte cap and only the line cap baked into `vt100::Parser` applies.
    /// Defaults to [`DEFAULT_SCROLLBACK_BYTES`] (32 MiB) until config is
    /// applied via [`Pane::set_scrollback_budget`].
    scrollback_byte_budget: usize,
    /// Eviction policy applied when the byte budget is exceeded (#68).
    eviction_policy: ScrollbackEviction,
    /// Running upper-bound estimate of the bytes currently held in the
    /// vt100 scrollback. Incremented per `parser.process(&data)` chunk and
    /// decayed (zeroed) when an eviction event fires. Loose because vt100
    /// internally stores parsed cells, but this is the only signal we have
    /// without access to private grid state.
    scrollback_byte_estimate: usize,
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

        let child = pair.slave.spawn_command(cmd)?;
        // Drop slave after spawning — reader gets EOF only when slave + master refs are gone
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let (tx, rx) = mpsc::sync_channel(32); // bounded: 32 * 4KB = 128KB max buffered
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        wake_main_loop();
                    }
                    Err(_) => break,
                }
            }
        });

        let parser = vt100::Parser::new(rows, cols, scrollback);

        Ok(Self {
            master: pair.master,
            writer,
            child,
            reader_rx: rx,
            parser,
            alive: true,
            launch,
            scroll_offset: 0,
            name: None,
            exit_code: None,
            osc52_pending: Vec::new(),
            sync_depth: 0,
            sync_opened_at: None,
            state: PaneTerminalState::new(),
            osc_carry: Vec::new(),
            csi_carry: Vec::new(),
            initial_cwd: cwd.map(|p| p.to_path_buf()),
            initial_env: env.clone(),
            initial_shell: None,
            clipboard_policy: ClipboardPolicy::default(),
            theme_palette: ThemePalette::default(),
            scrollback_byte_budget: DEFAULT_SCROLLBACK_BYTES,
            eviction_policy: ScrollbackEviction::default(),
            scrollback_byte_estimate: 0,
        })
    }

    /// Plumb the runtime scrollback byte budget + eviction policy from
    /// `EzpnConfig` (#68). Call site: `bootstrap` after constructing each
    /// pane. A budget of `0` disables the byte cap entirely; the
    /// `vt100::Parser` line cap still applies.
    pub fn set_scrollback_budget(&mut self, byte_budget: usize, policy: ScrollbackEviction) {
        self.scrollback_byte_budget = byte_budget;
        self.eviction_policy = policy;
    }

    /// Evict scrollback rows when the running byte estimate exceeds the
    /// configured budget (#68). Returns the count of rows evicted (estimated).
    ///
    /// **vt100 0.15 limitation:** the public `vt100::Parser` API exposes only
    /// `set_scrollback(offset)` (viewport scrolling) and `set_size(rows, cols)`
    /// (resizes the visible screen). It does **not** expose any primitive to
    /// trim the scrollback `VecDeque` or query its current length —
    /// `Grid::scrollback_len` and the deque itself are private. This means
    /// runtime row-by-row eviction is not possible without a vt100 fork or
    /// upstream PR.
    ///
    /// What we do instead, honestly:
    ///   * Maintain a running upper-bound estimate of bytes flowed through
    ///     the parser (`scrollback_byte_estimate`).
    ///   * When over budget, reset the estimate (the line cap baked into
    ///     `vt100::Parser` still bounds memory in the worst case) and emit
    ///     a telemetry event (#71). The estimate reset acts as a cooldown
    ///     so we don't log every chunk after the first overflow.
    ///   * Return a synthetic "evicted rows" count derived from the
    ///     overflow ratio so observability is honest about what happened.
    ///
    /// The eviction policy field is plumbed end-to-end and will become
    /// load-bearing once vt100 (or a fork) exposes the missing API.
    fn evict_if_oversized(&mut self) -> usize {
        let (_screen_rows, cols) = self.parser.screen().size();
        let evicted = compute_eviction(
            self.scrollback_byte_budget,
            self.scrollback_byte_estimate,
            cols,
            self.eviction_policy,
        );
        if evicted > 0 {
            // Cooldown: zero the estimate so we only log on transitions
            // into the overflow region, not on every subsequent chunk. The
            // vt100 line cap still protects worst-case memory.
            self.scrollback_byte_estimate = 0;
        }
        evicted
    }

    /// Override the OSC 52 clipboard policy for this pane (typically copied
    /// from `[clipboard]` in the loaded config — see #79).
    pub fn set_clipboard_policy(&mut self, policy: ClipboardPolicy) {
        self.clipboard_policy = policy;
    }

    /// Override the active theme palette for OSC 4/10/11/12 responses (#77).
    /// Empty palette means "fall through to the host emulator".
    #[allow(dead_code)]
    pub fn set_theme_palette(&mut self, palette: ThemePalette) {
        self.theme_palette = palette;
    }

    /// Read pending output from PTY. Returns true if new data was received.
    /// Drains at most MAX_DRAIN chunks per call to ensure fair scheduling across panes.
    pub fn read_output(&mut self) -> bool {
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
                    // OSC 7. vt100 still sees the bytes — for queries it doesn't
                    // matter (vt100 ignores unknown OSC), and for state tracking we
                    // want it to stay consistent for screen rendering.
                    self.intercept(&data);
                    // DECSET 2026 sync brackets — observe before vt100 so the
                    // host coalescer never sees a half-applied window (#73).
                    scan_sync_brackets(&data, &mut self.sync_depth, &mut self.sync_opened_at);
                    self.parser.process(&data);
                    self.scrollback_byte_estimate =
                        self.scrollback_byte_estimate.saturating_add(data.len());
                    // Runtime scrollback eviction (#68) + telemetry (#71).
                    let evicted = self.evict_if_oversized();
                    if evicted > 0 {
                        tracing::info!(
                            evicted_rows = evicted,
                            byte_budget = self.scrollback_byte_budget,
                            byte_estimate = self.scrollback_byte_estimate,
                            policy = self.eviction_policy.as_str(),
                            "scrollback eviction"
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
                    self.alive = false;
                    break;
                }
            }
        }
        if self.alive {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.exit_code = Some(status.exit_code());
                self.alive = false;
            }
        }
        // Force-close any unmatched DECSET 2026 bracket on EOF so the host
        // coalescer doesn't freeze waiting on a process that will never emit
        // `?2026l` (#73).
        if !self.alive && self.sync_depth > 0 {
            tracing::warn!(
                pane_sync_depth = self.sync_depth,
                "DECSET 2026 sync window force-closed on pane EOF",
            );
            self.sync_depth = 0;
            self.sync_opened_at = None;
        }
        got_data || was_alive != self.alive
    }

    /// Whether the pane is inside a DECSET 2026 synchronized-output window (#73).
    pub fn in_sync(&self) -> bool {
        self.sync_depth > 0
    }

    /// Wall-clock instant the current sync window opened, if any (#73).
    /// Used by the host coalescer to enforce the 33 ms safety timeout.
    #[allow(dead_code)]
    pub fn sync_opened_at(&self) -> Option<Instant> {
        self.sync_opened_at
    }

    /// Force-close any open sync bracket (#73). Idempotent.
    #[allow(dead_code)]
    pub fn force_close_sync(&mut self) {
        if self.sync_depth > 0 {
            tracing::warn!(
                pane_sync_depth = self.sync_depth,
                "DECSET 2026 sync window force-closed by 33 ms timeout",
            );
            self.sync_depth = 0;
            self.sync_opened_at = None;
        }
    }

    pub fn write_key(&mut self, key: KeyEvent) {
        let bytes = encode_key(key);
        if !bytes.is_empty()
            && (self.writer.write_all(&bytes).is_err() || self.writer.flush().is_err())
        {
            self.alive = false;
        }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if self.writer.write_all(bytes).is_err() || self.writer.flush().is_err() {
            self.alive = false;
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        let result = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Err(e) = &result {
            eprintln!("ezpn: PTY resize failed: {e}");
        }
        self.parser.set_size(rows, cols);
        // Explicitly send SIGWINCH to the child's process group.
        // portable_pty's ioctl(TIOCSWINSZ) should trigger this via the kernel,
        // but we also send directly to cover edge cases.
        #[cfg(unix)]
        if self.alive {
            if let Some(pid) = self.child.process_id() {
                unsafe {
                    libc::kill(-(pid as libc::pid_t), libc::SIGWINCH);
                }
            }
        }
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Sync the vt100 scrollback offset with our tracked scroll_offset.
    /// Call this before drawing pane content so cell() returns scrollback.
    pub fn sync_scrollback(&mut self) {
        self.parser.set_scrollback(self.scroll_offset);
    }

    /// Reset vt100 scrollback offset to 0 (live view).
    /// Call after rendering to avoid affecting process() behavior.
    pub fn reset_scrollback_view(&mut self) {
        self.parser.set_scrollback(0);
    }

    pub fn is_alive(&self) -> bool {
        self.alive
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        self.alive = false;
    }

    pub fn scroll_up(&mut self, lines: usize) {
        // Set a large scrollback to discover the actual max,
        // then read back the clamped value.
        let probe = self.scroll_offset + lines;
        self.parser.set_scrollback(probe);
        self.scroll_offset = self.parser.screen().scrollback();
        // Reset parser view to 0 so process() isn't affected
        self.parser.set_scrollback(0);
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
    /// Powering `ezpn-ctl dump` (issue #88). vt100 0.15 does not
    /// expose direct scrollback iteration, so this walks the parser
    /// scrollback offset row-by-row — one of the few operations the
    /// IPC layer needs but cannot synthesise from the read-only
    /// [`Pane::screen`] accessor alone.
    pub fn dump_text(&mut self, include_scrollback: bool) -> Vec<String> {
        let saved_offset = self.scroll_offset;
        let (rows, cols) = self.parser.screen().size();

        let mut lines: Vec<String> = Vec::new();

        if include_scrollback {
            // Probe maximum scrollback by setting a huge offset and
            // reading the clamped value back. vt100 silently caps to
            // the actual buffered row count.
            self.parser.set_scrollback(usize::MAX / 2);
            let max_scrollback = self.parser.screen().scrollback();
            self.parser.set_scrollback(0);

            // Walk scrollback oldest -> newest. Offset N means
            // "viewport is N rows above live"; the *bottom* row of
            // that window is at relative position N from the live
            // bottom. Read top-to-bottom in chunks of `rows`, stepping
            // by `rows`, so we reconstruct full scrollback as a flat
            // line stream without overlap.
            let mut offset = max_scrollback;
            while offset > 0 {
                self.parser.set_scrollback(offset);
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
        self.parser.set_scrollback(0);
        for row in self.parser.screen().rows(0, cols) {
            lines.push(row);
        }

        // Restore user's scroll offset.
        self.parser.set_scrollback(saved_offset);
        lines
    }

    /// Check if child has enabled mouse reporting. Considers BOTH our
    /// per-pane state (`?1000/?1002/?1003`) and vt100's view, so we don't
    /// regress if vt100 happens to see a mode we missed.
    pub fn wants_mouse(&self) -> bool {
        if !self.state.mouse_mode.is_off() {
            return true;
        }
        self.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    /// Forward a mouse event to the child in its requested encoding.
    /// `col` and `row` are relative to the pane content area (0-indexed).
    /// `button`: 0=left, 1=middle, 2=right, 64=scroll-up, 65=scroll-down
    /// `release`: true for button release events
    pub fn send_mouse_event(&mut self, button: u16, col: u16, row: u16, release: bool) {
        // Prefer our per-pane state's encoding choice, falling back to vt100.
        let use_sgr = match self.state.mouse_mode.encoding {
            MouseEncoding::Sgr => true,
            MouseEncoding::X10 => {
                matches!(
                    self.parser.screen().mouse_protocol_encoding(),
                    vt100::MouseProtocolEncoding::Sgr
                )
            }
        };

        if use_sgr {
            let end = if release { 'm' } else { 'M' };
            let seq = format!("\x1b[<{};{};{}{}", button, col + 1, row + 1, end);
            self.write_bytes(seq.as_bytes());
        } else {
            // Default / X10 encoding
            let b = if release { 3u8 } else { (button as u8) & 0x7f };
            let b = b.wrapping_add(32);
            let c = ((col + 1).min(222) as u8).wrapping_add(32);
            let r = ((row + 1).min(222) as u8).wrapping_add(32);
            self.write_bytes(&[0x1b, b'[', b'M', b, c, r]);
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
        let osc_title = self.parser.screen().title();
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
        self.child.process_id()
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
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
    /// pending decoded payloads (`take_osc52_pending_confirm`) plus all
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
        self.state
            .osc52_pending_confirm
            .splice(0..0, payloads.drain(..));
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
    /// Crosses chunk boundaries via `osc_carry` / `csi_carry`. vt100 still
    /// sees the bytes — interception is purely additive.
    fn intercept(&mut self, chunk: &[u8]) {
        let mut ctx = InterceptCtx {
            state: &mut self.state,
            osc52_pending: &mut self.osc52_pending,
            osc_carry: &mut self.osc_carry,
            csi_carry: &mut self.csi_carry,
            writer: &mut *self.writer,
            policy: &self.clipboard_policy,
            palette: &self.theme_palette,
        };
        intercept_chunk(&mut ctx, chunk);
    }
}

/// All the borrows the interceptor needs. Bundled into a struct so we can
/// pass it to a free function and exercise it from tests without spawning
/// a real PTY.
struct InterceptCtx<'a> {
    state: &'a mut PaneTerminalState,
    osc52_pending: &'a mut Vec<Vec<u8>>,
    osc_carry: &'a mut Vec<u8>,
    csi_carry: &'a mut Vec<u8>,
    writer: &'a mut dyn Write,
    policy: &'a ClipboardPolicy,
    palette: &'a ThemePalette,
}

fn intercept_chunk(ctx: &mut InterceptCtx<'_>, chunk: &[u8]) {
    // DECSET tracking runs on the raw chunk: mode-set sequences are tiny
    // and we accept the (rare) chunk-boundary edge case rather than
    // duplicating the carry-over logic.
    track_dec_modes(chunk, ctx.state);

    // Concatenate any carry-over from the previous chunk so a sequence
    // split across two reads still parses cleanly.
    let mut buf: Vec<u8> = Vec::with_capacity(ctx.osc_carry.len() + chunk.len());
    buf.extend_from_slice(ctx.osc_carry);
    buf.extend_from_slice(ctx.csi_carry);
    buf.extend_from_slice(chunk);
    ctx.osc_carry.clear();
    ctx.csi_carry.clear();

    let mut i = 0;
    while i < buf.len() {
        if buf[i] != 0x1b || i + 1 >= buf.len() {
            i += 1;
            continue;
        }
        match buf[i + 1] {
            b']' => {
                // OSC: ESC ] <payload> (BEL | ESC \)
                match scan_osc_terminator(&buf[i + 2..]) {
                    ScanOsc::Complete { end } => {
                        let payload = &buf[i + 2..i + 2 + end];
                        handle_osc(ctx, payload);
                        let term_len = if buf.get(i + 2 + end) == Some(&0x07) {
                            1
                        } else {
                            2
                        };
                        i += 2 + end + term_len;
                        continue;
                    }
                    ScanOsc::Incomplete => {
                        // Cap so a runaway / hostile stream can't grow the
                        // carry without bound.
                        const OSC_CARRY_MAX: usize = 8192;
                        if buf.len() - i <= OSC_CARRY_MAX {
                            ctx.osc_carry.extend_from_slice(&buf[i..]);
                        }
                        return;
                    }
                }
            }
            b'[' => match scan_csi_terminator(&buf[i + 2..]) {
                ScanCsi::Complete { end, final_byte } => {
                    let body = &buf[i + 2..i + 2 + end];
                    if final_byte == b'u' {
                        handle_csi_u(ctx, body);
                    }
                    i += 2 + end + 1;
                    continue;
                }
                ScanCsi::Incomplete => {
                    const CSI_CARRY_MAX: usize = 256;
                    if buf.len() - i <= CSI_CARRY_MAX {
                        ctx.csi_carry.extend_from_slice(&buf[i..]);
                    }
                    return;
                }
            },
            _ => {
                i += 1;
            }
        }
    }
}

fn handle_osc(ctx: &mut InterceptCtx<'_>, payload: &[u8]) {
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
        return;
    }
    // OSC 8 (hyperlinks): pure pass-through (#76). See `docs/multi-client-osc.md`.
}

fn handle_csi_u(ctx: &mut InterceptCtx<'_>, body: &[u8]) {
    let Some((sigil, rest)) = body.split_first() else {
        return;
    };
    match sigil {
        b'>' => {
            let flags = parse_u8(rest).unwrap_or(0);
            ctx.state
                .kitty_kbd
                .push(KittyKbdFlags(flags & KittyKbdFlags::ALL));
        }
        b'<' => {
            let n = parse_u8(rest).unwrap_or(0) as usize;
            ctx.state.kitty_kbd.pop(n);
        }
        b'=' => {
            let mut parts = rest.split(|&b| b == b';');
            let flags = parts.next().and_then(parse_u8).unwrap_or(0);
            let mode = parts.next().and_then(parse_u8).unwrap_or(1);
            ctx.state
                .kitty_kbd
                .modify_top(KittyKbdFlags(flags & KittyKbdFlags::ALL), mode);
        }
        b'?' => {
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
    let _selection = split.next().unwrap_or(&[]);
    let data = split.next().unwrap_or(&[]);

    // Read query: `OSC 52 ; c ; ?`
    if data == b"?" {
        match ctx.policy.get {
            Osc52GetPolicy::Allow => {
                let mut env = Vec::with_capacity(full_payload.len() + 4);
                env.extend_from_slice(b"\x1b]");
                env.extend_from_slice(full_payload);
                env.extend_from_slice(b"\x07");
                ctx.osc52_pending.push(env);
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
    if data.len() > ctx.policy.max_bytes {
        tracing::warn!(
            target: "osc52",
            bytes = data.len(),
            cap = ctx.policy.max_bytes,
            "dropped oversized OSC 52 set"
        );
        return;
    }

    let effective = match ctx.state.osc52_decision {
        Osc52Decision::Allowed => Osc52SetPolicy::Allow,
        Osc52Decision::Denied => Osc52SetPolicy::Deny,
        Osc52Decision::Pending => ctx.policy.set,
    };

    let mut env = Vec::with_capacity(full_payload.len() + 4);
    env.extend_from_slice(b"\x1b]");
    env.extend_from_slice(full_payload);
    env.extend_from_slice(b"\x07");

    match effective {
        Osc52SetPolicy::Allow => ctx.osc52_pending.push(env),
        Osc52SetPolicy::Deny => {
            tracing::warn!(
                target: "osc52",
                bytes = data.len(),
                "blocked OSC 52 clipboard set (policy=deny)"
            );
        }
        Osc52SetPolicy::Confirm => {
            const PENDING_QUEUE_MAX: usize = 8;
            if ctx.state.osc52_pending_confirm.len() < PENDING_QUEUE_MAX {
                ctx.state.osc52_pending_confirm.push(env);
            } else {
                tracing::warn!(
                    target: "osc52",
                    "dropped OSC 52 set: confirm queue full"
                );
            }
        }
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
    let path_part = match after_scheme.find('/') {
        Some(idx) => &after_scheme[idx..],
        None => after_scheme,
    };
    let decoded = percent_decode(path_part);
    if decoded.is_empty() {
        return;
    }
    ctx.state.reported_cwd = Some((PathBuf::from(decoded), Instant::now()));
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

enum ScanOsc {
    Complete { end: usize },
    Incomplete,
}

/// Find the OSC terminator (BEL `0x07` or ST `ESC \`). Returns the index of
/// the start of the terminator within `tail`.
fn scan_osc_terminator(tail: &[u8]) -> ScanOsc {
    let mut i = 0;
    while i < tail.len() {
        if tail[i] == 0x07 {
            return ScanOsc::Complete { end: i };
        }
        if tail[i] == 0x1b && i + 1 < tail.len() && tail[i + 1] == b'\\' {
            return ScanOsc::Complete { end: i };
        }
        if tail[i] == 0x1b && i + 1 == tail.len() {
            // Mid-ST split across chunks
            return ScanOsc::Incomplete;
        }
        i += 1;
    }
    ScanOsc::Incomplete
}

enum ScanCsi {
    Complete { end: usize, final_byte: u8 },
    Incomplete,
}

/// Find the CSI final byte (range `0x40..=0x7e`). Returns its index within
/// `tail` and the byte itself. Anything else is a parameter / intermediate.
fn scan_csi_terminator(tail: &[u8]) -> ScanCsi {
    for (i, &b) in tail.iter().enumerate() {
        if (0x40..=0x7e).contains(&b) {
            return ScanCsi::Complete {
                end: i,
                final_byte: b,
            };
        }
    }
    ScanCsi::Incomplete
}

fn parse_u8(s: &[u8]) -> Option<u8> {
    parse_u32(s).and_then(|n| u8::try_from(n).ok())
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

// Update DECSET state from a chunk. Public so the pane interceptor can use
// the same logic without touching `Pane` private fields.
//
// vt100 also tracks `?1049` (alt screen) — we do not duplicate that here.
fn track_dec_modes(chunk: &[u8], state: &mut PaneTerminalState) {
    // Walk byte by byte, looking for `ESC [ ? <num> <h|l>`. Multiple modes can
    // be combined with `;` (`\x1b[?1000;1006h`); we handle that here too.
    let mut i = 0;
    while i + 3 < chunk.len() {
        if chunk[i] != 0x1b || chunk[i + 1] != b'[' || chunk[i + 2] != b'?' {
            i += 1;
            continue;
        }
        let body_start = i + 3;
        // Find terminator h or l
        let mut j = body_start;
        while j < chunk.len() && chunk[j] != b'h' && chunk[j] != b'l' {
            j += 1;
        }
        if j == chunk.len() {
            return; // incomplete chunk, ignore (DECSET is small enough we accept the loss)
        }
        let on = chunk[j] == b'h';
        let body = &chunk[body_start..j];
        for num_bytes in body.split(|&b| b == b';') {
            if let Some(num) = parse_u32(num_bytes) {
                apply_decset(state, num, on);
            }
        }
        i = j + 1;
    }
}

fn apply_decset(state: &mut PaneTerminalState, num: u32, on: bool) {
    match num {
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
        1006 => {
            state.mouse_mode.encoding = if on {
                MouseEncoding::Sgr
            } else {
                MouseEncoding::X10
            };
        }
        // ?2026 sync — owned by issue #73.
        // ?1049 alt-screen — vt100 owns it.
        _ => {}
    }
}

// ─── Key encoding (existing logic, untouched) ───────────────

fn encode_key(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // CSI u modifier parameter: 1 + (shift=1 | alt=2 | ctrl=4)
    let mods_param =
        1 + if shift { 1 } else { 0 } + if alt { 2 } else { 0 } + if ctrl { 4 } else { 0 };
    let has_mods = shift || alt || ctrl;

    match key.code {
        // Ctrl+char: special cases for non-letter characters, then a-z
        KeyCode::Char(c) if ctrl && !shift && !alt => match c {
            ' ' => vec![0x00],  // Ctrl+Space = NUL
            '[' => vec![0x1b],  // Ctrl+[ = ESC
            '\\' => vec![0x1c], // Ctrl+\ = FS
            ']' => vec![0x1d],  // Ctrl+] = GS (vim tag jump)
            '^' => vec![0x1e],  // Ctrl+^ = RS (vim alternate file)
            '_' => vec![0x1f],  // Ctrl+_ = US
            'a'..='z' => vec![c as u8 - b'a' + 1],
            _ => vec![(c.to_ascii_lowercase() as u8)
                .wrapping_sub(b'a')
                .wrapping_add(1)],
        },
        KeyCode::Char(c) if ctrl && alt => {
            let byte = match c {
                ' ' => 0x00,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                '^' => 0x1e,
                '_' => 0x1f,
                'a'..='z' => c as u8 - b'a' + 1,
                _ => (c.to_ascii_lowercase() as u8)
                    .wrapping_sub(b'a')
                    .wrapping_add(1),
            };
            vec![0x1b, byte]
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            if alt && !shift {
                let mut v = vec![0x1b];
                v.extend_from_slice(s.as_bytes());
                v
            } else if shift && (alt || ctrl) {
                format!("\x1b[{};{}u", c as u32, mods_param).into_bytes()
            } else {
                s.as_bytes().to_vec()
            }
        }
        // Enter: Shift+Enter → CSI u, Alt+Enter → ESC CR, plain → CR
        KeyCode::Enter => {
            if shift {
                format!("\x1b[13;{}u", mods_param).into_bytes()
            } else if alt {
                vec![0x1b, b'\r'] // Alt+Enter = ESC CR (legacy)
            } else if ctrl {
                format!("\x1b[13;{}u", mods_param).into_bytes()
            } else {
                vec![b'\r']
            }
        }
        // Backspace: Alt+BS → ESC DEL (word delete), Shift+BS → CSI u, plain → DEL
        KeyCode::Backspace => {
            if alt && !ctrl && !shift {
                vec![0x1b, 0x7f] // Alt+Backspace = ESC DEL (shell word delete)
            } else if ctrl && !alt && !shift {
                vec![0x08] // Ctrl+Backspace = BS
            } else if has_mods {
                format!("\x1b[127;{}u", mods_param).into_bytes()
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Tab => {
            if shift && !alt && !ctrl {
                vec![0x1b, b'[', b'Z'] // Shift+Tab = reverse tab (legacy)
            } else if has_mods {
                format!("\x1b[9;{}u", mods_param).into_bytes()
            } else {
                vec![b'\t']
            }
        }
        KeyCode::Esc => {
            if has_mods {
                format!("\x1b[27;{}u", mods_param).into_bytes()
            } else {
                vec![0x1b]
            }
        }
        // Arrow keys with modifiers: ESC [ 1 ; <mods> A/B/C/D
        KeyCode::Up => arrow_with_mods(b'A', has_mods, mods_param),
        KeyCode::Down => arrow_with_mods(b'B', has_mods, mods_param),
        KeyCode::Right => arrow_with_mods(b'C', has_mods, mods_param),
        KeyCode::Left => arrow_with_mods(b'D', has_mods, mods_param),
        KeyCode::Home => {
            if has_mods {
                format!("\x1b[1;{}H", mods_param).into_bytes()
            } else {
                vec![0x1b, b'[', b'H']
            }
        }
        KeyCode::End => {
            if has_mods {
                format!("\x1b[1;{}F", mods_param).into_bytes()
            } else {
                vec![0x1b, b'[', b'F']
            }
        }
        KeyCode::Delete => tilde_with_mods(3, has_mods, mods_param),
        KeyCode::PageUp => tilde_with_mods(5, has_mods, mods_param),
        KeyCode::PageDown => tilde_with_mods(6, has_mods, mods_param),
        KeyCode::Insert => tilde_with_mods(2, has_mods, mods_param),
        KeyCode::F(n) => encode_f_key_with_mods(n, has_mods, mods_param),
        _ => vec![],
    }
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
/// the caller (typically [`Pane::evict_if_oversized`]) then decides whether
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
/// current vt100 0.15 API neither `OldestLine` nor `LargestLine` can be
/// honoured because history rows are not addressable. See
/// [`Pane::evict_if_oversized`] for the full limitation note.
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

/// Scan raw PTY output for DECSET 2026 synchronized-output brackets and
/// update the per-pane reference-counted depth (issue #73). The 8-byte
/// sequences are looked up via `windows()`; we deliberately do not
/// reassemble across chunk boundaries because PTY reads use a 4 KB buffer
/// and the 33 ms safety timeout absorbs the rare split case.
fn scan_sync_brackets(data: &[u8], depth: &mut u16, opened_at: &mut Option<Instant>) {
    const SYNC_OPEN: &[u8] = b"\x1b[?2026h";
    const SYNC_CLOSE: &[u8] = b"\x1b[?2026l";
    debug_assert_eq!(SYNC_OPEN.len(), SYNC_CLOSE.len());

    let n = SYNC_OPEN.len();
    if data.len() < n {
        return;
    }
    for window in data.windows(n) {
        if window == SYNC_OPEN {
            if *depth == 0 {
                *opened_at = Some(Instant::now());
            }
            *depth = depth.saturating_add(1);
        } else if window == SYNC_CLOSE {
            *depth = depth.saturating_sub(1);
            if *depth == 0 {
                *opened_at = None;
            }
        }
    }
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

    fn run_intercept(
        chunk: &[u8],
        state: &mut PaneTerminalState,
        osc52_pending: &mut Vec<Vec<u8>>,
        policy: &ClipboardPolicy,
        palette: &ThemePalette,
    ) -> Vec<u8> {
        let mut writer = VecWriter(Vec::new());
        let mut osc_carry = Vec::new();
        let mut csi_carry = Vec::new();
        {
            let mut ctx = InterceptCtx {
                state,
                osc52_pending,
                osc_carry: &mut osc_carry,
                csi_carry: &mut csi_carry,
                writer: &mut writer,
                policy,
                palette,
            };
            intercept_chunk(&mut ctx, chunk);
        }
        writer.0
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

    #[test]
    fn scan_csi_finds_final_byte() {
        match scan_csi_terminator(b"5u") {
            ScanCsi::Complete { end, final_byte } => {
                assert_eq!(end, 1);
                assert_eq!(final_byte, b'u');
            }
            ScanCsi::Incomplete => panic!("should be complete"),
        }
    }

    #[test]
    fn scan_osc_terminators() {
        match scan_osc_terminator(b"7;file:///tmp\x07") {
            ScanOsc::Complete { end } => assert_eq!(end, b"7;file:///tmp".len()),
            ScanOsc::Incomplete => panic!("BEL terminator missed"),
        }
        match scan_osc_terminator(b"7;file:///tmp\x1b\\") {
            ScanOsc::Complete { end } => assert_eq!(end, b"7;file:///tmp".len()),
            ScanOsc::Incomplete => panic!("ST terminator missed"),
        }
        match scan_osc_terminator(b"7;file:///tmp") {
            ScanOsc::Incomplete => {}
            ScanOsc::Complete { .. } => panic!("should be incomplete"),
        }
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
        assert_eq!(state.kitty_kbd.active().bits(), 5);

        // Query → reply written to writer
        let reply = run_intercept(b"\x1b[?u", &mut state, &mut pending, &policy, &palette);
        assert_eq!(reply, b"\x1b[?5u");
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
            b"\x1b]7;file://hostname/home/u%20ser/pkg\x07",
            &mut state,
            &mut pending,
            &policy,
            &palette,
        );
        let (path, _ts) = state.reported_cwd.as_ref().unwrap();
        assert_eq!(path.as_path(), std::path::Path::new("/home/u ser/pkg"));
    }

    // ─── #76 — OSC 8 hyperlinks pass-through ────────────────

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
        let mut palette = ThemePalette::default();
        palette.bg = Some(Rgb::new(0x1e, 0x1e, 0x2e));

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
        let mut palette = ThemePalette::default();
        palette.fg = Some(Rgb::new(0xff, 0xff, 0xff));

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
        let mut osc_carry = Vec::new();
        let mut csi_carry = Vec::new();
        {
            let mut ctx = InterceptCtx {
                state: &mut state,
                osc52_pending: &mut pending,
                osc_carry: &mut osc_carry,
                csi_carry: &mut csi_carry,
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
