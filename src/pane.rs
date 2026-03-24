use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
        })
    }

    /// Read pending output from PTY. Returns true if new data was received.
    pub fn read_output(&mut self) -> bool {
        let was_alive = self.alive;
        let mut got_data = false;
        loop {
            match self.reader_rx.try_recv() {
                Ok(data) => {
                    self.parser.process(&data);
                    // New output snaps scroll to bottom
                    if self.scroll_offset > 0 {
                        self.scroll_offset = 0;
                    }
                    got_data = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.alive = false;
                    break;
                }
            }
        }
        if self.alive {
            if let Ok(Some(_)) = self.child.try_wait() {
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
        if let Some(pid) = self.child.process_id() {
            unsafe {
                // Send to process group (negative PID) to reach shell's children (e.g. claude)
                libc::kill(-(pid as libc::pid_t), libc::SIGWINCH);
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
        match &self.launch {
            PaneLaunch::Shell => shell.to_string(),
            PaneLaunch::Command(command) => command.clone(),
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
}

fn encode_key(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char(c) if ctrl => {
            let byte = (c.to_ascii_lowercase() as u8)
                .wrapping_sub(b'a')
                .wrapping_add(1);
            if alt {
                vec![0x1b, byte]
            } else {
                vec![byte]
            }
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            if alt {
                let mut v = vec![0x1b];
                v.extend_from_slice(s.as_bytes());
                v
            } else {
                s.as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                vec![0x1b, b'[', b'Z'] // Shift+Tab = reverse tab
            } else {
                vec![b'\t']
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => esc_bracket(b'A'),
        KeyCode::Down => esc_bracket(b'B'),
        KeyCode::Right => esc_bracket(b'C'),
        KeyCode::Left => esc_bracket(b'D'),
        KeyCode::Home => vec![0x1b, b'[', b'H'],
        KeyCode::End => vec![0x1b, b'[', b'F'],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Insert => vec![0x1b, b'[', b'2', b'~'],
        KeyCode::F(n) => encode_f_key(n),
        _ => vec![],
    }
}

fn esc_bracket(code: u8) -> Vec<u8> {
    vec![0x1b, b'[', code]
}

fn encode_f_key(n: u8) -> Vec<u8> {
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
