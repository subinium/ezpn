use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneLaunch {
    Shell,
    Command(String),
}

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
    pub osc52_pending: Vec<Vec<u8>>,
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
            bracketed_paste: false,
            focus_events: false,
            initial_cwd: cwd.map(|p| p.to_path_buf()),
            initial_env: env.clone(),
            initial_shell: None,
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
                    scan_osc52(&data, &mut self.osc52_pending);
                    // Track DEC private modes from child output
                    track_dec_modes(&data, &mut self.bracketed_paste, &mut self.focus_events);
                    self.parser.process(&data);
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
        std::mem::take(&mut self.osc52_pending)
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

/// Scan raw PTY output for OSC 52 clipboard sequences and collect them.
fn scan_osc52(data: &[u8], out: &mut Vec<Vec<u8>>) {
    const PREFIX: &[u8] = b"\x1b]52;";
    let mut i = 0;
    while i + PREFIX.len() < data.len() {
        if data[i..].starts_with(PREFIX) {
            let start = i;
            i += PREFIX.len();
            // Find terminator: BEL (\x07) or ST (\x1b\\)
            while i < data.len() {
                if data[i] == 0x07 {
                    out.push(data[start..=i].to_vec());
                    break;
                }
                if i + 1 < data.len() && data[i] == 0x1b && data[i + 1] == b'\\' {
                    out.push(data[start..i + 2].to_vec());
                    i += 1;
                    break;
                }
                i += 1;
            }
        }
        i += 1;
    }
}

/// Track DEC private mode changes in raw PTY output.
fn track_dec_modes(data: &[u8], bracketed_paste: &mut bool, focus_events: &mut bool) {
    // \x1b[?2004h = enable bracketed paste, \x1b[?2004l = disable
    // \x1b[?1004h = enable focus events, \x1b[?1004l = disable
    const BP_ON: &[u8] = b"\x1b[?2004h";
    const BP_OFF: &[u8] = b"\x1b[?2004l";
    const FE_ON: &[u8] = b"\x1b[?1004h";
    const FE_OFF: &[u8] = b"\x1b[?1004l";

    for window in data.windows(BP_ON.len().max(FE_ON.len())) {
        if window.starts_with(BP_ON) {
            *bracketed_paste = true;
        } else if window.starts_with(BP_OFF) {
            *bracketed_paste = false;
        } else if window.starts_with(FE_ON) {
            *focus_events = true;
        } else if window.starts_with(FE_OFF) {
            *focus_events = false;
        }
    }
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
