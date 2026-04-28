//! Fuzz target — feed arbitrary bytes to the IPC frame decoder.
//!
//! Acceptance (issue #94):
//! - No panic on any input.
//! - No allocation > 100 MB (`MAX_PAYLOAD` is 16 MB; the decoder must
//!   reject larger declared lengths before allocating).
//!
//! Run: `cargo +nightly fuzz run protocol_decode -- -max_total_time=600`
//!      (CI runs 10 minutes per PR, per the issue's spec.)

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

#[path = "../../src/protocol.rs"]
mod protocol;

fuzz_target!(|data: &[u8]| {
    let mut cursor = Cursor::new(data);
    // Loop so a single fuzz input can stage multiple frames back-to-back —
    // matches what a malicious client might send. We bound the loop so a
    // pathological zero-length stream cannot run forever.
    for _ in 0..16 {
        match protocol::read_msg(&mut cursor) {
            Ok((_tag, _payload)) => continue,
            Err(_) => break,
        }
    }

    // Also exercise the resize codec — it has its own short-payload path.
    let _ = protocol::decode_resize(data);
});
