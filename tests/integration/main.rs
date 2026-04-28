//! Entry point for the ezpn integration test suite.
//!
//! Cargo treats every file under `tests/` as its own crate by default, which
//! makes sharing helpers awkward. We collect every integration scenario into
//! a single `integration` test target (configured in `Cargo.toml`) and pull
//! the helpers in via `#[path]` so `tests/common/mod.rs` stays at the
//! conventional location.
//!
//! Every scenario in this suite spawns the real `ezpn` binary and talks to
//! it over a real Unix socket. They depend on the daemon honoring the
//! `EZPN_TEST_SOCKET_DIR` environment variable so each test uses an isolated
//! tempdir. Until that wiring lands in `src/main.rs` (tracked in this same
//! issue), each scenario is gated with `#[ignore]` so `cargo test` is green
//! by default.

#[path = "../common/mod.rs"]
mod common;

mod attach_smoke;
mod detach_reattach;
mod ipc_version;
mod kill_session;
mod multi_client;
mod signal_handling;
