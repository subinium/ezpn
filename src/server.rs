//! Server daemon entry point.
//!
//! The historical `server.rs` was a 2.7k-line monolith that owned the
//! event loop, client routing, snapshot save/load, render frame builder,
//! and the entire keybinding + command-palette dispatcher. Issue #24
//! split it across [`crate::daemon`] submodules and turned this file into
//! a thin orchestrator that only exposes the public `run()` entry point
//! used by `main.rs` when launched with `--server`.
//!
//! Why keep `crate::server` at all? `main.rs` calls `server::run(...)`
//! and bumping that path would be an incompatible API change for
//! anything that re-exports the binary's modules from a sister crate or
//! integration test. Re-exporting from `crate::daemon::run` here is
//! free at runtime.

pub use crate::daemon::run;
