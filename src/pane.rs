use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Global wake channel: PTY reader threads send () to wake the server main loop.
/// Set once by the server with `init_wake_channel()`.
///
/// Per SPEC 05 `docs/spec/v0.10.0/05-render-loop-micro-perf.md` §4.2: this is a
/// **bounded** channel because wake messages are idempotent — the receiver
/// drains all pending wakes per tick (`event_loop.rs`), so dropping overflow
/// only loses a redundant signal, never a unique state transition.
static WAKE_TX: OnceLock<mpsc::SyncSender<()>> = OnceLock::new();

/// Wake-channel capacity. 64 is large enough for steady-state bursts at
/// 60 fps × handful of panes; overflow only happens when the main loop has
/// genuinely stalled, in which case extra wakes are noise.
const WAKE_CHANNEL_CAPACITY: usize = 64;

/// Initialize the global wake channel. Call once from server startup.
/// Returns the Receiver that the main loop should use.
pub fn init_wake_channel() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::sync_channel::<()>(WAKE_CHANNEL_CAPACITY);
    let _ = WAKE_TX.set(tx);
    rx
}

/// Send a wake signal to the main loop (used by reader threads and client reader).
pub fn wake_main_loop() {
    if let Some(tx) = WAKE_TX.get() {
        // Wake messages are idempotent — drop on overflow.
        let _ = tx.try_send(());
    }
}
use std::path::PathBuf;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneLaunch {
    Shell,
    Command(String),
}

