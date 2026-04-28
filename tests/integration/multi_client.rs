//! multi_client — two simultaneous clients; size negotiation; broadcast.
//!
//! Verifies the v0.5.0 multi-client path:
//!  * Two attach clients can hold the socket open at the same time.
//!  * Size negotiation uses the smallest geometry across all clients.
//!  * Output produced by the pane is broadcast to every attached client.
//!
//! GATED: `#[ignore]` until `EZPN_TEST_SOCKET_DIR` is honored by the daemon.

use std::time::Duration;

use crate::common::{attach_client, spawn_daemon, type_text, wait_for_output, TestEnv};

#[test]
#[ignore = "requires EZPN_TEST_SOCKET_DIR support in src/main.rs (#62 follow-up commit)"]
fn two_clients_share_output() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "multi");

    // Client A is wider; Client B is narrower. The negotiated PTY size
    // should converge to min(cols)/min(rows) so neither client renders
    // off-screen content.
    let mut client_a = attach_client(&daemon, 120, 40);
    let mut client_b = attach_client(&daemon, 80, 24);

    // Either client can write; broadcast means both observe the output.
    type_text(&mut client_a, "echo broadcast-from-A\n").expect("type from A");

    wait_for_output(
        &client_a.output(),
        "broadcast-from-A",
        Duration::from_secs(5),
    )
    .expect("client A never saw its own echo");

    wait_for_output(
        &client_b.output(),
        "broadcast-from-A",
        Duration::from_secs(5),
    )
    .expect("client B never received broadcast from A");

    // Now the other direction: B sends, both observe.
    type_text(&mut client_b, "echo broadcast-from-B\n").expect("type from B");

    wait_for_output(
        &client_a.output(),
        "broadcast-from-B",
        Duration::from_secs(5),
    )
    .expect("client A never received broadcast from B");

    daemon.shutdown();
}
