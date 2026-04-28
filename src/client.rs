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

/// Structured error returned when the server rejects us with `S_INCOMPAT`.
///
/// Held by `anyhow` so the user-facing message bubbles up via main.rs's
/// default error formatter. Exit code 2 specifically (per #57 UX spec)
/// requires a small main.rs change to map this error variant — that
/// wiring is intentionally out of scope for this commit (main.rs is
/// owned by another agent in the v0.12 split).
#[derive(Debug)]
pub struct IncompatibleServerError {
    // Retained for the main.rs follow-up that maps this error to exit
    // code 2 and surfaces structured diagnostics; not yet read here.
    #[allow(dead_code)]
    pub server_proto: String,
    #[allow(dead_code)]
    pub client_proto: String,
    pub message: String,
}

impl std::fmt::Display for IncompatibleServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IncompatibleServerError {}

/// Connect to a running server with a specific attach mode.
pub fn run_with_mode(
    socket_path: &std::path::Path,
    session_name: &str,
    attach_mode: protocol::AttachMode,
) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket_path)?;
    stream.set_nonblocking(false)?;

    // ── Version handshake (issue #57) ──
    //
    // Run BEFORE entering raw mode so any friendly incompat error prints
    // normally to stderr. We use a generous timeout (2s) on the handshake
    // read: a healthy server emits S_VERSION immediately on accept; a
    // longer wait means something is wrong and we'd rather fail fast than
    // hang. The timeout is cleared once the handshake completes so the
    // long-lived reader thread can block indefinitely on idle connections.
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let server_hello = perform_handshake(&stream, session_name)?;
    stream.set_read_timeout(None)?;

    // Stash the server's protocol version on a debug log line (one-shot).
    // A future commit will plumb this into `ezpn ls --json` via session
    // metadata; for now it's purely informational.
    if std::env::var("EZPN_DEBUG").is_ok() {
        eprintln!(
            "ezpn: handshake ok — server {}.{} ({})",
            server_hello.proto_major, server_hello.proto_minor, server_hello.build
        );
    }

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

/// Drive the client-side IPC handshake to completion before any other I/O.
///
/// Reads `S_VERSION`, sends `C_HELLO`. On `S_INCOMPAT` (or a major version
/// mismatch detected client-side) prints the friendly UX message to stderr
/// and returns an `IncompatibleServerError` wrapped in `anyhow`. On a
/// timeout (e.g. legacy v0.5 server that doesn't speak S_VERSION) the
/// underlying `io::Error` propagates — the auto-attach path in main.rs
/// will treat this as a stale session and fall through to spawning a new
/// server, which is the desired UX for the v0.5 → v0.12 transition.
fn perform_handshake(
    stream: &UnixStream,
    session_name: &str,
) -> anyhow::Result<protocol::ServerHello> {
    let mut reader = stream;
    let mut writer = stream;
    match protocol::client_handshake(&mut reader, &mut writer)? {
        protocol::HandshakeOutcome::Ok(hello) => Ok(hello),
        protocol::HandshakeOutcome::Incompat(notice) => {
            // Print the canonical message ahead of any anyhow formatting so
            // the user sees it cleanly even if main.rs's error path adds
            // its own prefix later.
            eprintln!("Error: {}", notice.message);
            // Hint with the session-scoped command spec'd in #57 in case
            // the server's message didn't include it (e.g. legacy notice).
            if !notice.message.contains("ezpn kill") {
                eprintln!("hint: ezpn kill {}", session_name);
            }
            Err(anyhow::Error::new(IncompatibleServerError {
                server_proto: notice.server_proto,
                client_proto: notice.client_proto,
                message: notice.message,
            }))
        }
    }
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
