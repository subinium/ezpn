//! Thin client that proxies terminal I/O to an ezpn server.

use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::{
    cursor,
    event::{
        self, Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::protocol;

/// Reason the client loop exited.
pub enum ExitReason {
    /// Server sent detach acknowledgement.
    Detached,
    /// Server is shutting down.
    ServerExit,
    /// Connection to server was lost.
    ConnectionLost,
}

/// Connect to a running server and act as a terminal proxy.
pub fn run(socket_path: &std::path::Path, session_name: &str) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket_path)?;
    stream.set_nonblocking(false)?;

    let write_stream = stream.try_clone()?;
    let read_stream = stream;
    // No read timeout on the reader stream — the server may be idle for
    // long periods (no PTY output). The reader thread exits naturally when
    // the socket is closed (server exit or client disconnect drops the write half).

    // Start reader thread: server → client
    let (server_tx, server_rx) = mpsc::channel::<(u8, Vec<u8>)>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(read_stream);
        while let Ok(msg) = protocol::read_msg(&mut reader) {
            if server_tx.send(msg).is_err() {
                break;
            }
        }
    });

    // Enter raw mode + alternate screen + enhanced keyboard (for Shift+Enter etc.)
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        event::EnableMouseCapture,
        event::EnableFocusChange,
        event::EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        cursor::Hide
    )?;

    // Set terminal title to show session name
    let _ = write!(stdout, "\x1b]0;ezpn: {}\x07", session_name);
    let _ = stdout.flush();

    let reason = client_loop(&mut stdout, write_stream, &server_rx);

    // Cleanup terminal FIRST — before printing any messages
    {
        let mut out = io::stdout();
        let _ = write!(out, "\x1b]0;\x07"); // Restore terminal title
        let _ = execute!(
            out,
            PopKeyboardEnhancementFlags,
            event::DisableBracketedPaste,
            event::DisableFocusChange,
            cursor::Show,
            event::DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
    let _ = terminal::disable_raw_mode();

    // Now print status message (after terminal is restored)
    match reason {
        Ok(ExitReason::Detached) => {
            println!("[detached from session {}]", session_name);
        }
        Ok(ExitReason::ServerExit) => {
            println!("[session {} ended]", session_name);
        }
        Ok(ExitReason::ConnectionLost) => {
            return Err(anyhow::anyhow!("server connection lost"));
        }
        Err(e) => return Err(e),
    }

    Ok(())
}

fn client_loop(
    stdout: &mut io::Stdout,
    mut writer: UnixStream,
    server_rx: &mpsc::Receiver<(u8, Vec<u8>)>,
) -> anyhow::Result<ExitReason> {
    // Send initial terminal size
    let (cols, rows) = terminal::size()?;
    let resize_data = protocol::encode_resize(cols, rows);
    protocol::write_msg(&mut writer, protocol::C_RESIZE, &resize_data)?;

    loop {
        // 1. Process server messages — batch all output, flush once
        let mut got_output = false;
        loop {
            match server_rx.try_recv() {
                Ok((tag, payload)) => match tag {
                    protocol::S_OUTPUT => {
                        stdout.write_all(&payload)?;
                        got_output = true;
                    }
                    protocol::S_DETACHED => {
                        return Ok(ExitReason::Detached);
                    }
                    protocol::S_EXIT => {
                        return Ok(ExitReason::ServerExit);
                    }
                    _ => {}
                },
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Ok(ExitReason::ConnectionLost);
                }
            }
        }
        if got_output {
            stdout.flush()?;
        }

        // 2. Read terminal events and forward to server
        while event::poll(Duration::from_millis(8))? {
            let ev = event::read()?;

            match &ev {
                Event::Resize(w, h) => {
                    let data = protocol::encode_resize(*w, *h);
                    if protocol::write_msg(&mut writer, protocol::C_RESIZE, &data).is_err() {
                        return Ok(ExitReason::ConnectionLost);
                    }
                }
                _ => {
                    let json = serde_json::to_vec(&ev)?;
                    if protocol::write_msg(&mut writer, protocol::C_EVENT, &json).is_err() {
                        return Ok(ExitReason::ConnectionLost);
                    }
                }
            }
        }
    }
}
