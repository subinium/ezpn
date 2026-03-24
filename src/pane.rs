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
}

impl Pane {
    pub fn new(shell: &str, cols: u16, rows: u16) -> anyhow::Result<Self> {
        Self::spawn(shell, PaneLaunch::Shell, cols, rows)
    }

    pub fn with_command(shell: &str, command: &str, cols: u16, rows: u16) -> anyhow::Result<Self> {
        Self::spawn(shell, PaneLaunch::Command(command.to_string()), cols, rows)
    }

    #[allow(dead_code)]
    pub fn with_scrollback(
        shell: &str,
        launch: PaneLaunch,
        cols: u16,
        rows: u16,
        scrollback: usize,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(shell, launch, cols, rows, scrollback)
    }

    fn spawn(shell: &str, launch: PaneLaunch, cols: u16, rows: u16) -> anyhow::Result<Self> {
        Self::spawn_inner(shell, launch, cols, rows, 0)
    }

    fn spawn_inner(
        shell: &str,
        launch: PaneLaunch,
        cols: u16,
        rows: u16,
        scrollback: usize,
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
        cmd.env("TERM", "xterm-256color");
        cmd.env("EZPN", "1"); // prevent nesting

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
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.parser.set_size(rows, cols);
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn is_alive(&self) -> bool {
        self.alive
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        self.alive = false;
    }

    #[allow(dead_code)]
    pub fn scroll_up(&mut self, lines: usize) {
        let max = self.parser.screen().scrollback();
        self.scroll_offset = (self.scroll_offset + lines).min(max);
    }

    #[allow(dead_code)]
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    #[allow(dead_code)]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    #[allow(dead_code)]
    pub fn is_scrolled(&self) -> bool {
        self.scroll_offset > 0
    }

    #[allow(dead_code)]
    pub fn snap_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn launch(&self) -> &PaneLaunch {
        &self.launch
    }

    pub fn launch_label(&self, shell: &str) -> String {
        match &self.launch {
            PaneLaunch::Shell => shell.to_string(),
            PaneLaunch::Command(command) => command.clone(),
        }
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
