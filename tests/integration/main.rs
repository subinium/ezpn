//! Entry point for the ezpn integration test suite.
//!
//! Cargo treats every file under `tests/` as its own crate by default, which
//! makes sharing helpers awkward. We collect every integration scenario into
//! a single `integration` test target (configured in `Cargo.toml`) and pull
//! the helpers in via `#[path]` so `tests/common/mod.rs` stays at the
//! conventional location.
//!
//! All scenarios execute against isolated real processes and Unix sockets.
//! PTY scenarios also run the actual interactive client on a controlling TTY.

#![cfg(unix)]

#[path = "../common/mod.rs"]
mod common;

#[path = "../../src/vt100/mod.rs"]
mod vt100;

mod attach_smoke;
mod cli_reliability;
mod detach_reattach;
mod ipc_version;
mod kill_session;
mod multi_client;
mod signal_handling;
mod terminal_reliability;
