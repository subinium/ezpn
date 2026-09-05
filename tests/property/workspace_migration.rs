//! Exercise the production migration CLI, not serde_json::Value identity.
use crate::common::{run_bounded, TestEnv};
use proptest::prelude::*;
use serde_json::{json, Value};

fn snapshot(panes: usize, version: u32) -> Value {
    let mut root = json!({"Leaf": {"id": 0}});
    for id in 1..panes {
        root = json!({"Split": {"direction": "horizontal", "ratio": 0.5, "first": root, "second": {"Leaf": {"id": id}}}});
    }
    json!({
        "version": version, "shell": "/bin/sh", "border_style": "rounded",
        "show_status_bar": true, "show_tab_bar": true, "scrollback": 1000,
        "active_tab": 0, "tabs": [{"name": "main", "layout": {"root": root, "next_id": panes},
            "active_pane": 0, "panes": (0..panes).map(|id| json!({"id": id, "launch": "shell"})).collect::<Vec<_>>() }]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]
    #[test]
    fn production_migration_is_idempotent(panes in 1usize..6, key in "future_[a-z]{3,12}") {
        let env = TestEnv::new();
        let path = env.root().join("snapshot.json");
        let mut value = snapshot(panes, 2);
        value[key] = json!({"nested": "additive"});
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let output = run_bounded(env.command().arg("upgrade-snapshot").arg(&path));
        prop_assert!(output.status.success(), "{output:?}");
        let first: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        prop_assert_eq!(&first["version"], &json!(3));
        prop_assert_eq!(first["tabs"][0]["panes"].as_array().unwrap().len(), panes);
        let output = run_bounded(env.command().arg("upgrade-snapshot").arg(&path));
        prop_assert!(output.status.success(), "{output:?}");
        let second: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        prop_assert_eq!(first, second);
    }

    #[test]
    fn production_migration_rejects_future_version_without_overwriting(version in 4u32..1000) {
        let env = TestEnv::new();
        let path = env.root().join("future.json");
        let bytes = serde_json::to_vec(&snapshot(1, version)).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let output = run_bounded(env.command().arg("upgrade-snapshot").arg(&path));
        prop_assert!(!output.status.success());
        prop_assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}
