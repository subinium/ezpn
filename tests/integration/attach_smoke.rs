//! attach_smoke — daemon spawn → attach → echo → assert pane output.
//!
//! Smoke test for the happy path: a freshly spawned daemon accepts an
//! attach client, the client types `echo hello`, and the daemon's pane
//! echoes that text back through the framed protocol.
//!

use std::time::Duration;

use crate::common::{attach_client, spawn_daemon, type_text, wait_for_output, TestEnv};

#[test]
fn attach_smoke_echo_hello() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "smoke");

    // Attach a single client at a known size so the test is reproducible
    // across CI hosts with different default terminal dimensions.
    let mut client = attach_client(&daemon, 80, 24);

    // The shell is `/bin/sh` (forced by spawn_daemon). Type a literal echo
    // command and wait for the output to land in the pane.
    // The full marker is absent from the input, so terminal echo cannot pass.
    type_text(&mut client, "printf 'ezpn-smoke-%s\\n' marker\n").expect("type into pane");

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
