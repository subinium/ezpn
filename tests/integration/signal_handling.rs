//! SIGTERM must persist real live session state before successful exit.
use crate::common::*;
use std::io::Read;

#[test]
fn sigterm_persists_workspace_snapshot() {
    let env = TestEnv::new();
    let config = env.root().join("config/ezpn");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[global]\npersist_scrollback = true\n",
    )
    .unwrap();
    let mut daemon = spawn_daemon(&env, "sigterm");
    let mut client = attach_client(&daemon, 80, 24);
    type_text(&mut client, "printf 'snapshot-%s\\n' ready\n").unwrap();
    wait_for_output(&client.output(), "snapshot-ready", DEFAULT_TIMEOUT).unwrap();
    daemon.signal(libc::SIGTERM);
    assert!(daemon.wait_exit().success());
    let snapshot = env.snapshot_dir().join("sigterm.json");
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(snapshot).expect("SIGTERM snapshot missing"))
            .unwrap();
    assert_eq!(value["version"], 3);
    assert_eq!(value["tabs"].as_array().unwrap().len(), 1);
    assert_eq!(value["tabs"][0]["panes"].as_array().unwrap().len(), 1);
    let blob = &value["tabs"][0]["panes"][0]["scrollback"];
    assert_eq!(blob["encoding"], "bincode-gz");
    let compressed: Vec<u8> = serde_json::from_value(blob["payload"].clone()).unwrap();
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .take(1024 * 1024)
        .read_to_end(&mut raw)
        .unwrap();
    let rows: Vec<(String, Vec<u8>)> = bincode::deserialize(&raw).unwrap();
    assert!(
        rows.iter().any(|(text, _)| text.contains("snapshot-ready")),
        "SIGTERM snapshot lost real terminal output"
    );
    assert!(!daemon.socket.exists());
}
