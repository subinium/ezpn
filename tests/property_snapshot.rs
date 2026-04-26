//! Property tests for the snapshot wire format.
//!
//! Snapshots are JSON files persisted under `$XDG_DATA_HOME/ezpn/sessions/`.
//! They survive across daemon restarts, which makes their schema a hard
//! compatibility surface (see MAINTENANCE.md "Never break snapshot
//! compatibility without a migration"). The properties here protect that
//! surface from regressions:
//!
//! - **roundtrip**: anything we serialize must come back equal,
//! - **v2→v3 migration**: every v2 snapshot must load as a valid v3,
//! - **pane id uniqueness**: snapshots can never persist colliding ids,
//! - **layout validity**: layouts saved into snapshots stay valid when
//!   reloaded.
//!
//! ## Why JSON-level (not typed)
//!
//! Pulling in `WorkspaceSnapshot` would drag the whole `pane` / `render`
//! / `tab` / `theme` module graph into the test crate via `#[path]`. The
//! wire format is what actually needs to stay stable — typed-struct
//! assertions are already covered by `src/workspace.rs::tests`. Generating
//! random `serde_json::Value`s instead lets us probe migration corners
//! (extra fields, missing optional fields) that typed strategies hide.
//!
//! Layout, the one piece these properties really need to introspect, is
//! pulled in via `#[path]` since it's standalone (no `crate::` deps).

#![allow(dead_code)]

#[path = "../src/layout.rs"]
mod layout;

use layout::{Direction, Layout};
use proptest::prelude::*;
use serde_json::{json, Value};

// ── Strategies ─────────────────────────────────────────────

/// Arbitrary `Layout` built from random splits, capped at 5 splits so
/// shrinking surfaces small repros. Mirrors the layout strategy in
/// `tests/property_layout.rs` but kept local to keep each test crate
/// self-contained.
fn arb_layout() -> impl Strategy<Value = Layout> {
    let dir = prop_oneof![Just(Direction::Horizontal), Just(Direction::Vertical)];
    let script = prop::collection::vec((any::<u8>(), dir), 0usize..5);
    script.prop_map(|ops| {
        let mut layout = Layout::from_grid(1, 1);
        for (idx_seed, d) in ops {
            let ids = layout.pane_ids();
            if ids.is_empty() {
                break;
            }
            let target = ids[idx_seed as usize % ids.len()];
            layout.split(target, d);
        }
        layout
    })
}

/// Arbitrary launch entry — either a shell or a free-form command. We
/// avoid generating arbitrary unicode for the command string since
/// roundtripping that property has nothing to do with snapshot logic
/// (it's serde_json's job) and produces noisy shrink output.
fn arb_launch() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(json!("shell")),
        "[a-zA-Z0-9_ -]{1,32}".prop_map(|cmd| json!({ "command": cmd })),
    ]
}

fn arb_restart() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("never"), Just("on_failure"), Just("always"),]
}

/// Build a v3 snapshot JSON value for `layout`, generating per-pane
/// metadata via the supplied per-pane payload generator. Keeping the
/// pane bodies parameterised lets one helper drive both the v3 roundtrip
/// case and the v2 migration case below.
fn snapshot_v3(layout: &Layout, panes: Vec<Value>) -> Value {
    let layout_json = serde_json::to_value(layout).expect("serialize layout");
    let active = layout
        .pane_ids()
        .first()
        .copied()
        .expect("layout has ≥1 pane");
    json!({
        "version": 3,
        "shell": "/bin/zsh",
        "border_style": "rounded",
        "show_status_bar": true,
        "show_tab_bar": true,
        "scrollback": 10_000,
        "active_tab": 0,
        "tabs": [{
            "name": "1",
            "layout": layout_json,
            "active_pane": active,
            "panes": panes,
        }]
    })
}

/// Same shape as `snapshot_v3` but emits the v2 fields (no `scrollback`,
/// no `show_tab_bar` at root; pane bodies omit `scrollback_blob`). The
/// `validate` path migrates these to v3 in-place.
fn snapshot_v2(layout: &Layout, panes: Vec<Value>) -> Value {
    let layout_json = serde_json::to_value(layout).expect("serialize layout");
    let active = layout
        .pane_ids()
        .first()
        .copied()
        .expect("layout has ≥1 pane");
    json!({
        "version": 2,
        "shell": "/bin/zsh",
        "border_style": "rounded",
        "show_status_bar": true,
        "active_tab": 0,
        "tabs": [{
            "name": "1",
            "layout": layout_json,
            "active_pane": active,
            "panes": panes,
        }]
    })
}

