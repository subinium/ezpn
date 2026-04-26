//! Daemon internals split out of the legacy `server.rs` monolith.
//!
//! Module map:
//! - [`state`]      — input mode, tab actions, drag/selection, client struct
//! - [`router`]     — socket accept, reader thread, smallest-client policy
//! - [`snapshot`]   — workspace snapshot capture helper
//! - [`render`]     — frame composition into the shared output buffer
//! - [`dispatch`]   — `process_event`, `process_mouse`, `execute_command`
//! - [`keys`]       — `process_key` (single ~625-line handler; spec deviation)
//! - [`event_loop`] — the daemon's main `run()` loop
//!
//! `pub fn run(...)` is the only public entry point and is re-exported here
//! so `crate::server` can keep a thin `pub use crate::daemon::run;`.

pub(crate) mod dispatch;
pub(crate) mod event_loop;
pub(crate) mod events;
pub(crate) mod hooks;
pub(crate) mod keys;
pub(crate) mod render;
pub(crate) mod router;
pub(crate) mod snapshot;
pub(crate) mod snapshot_worker;
pub(crate) mod state;
pub(crate) mod writer;

pub use event_loop::run;
