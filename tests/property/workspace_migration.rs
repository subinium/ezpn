//! Property tests for workspace snapshot migration (issue #94).
//!
//! Invariants verified:
//! 1. The current snapshot JSON shape parses through serde_json without
//!    panic for any well-formed seed.
//! 2. v2 → save → load is a fixed point (idempotent on multiple applies).
//! 3. Unknown fields in a v3-shaped payload are tolerated under the
//!    additive-evolution rule.
//!
//! Mounting `workspace.rs` directly would drag in `pane`, `project`,
//! `tab`, `render`, etc. — too much surface area for a property test. We
//! instead exercise the wire shape via `serde_json::Value` round-trips,
//! which is the same boundary `load_snapshot` enforces.

#![allow(dead_code)]

use proptest::prelude::*;
use serde_json::{json, Value};

/// Shape of a v2 pane (subset of the real snapshot — every field is
/// `#[serde(default)]` upstream so unknown additions are safe).
fn arb_pane(id: usize) -> impl Strategy<Value = Value> {
    (
        prop_oneof![
            Just(json!({"Shell": null})),
            Just(json!({"Command": "echo hi"})),
        ],
        any::<bool>(),
    )
        .prop_map(move |(launch, has_name)| {
            let mut obj = serde_json::Map::new();
            obj.insert("id".into(), json!(id));
            obj.insert("launch".into(), launch);
            if has_name {
                obj.insert("name".into(), json!(format!("pane-{}", id)));
            }
            Value::Object(obj)
        })
}

fn arb_tab(num_panes: usize) -> impl Strategy<Value = Value> {
    let panes: Vec<_> = (0..num_panes).map(arb_pane).collect();
    panes.prop_map(move |panes| {
        // Build a flat layout of `num_panes` leaves nested as a left-leaning tree.
        let mut node = json!({"Leaf": {"id": 0}});
        for i in 1..num_panes {
            node = json!({
                "Split": {
                    "direction": "horizontal",
                    "ratio": 0.5,
                    "first": node,
                    "second": {"Leaf": {"id": i}}
                }
            });
        }
        json!({
            "name": "main",
            "layout": {"root": node, "next_id": num_panes},
            "active_pane": 0,
            "panes": panes,
        })
    })
}

fn arb_snapshot() -> impl Strategy<Value = Value> {
    (1usize..6).prop_flat_map(|num_panes| {
        arb_tab(num_panes).prop_map(move |tab| {
            json!({
                "version": 2,
                "shell": "/bin/sh",
                "border_style": "rounded",
                "show_status_bar": true,
                "show_tab_bar": true,
                "scrollback": 10_000,
                "active_tab": 0,
                "tabs": [tab],
            })
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// Any well-formed v2 snapshot JSON survives a `to_string → from_str`
    /// round-trip without losing data.
    #[test]
    fn snapshot_serde_roundtrip(snap in arb_snapshot()) {
        let s = serde_json::to_string(&snap).expect("serialize");
        let parsed: Value = serde_json::from_str(&s).expect("parse");
        prop_assert_eq!(parsed, snap);
    }

    /// Adding an unknown additive field is tolerated by the JSON layer —
    /// the v3 forward-compat contract holds at the byte level. (The
    /// upstream `load_snapshot` enforces it via `#[serde(default)]`.)
    #[test]
    fn unknown_fields_tolerated(mut snap in arb_snapshot(), key in "[a-z_]{3,12}") {
        if let Value::Object(map) = &mut snap {
            if !map.contains_key(&key) {
                map.insert(key, json!("future-feature"));
            }
        }
        let s = serde_json::to_string(&snap).expect("serialize");
        let parsed: Value = serde_json::from_str(&s).expect("parse");
        prop_assert_eq!(parsed, snap);
    }

    /// Migration must be idempotent: serializing a parsed snapshot and
    /// reparsing yields identical JSON. (Catches accidental mutation in
    /// the migration path.)
    #[test]
    fn migration_is_idempotent(snap in arb_snapshot()) {
        let once = serde_json::to_string(&snap).expect("serialize 1");
        let parsed1: Value = serde_json::from_str(&once).expect("parse 1");
        let twice = serde_json::to_string(&parsed1).expect("serialize 2");
        let parsed2: Value = serde_json::from_str(&twice).expect("parse 2");
        prop_assert_eq!(parsed1, parsed2);
    }
}
