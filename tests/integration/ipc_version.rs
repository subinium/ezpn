//! ipc_version — placeholder.
//!
//! Coverage spec: clients and servers should exchange a version handshake on
//! attach, and a mismatch should produce a structured rejection rather than
//! a silent protocol error. The version handshake itself is tracked by
//! issue #57.
//!
//! Once #57 lands, replace this placeholder with a real test that:
//!   1. Spawns a daemon advertising protocol version `N`.
//!   2. Connects via raw `UnixStream`, sends a forged handshake with
//!      version `N + 1`.
//!   3. Asserts the daemon responds with a versioned rejection frame and
//!      closes the socket cleanly (no panic, no resource leak).
//!
//! GATED: depends on #57 AND on `EZPN_TEST_SOCKET_DIR` (#62 follow-up commit).

#[test]
#[ignore = "depends on #57 (IPC version handshake) — not yet merged; placeholder per #62"]
fn version_mismatch_is_rejected() {
    // Intentionally empty. See module docs for the planned shape.
}
