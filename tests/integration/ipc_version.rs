//! Reject an incompatible client using the running daemon, not a codec mock.
use crate::common::*;
use std::io::Read;

#[test]
fn version_mismatch_is_rejected() {
    let env = TestEnv::new();
    let mut daemon = spawn_daemon(&env, "version");
    let mut stream = connect(&daemon);
    let version = hello(&mut stream, Some(u16::MAX as u64));
    let (tag, payload) = read_msg(&mut stream).expect("version rejection frame");
    assert_eq!(tag, 0x12, "expected S_INCOMPAT");
    let notice: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(
        notice["server_proto"],
        format!("{}.{}", version["proto_major"], version["proto_minor"])
    );
    assert!(notice["message"]
        .as_str()
        .unwrap()
        .contains("cannot attach"));
    assert_eq!(stream.read(&mut [0]).expect("EOF after rejection"), 0);
    daemon.assert_alive();
    assert!(ls(&env).contains("version"));
}
