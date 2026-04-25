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
/// Uses legacy C_RESIZE handshake (steal mode).
pub fn run(socket_path: &std::path::Path, session_name: &str) -> anyhow::Result<()> {
    run_with_mode(socket_path, session_name, protocol::AttachMode::Steal)
}

/// Connect to a running server with a specific attach mode.
pub fn run_with_mode(
    socket_path: &std::path::Path,
    session_name: &str,
    attach_mode: protocol::AttachMode,
) -> anyhow::Result<()> {
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
        // Isolate reader panics so the client UI shuts down cleanly instead of
        // aborting (which would leave the host terminal in raw mode).
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut reader = BufReader::new(read_stream);
            while let Ok(msg) = protocol::read_msg(&mut reader) {
                if server_tx.send(msg).is_err() {
                    break;
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
            eprintln!("ezpn: client reader thread panicked: {}", reason);
            // server_tx drops here → main loop sees Disconnected and exits cleanly.
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

    let reason = client_loop(&mut stdout, write_stream, &server_rx, attach_mode);

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
    attach_mode: protocol::AttachMode,
) -> anyhow::Result<ExitReason> {
    // Send initial handshake with terminal size and attach mode
    let (cols, rows) = terminal::size()?;
    if attach_mode == protocol::AttachMode::Steal {
        // Legacy handshake for backward compatibility
        let resize_data = protocol::encode_resize(cols, rows);
        protocol::write_msg(&mut writer, protocol::C_RESIZE, &resize_data)?;
    } else {
        // New protocol with attach mode
        let req = protocol::AttachRequest {
            cols,
            rows,
            mode: attach_mode,
        };
        let json = serde_json::to_vec(&req)?;
        protocol::write_msg(&mut writer, protocol::C_ATTACH, &json)?;
    }

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
        // Use 1ms poll when we had output (expect more soon), 4ms otherwise.
        // This reduces input latency vs the previous 8ms fixed poll.
        let poll_ms = if got_output { 1 } else { 4 };
        while event::poll(Duration::from_millis(poll_ms))? {
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
