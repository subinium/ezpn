//! attach_smoke — daemon spawn → attach → echo → assert pane output.
//!
//! Smoke test for the happy path: a freshly spawned daemon accepts an
//! attach client, the client types `echo hello`, and the daemon's pane
//! echoes that text back through the framed protocol.
//!
//! GATED: depends on `EZPN_TEST_SOCKET_DIR` being honored by the daemon.
//! Remove `#[ignore]` once that wiring lands in `src/main.rs`.

use std::time::Duration;

use crate::common::{attach_client, spawn_daemon, type_text, wait_for_output, TestEnv};

#[test]
#[ignore = "requires EZPN_TEST_SOCKET_DIR support in src/main.rs (#62 follow-up commit)"]
fn attach_smoke_echo_hello() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "smoke");

    // Attach a single client at a known size so the test is reproducible
    // across CI hosts with different default terminal dimensions.
    let mut client = attach_client(&daemon, 80, 24);

    // The shell is `/bin/sh` (forced by spawn_daemon). Type a literal echo
    // command and wait for the output to land in the pane.
    type_text(&mut client, "echo ezpn-smoke-marker\n").expect("type into pane");

    wait_for_output(
        &client.output(),
        "ezpn-smoke-marker",
        Duration::from_secs(5),
    )
    .expect("pane output never echoed marker");

    // Explicit cleanup; Drop on `daemon` would also handle this, but we
    // exercise the path here so panics in later tests still tear down clean.
    daemon.shutdown();
}