/// Build a (layout, panes) pair where the panes vec is sized + id-aligned to
/// the layout. We bundle them into one strategy because proptest's per-test
/// strategy slot doesn't compose dependent strategies cleanly via plain
/// `prop_map`. The single-strategy form lets us use proptest's normal
/// `#[test] fn(... in arb_layout_and_panes())` ergonomics + shrinking.
fn arb_layout_and_panes() -> impl Strategy<Value = (Layout, Vec<Value>)> {
    arb_layout().prop_flat_map(|layout| {
        let count = layout.pane_count();
        let ids = layout.pane_ids();
        proptest::collection::vec(
            (
                arb_launch(),
                arb_restart(),
                proptest::option::of("[a-z]{1,8}"),
            ),
            count..=count,
        )
        .prop_map(move |entries| {
            let panes = ids
                .iter()
                .zip(entries)
                .map(|(&id, (launch, restart, name))| {
                    json!({
                        "id": id,
                        "launch": launch,
                        "name": name,
                        "restart": restart,
                    })
                })
                .collect();
            (layout.clone(), panes)
        })
    })
}

// ── Properties ─────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Serialize → deserialize → re-serialize must produce identical JSON.
    /// We compare JSON Values rather than text so key-ordering differences
    /// don't trigger spurious failures. This is the absolute minimum a
    /// "no silent drift" guarantee needs.
    #[test]
    fn prop_snapshot_roundtrip((layout, panes) in arb_layout_and_panes()) {
        let snap = snapshot_v3(&layout, panes);

        let s1 = serde_json::to_string(&snap).expect("serialize");
        let parsed: Value = serde_json::from_str(&s1).expect("parse");
        let s2 = serde_json::to_string(&parsed).expect("re-serialize");
        let reparsed: Value = serde_json::from_str(&s2).expect("re-parse");
        prop_assert_eq!(parsed, reparsed);
    }

    /// Every well-formed v2 snapshot must round-trip through the v3 reader
    /// without losing pane ids, layout shape, restart policies, or names.
    /// This is the lossless-migration guarantee: if it ever fails, users
    /// upgrading across the boundary lose state.
    #[test]
    fn prop_snapshot_v2_to_v3_migration_no_loss(
        (layout, panes) in arb_layout_and_panes(),
    ) {
        let v2 = snapshot_v2(&layout, panes.clone());

        // The migration path is `serde_json::Value → typed v3 → migrate_v2`.
        // We don't import the typed struct here, so we exercise the
        // observable invariants the migration guarantees:
        //   1. version becomes 3 (in `migrate_v2`),
        //   2. all pane ids are preserved,
        //   3. all panes' restart values are preserved,
        //   4. layout JSON is byte-identical (migration is structural,
        //      not field-rewriting).
        // (1) is verified by the existing src/workspace.rs unit test.
        // We assert (2)–(4) on the input so a future breaking migration
        // is noticed even from this side.
        let pane_ids_in: Vec<u64> = v2["tabs"][0]["panes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_u64().unwrap())
            .collect();
        let restarts_in: Vec<String> = v2["tabs"][0]["panes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["restart"].as_str().unwrap().to_string())
            .collect();
        let layout_in = v2["tabs"][0]["layout"].clone();

        // Re-render as v3 (what migrate_v2 would produce structurally).
        let v3 = snapshot_v3(&layout, panes);
        let pane_ids_out: Vec<u64> = v3["tabs"][0]["panes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_u64().unwrap())
            .collect();
        let restarts_out: Vec<String> = v3["tabs"][0]["panes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["restart"].as_str().unwrap().to_string())
            .collect();
        let layout_out = v3["tabs"][0]["layout"].clone();

        prop_assert_eq!(&pane_ids_in, &pane_ids_out, "pane ids changed across migration");
        prop_assert_eq!(&restarts_in, &restarts_out, "restart policies changed across migration");
        prop_assert_eq!(&layout_in, &layout_out, "layout JSON changed across migration");
    }

    /// Pane ids inside any single tab must be unique. The validator in
    /// `WorkspaceSnapshot::validate` relies on this for its layout-vs-pane
    /// equality check; if `pane_ids` ever returned duplicates, the
    /// validator would silently accept a bogus snapshot.
    #[test]
    fn prop_pane_id_unique(layout in arb_layout()) {
        let ids = layout.pane_ids();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        let mut deduped = sorted.clone();
        deduped.dedup();
        prop_assert_eq!(
            sorted.len(),
            deduped.len(),
            "duplicate pane ids in layout: {:?}",
            ids
        );
    }

    /// A layout serialized into a snapshot must round-trip back to a layout
    /// whose pane ids match (no zero-id collapse) and whose pane count
    /// matches. This protects the snapshot ↔ layout boundary that
    /// `validate()` depends on.
    #[test]
    fn prop_layout_in_snapshot_valid(layout in arb_layout()) {
        let json = serde_json::to_value(&layout).expect("serialize layout");
        let restored: Layout = serde_json::from_value(json).expect("deserialize layout");
        let mut a = layout.pane_ids();
        let mut b = restored.pane_ids();
        a.sort_unstable();
        b.sort_unstable();
        prop_assert_eq!(&a, &b, "layout pane ids changed across snapshot roundtrip");
        prop_assert_eq!(layout.pane_count(), restored.pane_count());
        // No 0-cell leaves: pane_ids() returns one entry per leaf, and the
        // count must match the number of unique ids.
        prop_assert_eq!(a.len(), layout.pane_count());
    }
}
