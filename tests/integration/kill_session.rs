//! kill_session — `ezpn kill <name>` removes the socket and tears down children.
//!
//! Verifies the documented kill semantics:
//!   1. Spawn a daemon, confirm the socket exists.
//!   2. Run `ezpn kill <session>` against the same socket directory.
//!   3. Wait for the socket file to disappear.
//!   4. Confirm the daemon process exits (the helper's `Drop` will reap it,
//!      but we sanity-check that a follow-up `ezpn ls` no longer reports
//!      the session).
//!
//! GATED: `#[ignore]` until `EZPN_TEST_SOCKET_DIR` is honored by the daemon.

use std::time::Duration;

use crate::common::{kill_session, ls, spawn_daemon, wait_for, TestEnv};

#[test]
#[ignore = "requires EZPN_TEST_SOCKET_DIR support in src/main.rs (#62 follow-up commit)"]
fn kill_removes_socket_and_session() {
    let env = TestEnv::new();
    let daemon = spawn_daemon(&env, "killable");
    let socket = daemon.socket.clone();

    // Sanity: the socket exists before we ask for kill.
    assert!(
        socket.exists(),
        "expected socket at {} after spawn",
        socket.display()
    );

    let out = kill_session(&env, "killable");
    assert!(
        out.status.success(),
        "ezpn kill failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The kill command returns once it has signaled the daemon. The actual
    // socket removal is racy, so we wait with a bounded retry rather than
    // sleeping a fixed amount.
    wait_for(
        "socket file removed after kill",
        Duration::from_secs(5),
        || if !socket.exists() { Some(()) } else { None },
    )
    .expect("socket was not cleaned up after kill");

    // `ezpn ls` should no longer mention the session name.
    let listing = ls(&env);
    assert!(
        !listing.contains("killable"),
        "ezpn ls still reports killed session: {}",
        listing
    );

    // Drop the handle explicitly so a stale `child.wait()` doesn't hang
    // the test process if the daemon has already exited cleanly.
    drop(daemon);
}
