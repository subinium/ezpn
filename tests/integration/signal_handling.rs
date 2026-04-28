//! signal_handling — placeholder.
//!
//! Coverage spec: SIGTERM should trigger a graceful shutdown that flushes a
//! workspace snapshot to disk before the daemon exits. Implementation lives
//! behind issue #56 (signal handling rework). Once that lands, replace this
//! placeholder with a real test that:
//!   1. Spawns a daemon, attaches a client, writes a marker.
//!   2. Sends SIGTERM to the daemon process.
//!   3. Waits for the snapshot file to appear inside `env.snapshot_dir()`.
//!   4. Asserts the snapshot contains the pane state (marker text, layout).
//!
//! GATED: depends on #56 AND on `EZPN_TEST_SOCKET_DIR` (#62 follow-up commit).

#[test]
#[ignore = "depends on #56 (signal handling) — not yet merged; placeholder per #62"]
fn sigterm_persists_workspace_snapshot() {
    // Intentionally empty. Asserting `false` here would mask the signal
    // that #56 is still open: the test runner already reports `ignored`
    // status when the upstream feature lands, prompting us to fill this in.
}
