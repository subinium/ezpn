//! POSIX signal handling for the daemon.
//!
//! `signal-hook` registers the actual handlers (using a self-pipe internally)
//! so the only async-signal-safe work happens off-thread. The daemon's main
//! loop drains [`SignalHandlers::drain`] every tick and handles each fired
//! signal synchronously, where it's safe to call into the workspace, render
//! state, etc.
//!
//! Signals handled:
//! - `SIGTERM`, `SIGHUP` → graceful shutdown (auto-save snapshot, cleanup
//!   socket, exit cleanly). Replaces the previous behaviour of dying via
//!   default disposition (no snapshot, dangling socket).
//! - `SIGCHLD` → reap exited children. Without this, finished panes become
//!   zombies until the daemon also exits (memory + slot leak in long-lived
//!   sessions with many spawn/exit cycles).

#[cfg(unix)]
use signal_hook::consts::signal as sig;
#[cfg(unix)]
use signal_hook::iterator::Signals;

/// One firing of a registered signal. The set is intentionally small —
/// extending it requires updating both [`SignalHandlers::install`] and the
/// dispatch site in `server::run`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// SIGTERM or SIGHUP — caller should snapshot + cleanup + exit.
    Terminate,
    /// SIGCHLD — caller should iterate panes and reap zombies.
    Child,
}

#[cfg(unix)]
pub struct SignalHandlers {
    inner: Signals,
}

#[cfg(unix)]
impl SignalHandlers {
    /// Register handlers for SIGTERM, SIGHUP, SIGCHLD. Idempotent at the
    /// process level — the underlying `Signals` can be constructed multiple
    /// times in tests, but production code should call this exactly once.
    pub fn install() -> std::io::Result<Self> {
        let inner = Signals::new([sig::SIGTERM, sig::SIGHUP, sig::SIGCHLD])?;
        Ok(Self { inner })
    }

    /// Non-blocking drain — returns every signal that fired since the last
    /// call. Empty Vec if nothing fired (typical case in the hot loop).
    pub fn drain(&mut self) -> Vec<Signal> {
        let mut out = Vec::new();
        for s in self.inner.pending() {
            // Match by const value rather than pattern — uppercase
            // constants pulled in via `use sig` would otherwise be parsed
            // as variable bindings inside a `match` arm.
            if s == sig::SIGTERM || s == sig::SIGHUP {
                out.push(Signal::Terminate);
            } else if s == sig::SIGCHLD {
                out.push(Signal::Child);
            }
            // anything else: we never registered for it
        }
        out
    }
}

// Non-unix stub so the rest of the crate keeps compiling on non-POSIX
// platforms. ezpn currently only ships for macOS / Linux, but the file's
// callers are platform-neutral.
#[cfg(not(unix))]
pub struct SignalHandlers;

#[cfg(not(unix))]
impl SignalHandlers {
    pub fn install() -> std::io::Result<Self> {
        Ok(Self)
    }
    pub fn drain(&mut self) -> Vec<Signal> {
        Vec::new()
    }
}
