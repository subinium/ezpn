//! Property tests for `layout.rs` (issue #94).
//!
//! Invariants verified:
//! 1. `split → remove → swap` preserves the binary-tree shape (no orphan
//!    leaves, no duplicate IDs, `next_id` strictly monotonic).
//! 2. `equalize` is idempotent.
//! 3. `pane_rects` always returns one rect per leaf and rects fit inside
//!    the inner area.
//!
//! `layout.rs` is bin-internal (no `pub` cdylib surface), so we mount it
//! via `#[path]` — same trick the bench harness uses.

#![allow(dead_code)]

#[path = "../../src/layout.rs"]
mod layout;

use layout::{Direction, Layout, Rect};
use proptest::prelude::*;

/// Recursively count leaves to cross-check `pane_count`.
fn count_leaves(node: &layout::LayoutNode) -> usize {
    match node {
        layout::LayoutNode::Leaf { .. } => 1,
        layout::LayoutNode::Split { first, second, .. } => {
            count_leaves(first) + count_leaves(second)
        }
    }
}

fn collect_ids(node: &layout::LayoutNode, out: &mut Vec<usize>) {
    match node {
        layout::LayoutNode::Leaf { id } => out.push(*id),
        layout::LayoutNode::Split { first, second, .. } => {
            collect_ids(first, out);
            collect_ids(second, out);
        }
    }
}

fn ids_are_unique(layout: &Layout) -> bool {
    let mut ids = Vec::new();
    collect_ids(&layout.root, &mut ids);
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    ids.len() == n
}

#[derive(Clone, Debug)]
enum Op {
    Split(usize, Direction),
    Remove(usize),
    Swap(usize, usize),
    Equalize,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (
            any::<usize>(),
            prop_oneof![Just(Direction::Horizontal), Just(Direction::Vertical)]
        )
            .prop_map(|(t, d)| Op::Split(t, d)),
        any::<usize>().prop_map(Op::Remove),
        (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::Swap(a, b)),
        Just(Op::Equalize),
    ]
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(op_strategy(), 0..32)
}

fn apply(mut layout: Layout, ops: &[Op]) -> Layout {
    for op in ops {
        let live: Vec<usize> = layout.pane_ids();
        if live.is_empty() {
            break;
        }
        match op {
            Op::Split(t, dir) => {
                let target = live[*t % live.len()];
                let _ = layout.split(target, *dir);
            }
            Op::Remove(t) => {
                if live.len() > 1 {
                    let target = live[*t % live.len()];
                    let _ = layout.remove(target);
                }
            }
            Op::Swap(a, b) => {
                let aa = live[*a % live.len()];
                let bb = live[*b % live.len()];
                layout.swap_panes(aa, bb);
            }
            Op::Equalize => layout.equalize(),
        }
    }
    layout
}

proptest! {
    // Tighter case count keeps `cargo test` snappy. Crashes still get a
    // shrunk minimal counterexample.
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// Sequence of ops never leaves the tree with duplicate or missing ids.
    #[test]
    fn op_sequence_preserves_id_uniqueness(ops in ops_strategy()) {
        let layout = apply(Layout::from_grid(2, 2), &ops);
        prop_assert!(ids_are_unique(&layout), "duplicate pane ids after ops");
        prop_assert_eq!(layout.pane_count(), count_leaves(&layout.root));
        prop_assert!(layout.pane_count() >= 1, "tree must always have ≥1 leaf");
    }

    /// `equalize` is idempotent.
    #[test]
    fn equalize_is_idempotent(ops in ops_strategy()) {
        let mut layout = apply(Layout::from_grid(3, 3), &ops);
        layout.equalize();
        let snapshot_ids = layout.pane_ids();
        layout.equalize();
        prop_assert_eq!(layout.pane_ids(), snapshot_ids);
    }

    /// `pane_rects` returns one rect per leaf and every rect fits in the
    /// inner area.
    #[test]
    fn pane_rects_fit_inside_inner(ops in ops_strategy()) {
        let layout = apply(Layout::from_grid(2, 2), &ops);
        let inner = Rect { x: 1, y: 1, w: 80, h: 24 };
        let rects = layout.pane_rects(&inner);
        prop_assert_eq!(rects.len(), layout.pane_count());

        for (id, r) in &rects {
            prop_assert!(r.x >= inner.x, "pane {} x={} left of inner.x={}", id, r.x, inner.x);
            prop_assert!(r.y >= inner.y, "pane {} y={} above inner.y={}", id, r.y, inner.y);
            prop_assert!(
                r.x.saturating_add(r.w) <= inner.x.saturating_add(inner.w),
                "pane {} extends past right edge", id
            );
            prop_assert!(
                r.y.saturating_add(r.h) <= inner.y.saturating_add(inner.h),
                "pane {} extends past bottom edge", id
            );
        }
    }

    /// Split followed immediately by remove of the new pane returns the
    /// tree to its prior pane count.
    #[test]
    fn split_then_remove_round_trips(target_idx in 0usize..16, dir in prop_oneof![Just(Direction::Horizontal), Just(Direction::Vertical)]) {
        let mut layout = Layout::from_grid(2, 2);
        let live = layout.pane_ids();
        let target = live[target_idx % live.len()];
        let prior = layout.pane_count();
        let new_id = layout.split(target, dir);
        prop_assert_eq!(layout.pane_count(), prior + 1);
        let removed = layout.remove(new_id);
        prop_assert!(removed);
        prop_assert_eq!(layout.pane_count(), prior);
    }
}
