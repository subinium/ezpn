//! Entry point for the ezpn property-test suite (issue #94).
//!
//! Each module exercises invariants of one critical module. Tests use
//! [`proptest`] to fuzz inputs within `cargo test` so the regression net
//! catches edge cases the unit tests miss without needing libfuzzer.
//!
//! The suite is registered as the `property` test target in `Cargo.toml`,
//! which lets CI run it independently from the slower `integration` suite
//! when needed (e.g. coverage-only runs).

mod layout_invariants;
mod protocol_roundtrip;
mod workspace_migration;
