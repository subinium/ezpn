//! Foreground (`--no-daemon`) runtime.
//!
//! Submodules carve up what used to live in `main.rs`:
//! - [`state`]: tiny helper structs (`InputMode`, `RenderUpdate`, drag/selection).
//! - [`render_ctl`]: frame composition + dirty-set bookkeeping.
//! - [`lifecycle`]: pane lifecycle helpers (split/spawn/replace/resize/restore).
//! - [`bootstrap`]: initial workspace bring-up (snapshot/project/Procfile/grid).
//! - [`attach`]: subcommand handlers (`cmd_*`) reachable from `main`.
//! - [`input_dispatch`]: out-of-band IPC command dispatch.
//! - [`event_loop`]: the main `run` loop that wires it all together.
//!
//! Daemon-side rendering and dispatch still live in `server.rs` (#17).

pub(crate) mod attach;
pub(crate) mod bootstrap;
pub(crate) mod event_loop;
pub(crate) mod input_dispatch;
pub(crate) mod lifecycle;
pub(crate) mod render_ctl;
pub(crate) mod state;
