//! POSIX signal handling for the ezpn daemon.
//!
//! Provides the infrastructure (atomic flags, handler thread, JSON dump
//! helper) needed to react to SIGTERM, SIGHUP, SIGCHLD, and SIGUSR1.
//! `install()` spawns a dedicated thread that reads signals via
//! [`signal_hook::iterator::Signals`] and toggles the matching atomic on
//! [`SignalState`]. Each toggle is followed by a wake of the server's main
//! loop (via [`crate::pane::wake_main_loop`]) so the daemon picks up the
//! flag on its next iteration.
//!
//! # Wiring status (issue #56)
//!
//! This commit only provides the infrastructure. The actual policy —
//! - SIGTERM: graceful save of all session snapshots then `exit(0)` within
//!   500 ms,
//! - SIGHUP: reload `~/.config/ezpn/config.toml` and re-render,
//! - SIGCHLD: `waitpid(WNOHANG)` reap loop and exit-code marking,
//! - SIGUSR1: trigger [`dump_session_state`] with the live session list,
//!
//! is implemented in a follow-up commit by the parent agent because it
//! requires touching `server.rs` main loop. This module deliberately does
//! not import `server.rs` so the two changes stay reviewable in isolation.
//!
//! The `#[allow(dead_code)]` below silences "never used" warnings in the
//! interim window between this commit and the parent's wiring commit.
//! Remove it once `server.rs` calls `install()` and `dump_session_state`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use serde::Serialize;
use signal_hook::consts::signal::{SIGCHLD, SIGHUP, SIGTERM, SIGUSR1};
use signal_hook::iterator::Signals;

/// Sticky flags toggled by the signal handler thread.
///
/// Each flag is set to `true` when the corresponding signal arrives. The
/// server main loop is responsible for observing the flag, taking action,
/// and (if appropriate) clearing it back to `false` before resuming.
///
/// All fields use `Ordering::SeqCst` in the helpers below; callers may
/// pick a weaker ordering if they understand the implications.
#[derive(Debug, Default)]
pub struct SignalState {
    /// `SIGTERM`: request graceful shutdown (save snapshots then exit).
    pub sigterm: AtomicBool,
    /// `SIGHUP`: request configuration reload.
    pub sighup: AtomicBool,
    /// `SIGCHLD`: a child process exited; reap with `waitpid(WNOHANG)`.
    pub sigchld: AtomicBool,
    /// `SIGUSR1`: dump session state JSON for ad-hoc debugging.
    pub sigusr1: AtomicBool,
}

/// Install POSIX signal handlers for the daemon.
///
/// Registers a [`Signals`] iterator for `SIGTERM`, `SIGHUP`, `SIGCHLD`,
/// and `SIGUSR1`, then spawns a detached thread that reads signals
/// forever. For each signal the matching flag on the returned
/// [`SignalState`] is set, after which the server main loop is woken via
/// [`crate::pane::wake_main_loop`] so the policy handler runs on the next
/// iteration.
///
/// Returns the shared state (cloneable via [`Arc::clone`]) so other
/// modules — once the parent agent wires server.rs — can read flags from
/// any thread.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] if `signal-hook` cannot
/// register the handlers (typically only happens if the process has
/// already exhausted signal slots).
pub fn install() -> std::io::Result<Arc<SignalState>> {
    let state = Arc::new(SignalState::default());
    let state_for_thread = Arc::clone(&state);

    let mut signals = Signals::new([SIGTERM, SIGHUP, SIGCHLD, SIGUSR1])?;

    thread::Builder::new()
        .name("ezpn-signals".to_string())
        .spawn(move || {
            for sig in signals.forever() {
                match sig {
                    SIGTERM => state_for_thread.sigterm.store(true, Ordering::SeqCst),
                    SIGHUP => state_for_thread.sighup.store(true, Ordering::SeqCst),
                    SIGCHLD => state_for_thread.sigchld.store(true, Ordering::SeqCst),
                    SIGUSR1 => state_for_thread.sigusr1.store(true, Ordering::SeqCst),
                    _ => {
                        // Defensive: signal-hook only delivers signals we
                        // registered, so this branch should never fire.
                        continue;
                    }
                }
                crate::pane::wake_main_loop();
            }
        })?;

    Ok(state)
}

