//! Key-spec parsing & key-table dispatch helpers.
//!
//! Currently this module owns just the `keyspec` parser used by SPEC 06
//! `send-keys`. SPEC 09 (custom keymap TOML) will share the same `keyspec`
//! grammar — keep the parser self-contained so both consumers route
//! through one canonical implementation.

pub mod keyspec;
