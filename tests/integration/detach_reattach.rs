//! detach_reattach — detach mid-session, reattach, verify state preserved.
//!
//! The daemon owns the PTY, so disconnecting and reconnecting a client
//! should not lose buffered output. We:
//!   1. Spawn a daemon, attach, write a marker, observe it.
//!   2. Detach the first client cleanly.
//!   3. Reattach a second client and verify the marker is replayed
//!      (or, at minimum, that the pane is still alive and producing output).
//!
//! GATED: `#[ignore]` until `EZPN_TEST_SOCKET_DIR` is honored by the daemon.

use std::time::Duration;

use crate::common::{attach_client, spawn_daemon, type_text, wait_for_output, TestEnv};

#[test]
#[ignore = "requires EZPN_TEST_SOCKET_DIR support in src/main.rs (#62 follow-up commit)"]
fn detach_then_reattach_preserves_state() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "detach");

    let marker = "ezpn-detach-marker-7421";

    // Phase 1: attach, write a marker, observe it.
    {
        let mut client = attach_client(&daemon, 80, 24);
        type_text(&mut client, &format!("echo {}\n", marker)).expect("type marker");
        wait_for_output(&client.output(), marker, Duration::from_secs(5))
            .expect("first client never saw marker");
        client.send_detach().expect("send detach");
        // Dropping `client` closes its socket halves, completing the detach.
    }

    // Phase 2: reattach. The daemon should still be alive and the pane
    // still producing output, so a fresh `echo` round-trip succeeds.
    let mut client2 = attach_client(&daemon, 80, 24);
    let marker2 = "ezpn-reattach-marker-9911";
    type_text(&mut client2, &format!("echo {}\n", marker2)).expect("type after reattach");
    wait_for_output(&client2.output(), marker2, Duration::from_secs(5))
        .expect("reattached client never saw second marker");

    daemon.shutdown();
}