/// Write a JSON dump of `state` to
/// `$XDG_STATE_HOME/ezpn/dump-<pid>-<unix>.json`, falling back to
/// `$HOME/.local/state/ezpn/...` when `XDG_STATE_HOME` is unset (per the
/// XDG Base Directory spec).
///
/// Used by the SIGUSR1 handler. Accepts any [`Serialize`] value so the
/// caller chooses the dump shape; the parent agent will pass the live
/// session/workspace state once server.rs wiring lands.
///
/// Returns the path that was written.
///
/// # Errors
///
/// - `std::io::ErrorKind::NotFound` if neither `XDG_STATE_HOME` nor
///   `HOME` is set.
/// - Any I/O error from `create_dir_all`, `File::create`, or
///   `serde_json::to_writer`.
pub fn dump_session_state<T: Serialize>(state: &T) -> std::io::Result<PathBuf> {
    let dir = state_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "neither XDG_STATE_HOME nor HOME is set",
        )
    })?;
    std::fs::create_dir_all(&dir)?;

    let pid = std::process::id();
    let unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let path = dir.join(format!("dump-{pid}-{unix_secs}.json"));
    let file = std::fs::File::create(&path)?;
    serde_json::to_writer_pretty(file, state).map_err(std::io::Error::other)?;

    Ok(path)
}

/// Resolve the ezpn dump directory under `$XDG_STATE_HOME` or
/// `$HOME/.local/state` per the XDG Base Directory spec.
fn state_dir() -> Option<PathBuf> {
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        if !state.is_empty() {
            return Some(PathBuf::from(state).join("ezpn"));
        }
    }
    let home = std::env::var("HOME").ok()?;
    if home.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("ezpn"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_state_default_is_all_false() {
        let s = SignalState::default();
        assert!(!s.sigterm.load(Ordering::SeqCst));
        assert!(!s.sighup.load(Ordering::SeqCst));
        assert!(!s.sigchld.load(Ordering::SeqCst));
        assert!(!s.sigusr1.load(Ordering::SeqCst));
    }

    #[test]
    fn install_returns_arc_with_writable_flags() {
        // We only verify the returned Arc<SignalState> is shareable and
        // its flags can be flipped/observed. Actually delivering a real
        // SIGTERM in a unit test would race with the test harness's own
        // signal handling, so we exercise the data structure directly.
        let state = install().expect("install handlers");

        assert!(!state.sigterm.load(Ordering::SeqCst));
        state.sigterm.store(true, Ordering::SeqCst);
        assert!(state.sigterm.load(Ordering::SeqCst));

        // Arc is clone-shareable across threads.
        let cloned = Arc::clone(&state);
        cloned.sighup.store(true, Ordering::SeqCst);
        assert!(state.sighup.load(Ordering::SeqCst));
    }

    #[test]
    fn dump_session_state_writes_json_to_xdg_state_home() {
        // Use a per-test temp dir via XDG_STATE_HOME so we don't touch
        // the developer's real ~/.local/state.
        let tmp = std::env::temp_dir().join(format!(
            "ezpn-signals-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).expect("create tmp");

        // NOTE: this test mutates process-global env. It is the only
        // test in this module that touches XDG_STATE_HOME, so there is
        // no intra-module race; cross-module env races are accepted as
        // a known limitation of std::env::set_var on Rust 2021.
        let prev = std::env::var("XDG_STATE_HOME").ok();
        std::env::set_var("XDG_STATE_HOME", &tmp);

        #[derive(serde::Serialize)]
        struct Probe {
            kind: &'static str,
            n: u32,
        }
        let probe = Probe { kind: "test", n: 7 };

        let path = dump_session_state(&probe).expect("dump");
        assert!(path.exists(), "dump file should exist at {path:?}");
        assert!(
            path.starts_with(tmp.join("ezpn")),
            "dump path {path:?} should be under XDG_STATE_HOME/ezpn"
        );
        let body = std::fs::read_to_string(&path).expect("read dump");
        assert!(body.contains("\"kind\""));
        assert!(body.contains("\"test\""));
        assert!(body.contains("\"n\""));
        assert!(body.contains("7"));

        // Restore env.
        match prev {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
