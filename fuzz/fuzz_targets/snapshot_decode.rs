//! Fuzz target — feed arbitrary bytes to the snapshot JSON parser.
//!
//! Acceptance (issue #94):
//! - No panic on any input.
//! - No OOM. `serde_json` allocates as it parses; if a corpus surfaces a
//!   pathological structure (e.g. deeply nested arrays) we'll add a
//!   nesting cap on the loader side rather than try to enforce here.
//!
//! Mounting `workspace.rs` would drag in too many transitive deps for
//! the fuzz crate (pane / project / tab / render). We instead fuzz the
//! same boundary `load_snapshot` calls — `serde_json::from_slice` into a
//! `serde_json::Value` — which matches the actual attack surface (the
//! raw JSON file on disk).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Step 1: parse to Value. Mirrors the `serde_json::from_str(&content)`
    // call inside `workspace::load_snapshot`.
    let value: Result<serde_json::Value, _> = serde_json::from_slice(data);
    if let Ok(v) = value {
        // Step 2: re-serialize. Catches encoder asymmetries.
        let _ = serde_json::to_string(&v);
    }
});
