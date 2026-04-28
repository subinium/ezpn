//! RSS proxy benches (issue #99).
//!
//! Tracked metrics are the actual RSS thresholds from #99 (12 MB empty,
//! 60 MB at 100 panes), but criterion measures *time*, not memory. This
//! file provides a *proxy* for steady-state allocation cost: how long it
//! takes to construct and tear down the per-pane allocation graph that
//! the daemon holds in memory. A regression here usually correlates with
//! a regression in steady-state RSS.
//!
//! Real RSS gating lives in the soak test (`tests/soak/run.sh`); these
//! benches are the fast-feedback complement that runs on every PR.

use std::collections::HashMap;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

/// Stand-in for the per-pane state the daemon keeps. Field names mirror
/// what `pane.rs` allocates today; sizes are conservative upper bounds.
#[derive(Clone)]
struct PaneFootprint {
    id: usize,
    name: String,
    cwd: Option<String>,
    env: HashMap<String, String>,
    scrollback: Vec<u8>,
}

fn build_pane(id: usize, scrollback_kb: usize) -> PaneFootprint {
    let mut env = HashMap::with_capacity(16);
    for k in 0..16 {
        env.insert(format!("KEY_{}", k), format!("value-{}-{}", id, k));
    }
    PaneFootprint {
        id,
        name: format!("pane-{}", id),
        cwd: Some(format!("/home/user/project/pane-{}", id)),
        env,
        scrollback: vec![0u8; scrollback_kb * 1024],
    }
}

fn bench_empty_session(c: &mut Criterion) {
    // "Empty session" — daemon + 1 client. The 12 MB target in #99
    // accounts for binary + libc + ~1 pane state. We bench the pane
    // construction cost only.
    c.bench_function("rss_proxy/empty_session", |b| {
        b.iter(|| {
            let p = build_pane(0, 4); // 4 KB scrollback for an "empty" pane
            black_box(p);
        });
    });
}

fn bench_n_panes(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss_proxy/n_panes");
    for n in [10usize, 50, 100] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let panes: Vec<_> = (0..n).map(|i| build_pane(i, 64)).collect();
                black_box(panes);
            });
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(30)
        .measurement_time(Duration::from_secs(2));
    targets = bench_empty_session, bench_n_panes
}
criterion_main!(benches);
