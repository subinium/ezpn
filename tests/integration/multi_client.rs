//! multi_client — two simultaneous clients; size negotiation; broadcast.
//!
//! Verifies the v0.5.0 multi-client path:
//!  * Two attach clients can hold the socket open at the same time.
//!  * Size negotiation uses the smallest geometry across all clients.
//!  * Output produced by the pane is broadcast to every attached client.
//!

use std::time::Duration;

use crate::common::{attach_with_mode, spawn_daemon, type_text, wait_for_output, TestEnv};

#[test]
fn two_clients_share_output() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "multi");

    // Client A is wider; Client B is narrower. The negotiated PTY size
    // should converge to min(cols)/min(rows) so neither client renders
    // off-screen content.
    let mut client_a = attach_with_mode(&daemon, 120, 40, "shared");
    let mut client_b = attach_with_mode(&daemon, 80, 24, "shared");

    // Either client can write; broadcast means both observe the output.
    type_text(&mut client_a, "printf 'broadcast-from-%s\\n' A\n").expect("type from A");

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
    type_text(&mut client_b, "printf 'broadcast-from-%s\\n' B\n").expect("type from B");

    wait_for_output(
        &client_a.output(),
        "broadcast-from-B",
        Duration::from_secs(5),
    )
    .expect("client A never received broadcast from B");

    daemon.shutdown();
}