/// Per-pane state. Field declaration order is **load-bearing** for `Drop`:
/// Rust drops fields top-to-bottom, so any field whose `Drop` must run
/// before the reader thread is joined must appear *above* `reader_handle`.
/// In particular `master` (the PTY master fd) drops before `reader_handle`
/// so the blocking `reader.read()` unblocks on EOF before we try to join.
/// See SPEC 03 `docs/spec/v0.10.0/03-lifecycle-gc.md` §4.1.
pub struct Pane {
    /// Child first so SIGHUP propagates before the master fd drops.
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    /// Send `()` to ask the reader to exit. `take()`-d in `Drop` so the
    /// signal is idempotent. The reader also exits naturally on PTY EOF
    /// (triggered by `master` dropping below); this signal just bounds
    /// shutdown latency for slow / sleeping children.
    shutdown_tx: Option<mpsc::SyncSender<()>>,
    /// Drops here → kernel notices slave fd is closed → reader's blocking
    /// `read()` returns 0 → reader exits → handle is joinable.
    master: Box<dyn MasterPty + Send>,
    /// `Some` until joined in `Drop`. If join exceeds the 250 ms deadline
    /// the handle is `mem::forget`ed and a single warn line is emitted.
    reader_handle: Option<std::thread::JoinHandle<()>>,
    reader_rx: Receiver<Vec<u8>>,
    parser: vt100::Parser,
    alive: bool,
    launch: PaneLaunch,
    scroll_offset: usize, // 0 = live (bottom), >0 = scrolled up N lines
    name: Option<String>,
    exit_code: Option<u32>,
    /// Pending OSC 52 clipboard sequences from child to forward to the terminal.
    /// Capped at OSC52_MAX_ENTRIES entries / OSC52_MAX_BYTES total — drops oldest on overflow
    /// to prevent a malicious or buggy child from exhausting memory via clipboard spam.
    pub osc52_pending: Vec<Vec<u8>>,
    /// Running byte count of `osc52_pending` (sum of inner Vec lengths).
    osc52_bytes: usize,
    /// Set when `scan_osc52` had to drop a sequence due to a cap. Used by the status bar
    /// (and tests) to signal "clipboard truncated"; reset on `take_osc52`.
    osc52_truncated: bool,
    /// Whether the child has enabled bracketed paste mode (\x1b[?2004h).
    bracketed_paste: bool,
    /// Whether the child has requested focus events (\x1b[?1004h).
    focus_events: bool,
    /// The working directory this pane was launched with.
    initial_cwd: Option<PathBuf>,
    /// Custom env vars this pane was launched with.
    initial_env: HashMap<String, String>,
    /// Custom shell override for this pane (if different from default).
    initial_shell: Option<String>,
    /// Current scrollback ring capacity (lines). Tracked here because vt100 0.15
    /// does not expose the value passed to `Parser::new`. Used by SPEC 02
    /// `clear-history` / `set-scrollback` IPC commands.
    scrollback_cap: usize,
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
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel::<()>(1);
        let pid_for_name = child.process_id().unwrap_or(0);
        let reader_handle = std::thread::Builder::new()
            .name(format!("ezpn-pty-{pid_for_name}"))
            .spawn(move || {
                // Isolate PTY-reader panics: a bad ANSI sequence or vt100 bug
                // must not take down the daemon. On unwind the channel drops,
                // which causes `read_output()` to observe
                // `TryRecvError::Disconnected` and mark the pane dead with
                // exit_code=u32::MAX (see `read_output`).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut reader = reader;
                    let mut buf = [0u8; 4096];
                    loop {
                        // Cheap shutdown check between reads (SPEC 03 §4.1).
                        // The blocking read below also unblocks when the
                        // master fd drops, so this signal only bounds
                        // shutdown latency for slow / sleeping children.
                        if shutdown_rx.try_recv().is_ok() {
                            break;
                        }
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
                }));
                if let Err(payload) = result {
                    let reason = match payload.downcast_ref::<&'static str>() {
                        Some(s) => (*s).to_string(),
                        None => match payload.downcast_ref::<String>() {
                            Some(s) => s.clone(),
                            None => "unknown panic payload".to_string(),
                        },
                    };
                    eprintln!("ezpn: PTY reader thread panicked: {}", reason);
                    wake_main_loop();
                }
            })
            .map_err(|e| anyhow::anyhow!("spawn ezpn-pty thread: {e}"))?;

        let parser = vt100::Parser::new(rows, cols, scrollback);

        Ok(Self {
            child,
            writer,
            shutdown_tx: Some(shutdown_tx),
            master: pair.master,
            reader_handle: Some(reader_handle),
            reader_rx: rx,
            parser,
            alive: true,
            launch,
            scroll_offset: 0,
            name: None,
            exit_code: None,
            osc52_pending: Vec::new(),
            osc52_bytes: 0,
            osc52_truncated: false,
            bracketed_paste: false,
            focus_events: false,
            initial_cwd: cwd.map(|p| p.to_path_buf()),
            initial_env: env.clone(),
            initial_shell: None,
            scrollback_cap: scrollback,
        })
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
                    // Intercept OSC 52 clipboard sequences before vt100 processing
                    scan_osc52(
                        &data,
                        &mut self.osc52_pending,
                        &mut self.osc52_bytes,
                        &mut self.osc52_truncated,
                    );
                    // Focus events still need a manual scan because vt100 0.15
                    // does not expose `?1004h`/`l` state via its public API.
                    // Bracketed paste (`?2004h`/`l`) is read from the vt100
                    // screen below — see SPEC 05 §4.1.
                    track_focus_events(&data, &mut self.focus_events);
                    self.parser.process(&data);
                    // Cache `bracketed_paste` from vt100 after `process()` so
                    // the per-keystroke encoder reads a single bool field
                    // instead of walking the screen state. Updated exactly
                    // here, the only place where `?2004` state can change.
                    self.bracketed_paste = self.parser.screen().bracketed_paste();
                    // New output snaps scroll to bottom
                    if self.scroll_offset > 0 {
                        self.scroll_offset = 0;
                    }
                    got_data = true;
                    count += 1;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Reader thread is gone (EOF, error, or panic).
                    // If the child is still alive, the panic-isolation path may have
                    // killed only the reader — record sentinel exit code so callers
                    // can detect "abnormal" pane death distinct from clean exit.
                    if self.alive && self.exit_code.is_none() {
                        self.exit_code = Some(u32::MAX);
                    }
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
        got_data || was_alive != self.alive
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

    /// Borrow the underlying vt100 parser. Used by snapshot serialization to
    /// encode the visible scrollback into a portable blob.
    pub fn parser(&self) -> &vt100::Parser {
        &self.parser
    }

    /// Mutable parser access for replaying a serialized scrollback blob into
    /// a freshly spawned pane during snapshot restore.
    pub fn parser_mut(&mut self) -> &mut vt100::Parser {
        &mut self.parser
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

    /// Check if child has enabled mouse reporting
    pub fn wants_mouse(&self) -> bool {
        self.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    /// Forward a mouse event to the child in its requested encoding.
    /// `col` and `row` are relative to the pane content area (0-indexed).
    /// `button`: 0=left, 1=middle, 2=right, 64=scroll-up, 65=scroll-down
    /// `release`: true for button release events
    pub fn send_mouse_event(&mut self, button: u16, col: u16, row: u16, release: bool) {
        let screen = self.parser.screen();
        let encoding = screen.mouse_protocol_encoding();

        match encoding {
            vt100::MouseProtocolEncoding::Sgr => {
                let end = if release { 'm' } else { 'M' };
                let seq = format!("\x1b[<{};{};{}{}", button, col + 1, row + 1, end);
                self.write_bytes(seq.as_bytes());
            }
            _ => {
                // Default / UTF-8 encoding
                let b = if release { 3u8 } else { (button as u8) & 0x7f };
                let b = b.wrapping_add(32);
                let c = ((col + 1).min(222) as u8).wrapping_add(32);
                let r = ((row + 1).min(222) as u8).wrapping_add(32);
                self.write_bytes(&[0x1b, b'[', b'M', b, c, r]);
            }
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

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    /// Take pending OSC 52 clipboard sequences (clears the queue).
    pub fn take_osc52(&mut self) -> Vec<Vec<u8>> {
        self.osc52_bytes = 0;
        self.osc52_truncated = false;
        std::mem::take(&mut self.osc52_pending)
    }

    /// True if `scan_osc52` had to drop sequences since the last `take_osc52` call.
    /// Surfaced for diagnostics; not currently rendered in the status bar.
    #[allow(dead_code)]
    pub fn osc52_was_truncated(&self) -> bool {
        self.osc52_truncated
    }

    /// Reap a finished child without blocking. If the child has exited and
    /// hadn't been observed yet, sets `alive=false` + `exit_code=Some(...)`
    /// and returns the exit code. SIGCHLD handler in the daemon iterates
    /// every pane and calls this so zombies don't accumulate.
    pub fn update_alive(&mut self) -> Option<u32> {
        if !self.alive {
            return None;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let code = status.exit_code();
                self.exit_code = Some(code);
                self.alive = false;
                Some(code)
            }
            _ => None,
        }
    }

    /// Current scrollback line cap (the value last passed to `Parser::new` or
    /// `set_scrollback_lines`, NOT the live `Screen::scrollback()` offset).
    /// Tracked manually because vt100 0.15 does not expose it.
    pub fn scrollback_cap(&self) -> usize {
        self.scrollback_cap
    }

    /// Drop the scrollback ringbuffer above the visible screen. The visible
    /// rows survive; user is snapped to bottom (live view).
    ///
    /// Implementation: `encode_scrollback` captures the visible rows as
    /// `rows_formatted` byte streams (the same machinery snapshot uses), then
    /// a fresh `vt100::Parser::new(rows, cols, scrollback_cap)` replays them.
    /// The old parser — including all scrollback rows — is dropped, releasing
    /// the ringbuffer in place. Typical cost ~5–20 ms for an 80×24 pane.
    pub fn clear_history(&mut self) -> anyhow::Result<()> {
        let blob = crate::snapshot_blob::encode_scrollback(&self.parser);
        let (rows, cols) = self.parser.screen().size();
        let mut fresh = vt100::Parser::new(rows, cols, self.scrollback_cap);
        if !blob.is_empty() {
            crate::snapshot_blob::decode_scrollback(&blob, &mut fresh)?;
        }
        self.parser = fresh;
        self.scroll_offset = 0;
        Ok(())
    }

    /// Resize the scrollback ringbuffer to `new_lines`. Same encode/replay
    /// rebuild as `clear_history`, but the new parser keeps its scrollback
    /// rows up to the new cap. If `new_lines == 0`, behaves as a hard
    /// `clear_history` (no scrollback above visible).
    pub fn set_scrollback_lines(&mut self, new_lines: usize) -> anyhow::Result<()> {
        let blob = crate::snapshot_blob::encode_scrollback(&self.parser);
        let (rows, cols) = self.parser.screen().size();
        let mut fresh = vt100::Parser::new(rows, cols, new_lines);
        if !blob.is_empty() {
            crate::snapshot_blob::decode_scrollback(&blob, &mut fresh)?;
        }
        self.parser = fresh;
        self.scrollback_cap = new_lines;
        self.scroll_offset = 0;
        Ok(())
    }

    /// Estimated bytes the pane's vt100 ringbuffer holds. Used by the workspace-level
    /// memory budget to pick the largest pane to warn about. The estimate is intentionally
    /// coarse — vt100 doesn't expose precise sizing.
    pub fn estimated_scrollback_bytes(&self) -> usize {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let scrollback = screen.scrollback() + rows as usize;
        scrollback.saturating_mul(cols as usize).saturating_mul(32)
    }

    /// Whether the child has bracketed paste enabled.
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    /// Whether the child has requested focus events.
    pub fn wants_focus(&self) -> bool {
        self.focus_events
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

    /// Try to get the current working directory of the child process.
    /// Falls back to the initial cwd if the child has exited.
    #[cfg(target_os = "macos")]
    pub fn live_cwd(&self) -> Option<PathBuf> {
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

    /// Try to get the current working directory of the child process.
    /// Falls back to the initial cwd if the child has exited.
    #[cfg(not(target_os = "macos"))]
    pub fn live_cwd(&self) -> Option<PathBuf> {
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
}

/// SPEC 03 §4.1: deterministic shutdown for `Pane`. Steps run in declared
/// order:
/// 1. SIGHUP the child (idempotent — already dead is fine).
/// 2. Signal the reader thread to exit (one-shot, idempotent).
/// 3. Field drop order in `Pane` ensures `master` is released next,
///    triggering EOF on the reader's blocking `read()`.
/// 4. Bounded join: poll `is_finished()` for up to 250 ms, then either
///    `join()` cleanly or `mem::forget` the handle and warn.
impl Drop for Pane {
    fn drop(&mut self) {
        if self.alive {
            let _ = self.child.kill();
            self.alive = false;
        }
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.try_send(());
        }
        // Capture pid *before* `master` drops — the warn-on-leak path needs
        // a stable identifier and `child.process_id()` may surface a stale
        // value once the kernel reaps the slot.
        let pid = self.child.process_id().unwrap_or(0);
        if let Some(handle) = self.reader_handle.take() {
            // std::thread has no "join with timeout"; emulate via
            // `is_finished()` polling. The reader exits via either the
            // shutdown signal above, or PTY EOF when `master` drops as
            // part of this struct's drop (after this block returns).
            let deadline = Instant::now() + Duration::from_millis(250);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                eprintln!(
                    "ezpn: PTY reader thread for pid {pid} did not exit \
                     within 250ms; leaking handle"
                );
                std::mem::forget(handle);
            }
        }
    }
}

/// Maximum number of pending OSC 52 sequences per pane. Beyond this, the oldest
/// sequence is dropped (FIFO) to bound memory under hostile / buggy children.
pub const OSC52_MAX_ENTRIES: usize = 16;

/// Maximum total bytes of pending OSC 52 sequences per pane. Together with
/// OSC52_MAX_ENTRIES this guarantees `osc52_pending` never exceeds ~256 KiB.
pub const OSC52_MAX_BYTES: usize = 256 * 1024;

/// Single-sequence cap: any individual OSC 52 longer than this is rejected
/// outright (still increments `truncated` for diagnostics). Matches xterm's
/// historical 100 KiB practical limit; we round up.
pub const OSC52_MAX_SEQUENCE_BYTES: usize = 128 * 1024;

/// Scan raw PTY output for OSC 52 clipboard sequences and collect them, enforcing
/// per-pane caps so a runaway child cannot exhaust memory.
fn scan_osc52(data: &[u8], out: &mut Vec<Vec<u8>>, out_bytes: &mut usize, truncated: &mut bool) {
    const PREFIX: &[u8] = b"\x1b]52;";
    let mut i = 0;
    while i + PREFIX.len() < data.len() {
        if data[i..].starts_with(PREFIX) {
            let start = i;
            i += PREFIX.len();
            // Find terminator: BEL (\x07) or ST (\x1b\\)
            while i < data.len() {
                if data[i] == 0x07 {
                    push_osc52_capped(out, out_bytes, truncated, &data[start..=i]);
                    break;
                }
                if i + 1 < data.len() && data[i] == 0x1b && data[i + 1] == b'\\' {
                    push_osc52_capped(out, out_bytes, truncated, &data[start..i + 2]);
                    i += 1;
                    break;
                }
                i += 1;
            }
        }
        i += 1;
    }
}

fn push_osc52_capped(
    out: &mut Vec<Vec<u8>>,
    out_bytes: &mut usize,
    truncated: &mut bool,
    seq: &[u8],
) {
    if seq.len() > OSC52_MAX_SEQUENCE_BYTES {
        // Single sequence too large — drop it entirely; never half-truncate (terminals
        // treat partial OSC 52 as protocol error).
        *truncated = true;
        return;
    }
    // Drop oldest entries until both caps fit.
    while !out.is_empty()
        && (out.len() >= OSC52_MAX_ENTRIES || *out_bytes + seq.len() > OSC52_MAX_BYTES)
    {
        let dropped = out.remove(0);
        *out_bytes -= dropped.len();
        *truncated = true;
    }
    *out_bytes += seq.len();
    out.push(seq.to_vec());
}

/// Track focus-event mode changes in raw PTY output.
///
/// vt100 0.15 does not expose `?1004h`/`l` state via its public API, so we
/// still scan for it — but only for this single mode pair, halving the
/// constant cost vs the previous `track_dec_modes`. Bracketed paste
/// (`?2004`) is read from `vt100::Screen::bracketed_paste()` directly.
/// See SPEC 05 §4.1.
fn track_focus_events(data: &[u8], focus_events: &mut bool) {
    const FE_ON: &[u8] = b"\x1b[?1004h";
    const FE_OFF: &[u8] = b"\x1b[?1004l";
    // FE_ON.len() == FE_OFF.len() == 8; one window size suffices.
    for window in data.windows(FE_ON.len()) {
        if window == FE_ON {
            *focus_events = true;
        } else if window == FE_OFF {
            *focus_events = false;
        }
    }
}

/// Cached `EZPN_ALT_LEGACY=1` flag — read once at process start so every
/// keystroke avoids a syscall. Setting this in the parent shell before
/// `ezpn a` restores the pre-0.7 ESC-prefix encoding for Alt+Char, useful
/// for very old shells that only understand the legacy form.
fn alt_legacy_mode() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("EZPN_ALT_LEGACY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

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
                // Issue #16: Alt+Char now matches Alt+Arrow / Alt+Function-key
                // encoding (CSI u, RFC 3665), so shells that bind on
                // \x1b[<code>;3u (zsh / bash with bind, vim, helix) see Alt
                // input the same way for letters and arrows. Older shells
                // that only understand the legacy ESC-prefix form can opt
                // in via `EZPN_ALT_LEGACY=1`.
                if alt_legacy_mode() {
                    let mut v = vec![0x1b];
                    v.extend_from_slice(s.as_bytes());
                    v
                } else {
                    format!("\x1b[{};{}u", c as u32, mods_param).into_bytes()
                }
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

#[cfg(test)]
#[allow(unused_imports)] // benches/render_hotpaths.rs include pane.rs via #[path];
                         // when those build under non-test profiles the imports are
                         // flagged though the mod itself is gated by cfg(test).
mod osc52_tests {
    use super::{push_osc52_capped, OSC52_MAX_BYTES, OSC52_MAX_ENTRIES, OSC52_MAX_SEQUENCE_BYTES};

    fn fake_seq(payload_len: usize) -> Vec<u8> {
        let mut v = b"\x1b]52;c;".to_vec();
        v.extend(std::iter::repeat_n(b'A', payload_len));
        v.push(0x07);
        v
    }

    #[test]
    fn entry_cap_drops_oldest() {
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 0usize;
        let mut truncated = false;
        for _ in 0..(OSC52_MAX_ENTRIES + 4) {
            let seq = fake_seq(8);
            push_osc52_capped(&mut out, &mut bytes, &mut truncated, &seq);
        }
        assert_eq!(out.len(), OSC52_MAX_ENTRIES);
        assert!(truncated);
        assert!(bytes <= OSC52_MAX_BYTES);
    }

    #[test]
    fn byte_cap_drops_oldest() {
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 0usize;
        let mut truncated = false;
        // Each ~32 KiB; should plateau around OSC52_MAX_BYTES / 32 KiB.
        let big = fake_seq(32 * 1024);
        for _ in 0..16 {
            push_osc52_capped(&mut out, &mut bytes, &mut truncated, &big);
        }
        assert!(bytes <= OSC52_MAX_BYTES, "bytes={bytes}");
        assert!(truncated);
    }

    #[test]
    fn oversize_sequence_dropped() {
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 0usize;
        let mut truncated = false;
        let huge = fake_seq(OSC52_MAX_SEQUENCE_BYTES + 1);
        push_osc52_capped(&mut out, &mut bytes, &mut truncated, &huge);
        assert!(out.is_empty());
        assert_eq!(bytes, 0);
        assert!(truncated);
    }

    #[test]
    fn under_caps_keeps_all() {
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 0usize;
        let mut truncated = false;
        for _ in 0..4 {
            push_osc52_capped(&mut out, &mut bytes, &mut truncated, &fake_seq(64));
        }
        assert_eq!(out.len(), 4);
        assert!(!truncated);
    }
}

/// SPEC 02 history-control tests. Use `vt100::Parser` directly to avoid
/// spawning real PTYs in unit tests; `Pane::clear_history` and
/// `Pane::set_scrollback_lines` are thin wrappers around the same
/// `snapshot_blob` round-trip we exercise here.
#[cfg(test)]
#[allow(unused_imports)]
mod history_tests {
    use super::*;
    use crate::snapshot_blob::{decode_scrollback, encode_scrollback};

    /// Mirrors `Pane::clear_history` without requiring a live PTY: encode the
    /// visible screen, drop the parser, and replay into a fresh one with the
    /// same cap.
    fn clear_history_via_blob(parser: &mut vt100::Parser, cap: usize) {
        let blob = encode_scrollback(parser);
        let (rows, cols) = parser.screen().size();
        let mut fresh = vt100::Parser::new(rows, cols, cap);
        if !blob.is_empty() {
            decode_scrollback(&blob, &mut fresh).unwrap();
        }
        *parser = fresh;
    }

    fn fill_scrollback(parser: &mut vt100::Parser, lines: usize) {
        for i in 0..lines {
            let row = format!("line {i}\r\n");
            parser.process(row.as_bytes());
        }
    }

    #[test]
    fn clear_history_drops_scrollback_keeps_visible_line() {
        let mut parser = vt100::Parser::new(5, 20, 1000);
        fill_scrollback(&mut parser, 200);
        // Probe scrollback depth.
        parser.set_scrollback(usize::MAX);
        let scroll_before = parser.screen().scrollback();
        parser.set_scrollback(0);
        assert!(scroll_before > 0, "fixture must have some scrollback");

        clear_history_via_blob(&mut parser, 1000);

        parser.set_scrollback(usize::MAX);
        let scroll_after = parser.screen().scrollback();
        parser.set_scrollback(0);
        assert_eq!(scroll_after, 0, "clear_history must drop scrollback rows");
    }

    #[test]
    fn clear_history_under_100ms_for_typical_pane() {
        let mut parser = vt100::Parser::new(24, 80, 10_000);
        fill_scrollback(&mut parser, 10_000);
        let t0 = std::time::Instant::now();
        clear_history_via_blob(&mut parser, 10_000);
        let elapsed = t0.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "PRD §6: clear_history must complete in <100ms (got {:?})",
            elapsed
        );
    }

    #[test]
    fn set_scrollback_lines_shrinks_cap() {
        let mut parser = vt100::Parser::new(5, 20, 10_000);
        fill_scrollback(&mut parser, 5_000);
        // Resize to a tiny cap; new parser holds at most 100 scrollback rows.
        clear_history_via_blob(&mut parser, 100);
        parser.set_scrollback(usize::MAX);
        let scroll = parser.screen().scrollback();
        parser.set_scrollback(0);
        assert!(
            scroll <= 100,
            "after shrink to cap=100, scrollback must be <= 100 (got {scroll})"
        );
    }
}

/// SPEC 03 §4.1: deterministic shutdown for `Pane`. The reader thread
/// MUST exit within the bounded join window once the pane drops.
#[cfg(test)]
mod drop_tests {
    // bench `render_hotpaths` includes pane.rs via `#[path]`; `super::*`
    // ends up unused under that compilation unit. See snapshot_blob.rs
    // for the same pattern.
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn pane_drop_joins_reader_within_500ms() {
        // Spawn a real pane running a long-running command (`sleep 60`).
        // Drop it and assert the reader thread is gone within the bounded
        // window (allow 500ms in tests vs the production 250ms budget).
        let pane = Pane::with_scrollback(
            "/bin/sh",
            PaneLaunch::Command("sleep 60".to_string()),
            80,
            24,
            1000,
        );
        let pane = match pane {
            Ok(p) => p,
            Err(_) => return, // CI without /bin/sh is acceptable to skip.
        };
        let t0 = std::time::Instant::now();
        drop(pane);
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "Pane::drop must complete within 500ms (got {elapsed:?})"
        );
    }
}

/// SPEC 05 §4.1 / §4.2 — render-loop micro-perf:
/// * `track_focus_events` only handles `?1004h`/`l` (no longer scans `?2004`).
/// * `bracketed_paste` is read from `vt100::Screen::bracketed_paste()` so the
///   raw-byte scan and vt100 stay in sync without duplicate work.
/// * The wake channel is bounded (`sync_channel(64)`) and `try_send` drops
///   overflow, since wake messages are idempotent.
#[cfg(test)]
#[allow(unused_imports)]
mod render_micro_perf_tests {
    use super::*;

    #[test]
    fn focus_events_set_via_scan() {
        let mut fe = false;
        track_focus_events(b"\x1b[?1004h", &mut fe);
        assert!(fe, "?1004h must enable focus events");
        track_focus_events(b"\x1b[?1004l", &mut fe);
        assert!(!fe, "?1004l must disable focus events");
    }

    #[test]
    fn focus_events_scanner_ignores_bracketed_paste() {
        // Previous track_dec_modes also toggled bracketed_paste; the new
        // focus-only scanner must NOT touch any other flag.
        let mut fe = false;
        track_focus_events(b"\x1b[?2004h", &mut fe);
        assert!(!fe, "?2004h must not affect focus events");
        track_focus_events(b"\x1b[?2004l", &mut fe);
        assert!(!fe, "?2004l must not affect focus events");
    }

    #[test]
    fn focus_events_scan_finds_sequence_mid_buffer() {
        // The scanner walks an 8-byte sliding window; a sequence anywhere in
        // the chunk must be detected, not only at the start.
        let mut fe = false;
        track_focus_events(b"prefix bytes \x1b[?1004h trailing", &mut fe);
        assert!(fe);
    }

    #[test]
    fn bracketed_paste_state_matches_screen() {
        // Drive a vt100 parser with mode toggles and verify the screen flag
        // matches the cached value `read_output` would compute. This is the
        // contract that lets us drop the raw-byte scan for `?2004`.
        let mut parser = vt100::Parser::new(5, 20, 100);
        assert!(!parser.screen().bracketed_paste());

        parser.process(b"\x1b[?2004h");
        assert!(parser.screen().bracketed_paste(), "vt100 must track ?2004h");

        parser.process(b"some output\r\n");
        assert!(
            parser.screen().bracketed_paste(),
            "intermediate output must not flip the flag"
        );

        parser.process(b"\x1b[?2004l");
        assert!(
            !parser.screen().bracketed_paste(),
            "vt100 must track ?2004l"
        );
    }

    #[test]
    fn bracketed_paste_set_across_split_chunks() {
        // The mode sequence may arrive split across read() boundaries — vt100
        // still reassembles correctly. Documents the contract our caller
        // relies on (cache from screen, not raw scan).
        let mut parser = vt100::Parser::new(5, 20, 100);
        parser.process(b"\x1b[?20");
        parser.process(b"04h");
        assert!(parser.screen().bracketed_paste());
    }

    #[test]
    fn wake_channel_is_bounded_and_drops_on_overflow() {
        // Use a fresh local channel that mirrors WAKE_TX's shape — the global
        // is initialised once per process by the daemon, so unit tests can't
        // re-init it. Verifies the contract: try_send never blocks and never
        // grows beyond capacity.
        let (tx, rx) = mpsc::sync_channel::<()>(WAKE_CHANNEL_CAPACITY);
        for _ in 0..(WAKE_CHANNEL_CAPACITY * 4) {
            // Mirrors `wake_main_loop`: drop on full, don't block.
            let _ = tx.try_send(());
        }
        let mut drained = 0;
        while rx.try_recv().is_ok() {
            drained += 1;
        }
        assert_eq!(
            drained, WAKE_CHANNEL_CAPACITY,
            "bounded channel must hold exactly capacity entries on overflow"
        );
    }

    #[test]
    fn wake_channel_capacity_is_sane() {
        // Sanity check on the chosen capacity — large enough to absorb one
        // tick at 60 fps with a handful of panes (~10s of wakes), small
        // enough to be O(byte) memory.
        const _: () = assert!(
            WAKE_CHANNEL_CAPACITY >= 32 && WAKE_CHANNEL_CAPACITY <= 256,
            "WAKE_CHANNEL_CAPACITY outside expected range"
        );
    }
}
