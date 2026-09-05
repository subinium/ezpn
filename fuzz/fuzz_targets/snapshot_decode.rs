//! Exercise ezpn's real snapshot migration, validation and history codec.
#![no_main]
#![allow(dead_code, unused_imports)]
use libfuzzer_sys::fuzz_target;
#[path = "../../src/vt100/mod.rs"]
mod vt100;
#[path = "../../src/buffers.rs"]
mod buffers;
#[path = "../../src/clipboard.rs"]
mod clipboard;
#[path = "../../src/config.rs"]
mod config;
#[path = "../../src/copy_mode.rs"]
mod copy_mode;
#[path = "../../src/env_interp.rs"]
mod env_interp;
#[path = "../../src/fuzzy.rs"]
mod fuzzy;
#[path = "../../src/hooks.rs"]
mod hooks;
#[path = "../../src/keymap.rs"]
mod keymap;
#[path = "../../src/layout.rs"]
mod layout;
#[path = "../../src/pane.rs"]
mod pane;
#[path = "../../src/project.rs"]
mod project;
#[path = "../../src/render.rs"]
mod render;
#[path = "../../src/socket_security.rs"]
mod socket_security;
#[path = "../../src/tab.rs"]
mod tab;
#[path = "../../src/terminal_state.rs"]
mod terminal_state;
#[path = "../../src/theme.rs"]
mod theme;
#[path = "../../src/workspace.rs"]
mod workspace;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let Ok((snapshot, _)) = workspace::parse_snapshot_str(text) else { return };
    if snapshot.validate().is_err() { return; }
    for pane in snapshot.tabs.iter().flat_map(|tab| &tab.panes) {
        if let Some(blob) = &pane.scrollback { let _ = blob.decode(); }
    }
});
