//! Snapshot save / load benches (issue #99).
//!
//! Tracked metrics:
//! - `snapshot_save/4-panes-1k-lines` < 100 ms
//! - `snapshot_load/4-panes-1k-lines` < 100 ms
//!
//! `workspace.rs` drags in `pane`, `project`, `tab`, `render` and a
//! couple of unix-only modules — too much surface area for a bench
//! crate that is supposed to compile fast. Since the hot path inside
//! `save_snapshot` / `load_snapshot` is `serde_json` + an atomic
//! write/rename, we bench that boundary directly with a JSON payload
//! shaped like a real snapshot. This is the same approach taken in
//! `tests/property/workspace_migration.rs`.

use std::fs;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::{json, Value};

fn build_snapshot(panes: usize, lines_per_pane: usize) -> Value {
    let panes_json: Vec<Value> = (0..panes)
        .map(|i| {
            // Simulate scrollback by inflating the pane's `name` field with
            // `lines_per_pane` lines of text. Real snapshots don't store
            // scrollback today (#48 follow-up), but the JSON size is what
            // dominates the encode/decode cost so this is an honest proxy.
            let bulk: String = (0..lines_per_pane)
                .map(|n| format!("line {} of pane {}\n", n, i))
                .collect();
            json!({
                "id": i,
                "launch": {"Shell": null},
                "name": format!("pane-{}-payload-{}", i, bulk.len()),
                "scrollback_proxy": bulk,
            })
        })
        .collect();

    // Build a left-leaning tree so the layout is non-trivial.
    let mut node = json!({"Leaf": {"id": 0}});
    for i in 1..panes {
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
        "version": 2,
        "shell": "/bin/sh",
        "border_style": "rounded",
        "show_status_bar": true,
        "show_tab_bar": true,
        "scrollback": 10_000,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": {"root": node, "next_id": panes},
            "active_pane": 0,
            "panes": panes_json,
        }],
    })
}

fn bench_snapshot_save(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_save");
    for (label, panes, lines) in [
        ("4-panes-1k-lines", 4, 1_000),
        ("16-panes-100-lines", 16, 100),
    ] {
        let snap = build_snapshot(panes, lines);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("snap.json");

        group.bench_with_input(BenchmarkId::from_parameter(label), &snap, |b, snap| {
            b.iter(|| {
                let json = serde_json::to_string_pretty(snap).expect("serialize");
                // Mirror the atomic-write pattern in workspace::save_snapshot.
                let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
                fs::write(&tmp, &json).expect("write tmp");
                fs::rename(&tmp, &path).expect("rename");
                black_box(json.len());
            });
        });
    }
    group.finish();
}

fn bench_snapshot_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_load");
    for (label, panes, lines) in [
        ("4-panes-1k-lines", 4, 1_000),
        ("16-panes-100-lines", 16, 100),
    ] {
        let snap = build_snapshot(panes, lines);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("snap.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&snap).expect("serialize"),
        )
        .expect("seed snapshot file");

        group.bench_with_input(BenchmarkId::from_parameter(label), &path, |b, path| {
            b.iter(|| {
                let content = fs::read_to_string(path).expect("read");
                let parsed: Value = serde_json::from_str(&content).expect("parse");
                black_box(parsed);
            });
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(2));
    targets = bench_snapshot_save, bench_snapshot_load
}
criterion_main!(benches);
