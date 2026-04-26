//! Property tests for `crate::layout`.
//!
//! These exist because the unit tests in `src/layout.rs` cover hand-picked
//! shapes (1x1, 2x3, the named presets) and known-bad ratios. Layout bugs
//! that survive that suite tend to be input-shape regressions: weird
//! split sequences, extreme aspect ratios, deep recursion. proptest
//! generates those for us and shrinks failures to a minimal repro.
//!
//! ## Ground rules
//! - Run with at least 100 cases per property (proptest default; CI sets
//!   `PROPTEST_CASES=100` explicitly so a developer's local override
//!   doesn't silently lower coverage).
//! - We deliberately bound layout depth (≤ 6 splits) and pane count
//!   (≤ 32). Going wider produces shapes that no real terminal supports
//!   and bogs shrinking down without finding new failure modes.
//! - We import `layout.rs` via `#[path]` rather than a `lib.rs` re-export
//!   so this test crate stays decoupled from the binary's huge module
//!   graph (`pane`/`render`/`server` etc.). Same approach the existing
//!   `benches/render_hotpaths.rs` uses.

#![allow(dead_code, clippy::needless_range_loop)]

#[path = "../src/layout.rs"]
mod layout;

use layout::{Direction, Layout, NavDir, Rect, MIN_PANE_H, MIN_PANE_W};
use proptest::prelude::*;

// ── Strategies ─────────────────────────────────────────────

/// Generate a "sane" terminal area: 80–240 cols, 24–80 rows. Smaller areas
/// hit the "can't fit two MIN children" branch which is exercised by
/// dedicated unit tests; we want property tests to focus on the common
/// path where splits actually have headroom.
fn arb_inner() -> impl Strategy<Value = Rect> {
    (80u16..=240u16, 24u16..=80u16).prop_map(|(w, h)| Rect { x: 1, y: 1, w, h })
}

fn arb_direction() -> impl Strategy<Value = Direction> {
    prop_oneof![Just(Direction::Horizontal), Just(Direction::Vertical),]
}

/// Build a layout by replaying a script of (target_index_into_pane_ids, dir)
/// splits on top of a 1x1. Indexing into the live id list keeps splits
/// targeted at panes that exist after each previous step. Cap at 6 splits
/// so generated trees stay debuggable when shrinking.
fn arb_layout() -> impl Strategy<Value = Layout> {
    let script = prop::collection::vec((any::<u8>(), arb_direction()), 0usize..6);
    script.prop_map(|ops| {
        let mut layout = Layout::from_grid(1, 1);
        for (idx_seed, dir) in ops {
            let ids = layout.pane_ids();
            if ids.is_empty() {
                break;
            }
            let target = ids[idx_seed as usize % ids.len()];
            layout.split(target, dir);
        }
        layout
    })
}

// ── Helpers ────────────────────────────────────────────────

fn rects_overlap(a: &Rect, b: &Rect) -> bool {
    let ax2 = a.x + a.w;
    let ay2 = a.y + a.h;
    let bx2 = b.x + b.w;
    let by2 = b.y + b.h;
    a.x < bx2 && b.x < ax2 && a.y < by2 && b.y < ay2
}

fn within(r: &Rect, inner: &Rect) -> bool {
    r.x >= inner.x
        && r.y >= inner.y
        && r.x + r.w <= inner.x + inner.w
        && r.y + r.h <= inner.y + inner.h
}

// ── Properties ─────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// No two pane content rects overlap. `pane_rects` is what the renderer
    /// loops over per frame, so an overlap means we'd draw two PTYs into
    /// the same cells — visible corruption.
    #[test]
    fn prop_layout_render_no_overlap(layout in arb_layout(), inner in arb_inner()) {
        let rects = layout.pane_rects(&inner);
        let entries: Vec<_> = rects.iter().collect();
        for i in 0..entries.len() {
            for j in (i + 1)..entries.len() {
                let (id_a, ra) = entries[i];
                let (id_b, rb) = entries[j];
                // Zero-width / zero-height rects can occur on extreme ratios
                // (split_area returns w=0 / h=0 explicitly so callers can
                // skip them). Treat those as "not present" — they cannot
                // visually overlap anything.
                if ra.w == 0 || ra.h == 0 || rb.w == 0 || rb.h == 0 {
                    continue;
                }
                prop_assert!(
                    !rects_overlap(ra, rb),
                    "panes {} and {} overlap: {:?} vs {:?}",
                    id_a, id_b, ra, rb
                );
            }
        }
    }

    /// Every pane rect lives strictly inside the terminal `inner` area.
    /// A pane that extends outside `inner` writes over the border or
    /// off-screen — both render-bug symptoms.
    #[test]
    fn prop_layout_all_panes_within_bounds(layout in arb_layout(), inner in arb_inner()) {
        let rects = layout.pane_rects(&inner);
        for (id, r) in rects.iter() {
            // Skip the explicit zero-dimension "no room" sentinel rects.
            if r.w == 0 || r.h == 0 {
                continue;
            }
            prop_assert!(
                within(r, &inner),
                "pane {} rect {:?} escapes inner {:?}",
                id, r, inner
            );
        }
    }

    /// After any sequence of splits on a roomy terminal, every leaf pane
    /// satisfies the MIN_PANE size guarantees. This is the invariant
    /// `MIN_PANE_W` / `MIN_PANE_H` were introduced for (#17). We constrain
    /// the input area to one that can in principle hold all the splits
    /// (≥ 32 cells per dimension covers the 6-split worst case).
    #[test]
    fn prop_layout_split_min_size(
        layout in arb_layout(),
        // Force a roomy area so the MIN check is meaningful — narrow areas
        // intentionally fall back to the "best we can do" branch.
        w in 80u16..=240u16,
        h in 32u16..=80u16,
    ) {
        let inner = Rect { x: 1, y: 1, w, h };
        let rects = layout.pane_rects(&inner);
        for (id, r) in rects.iter() {
            if r.w == 0 || r.h == 0 {
                // Degenerate split_area branch — see prop_layout_render_no_overlap rationale.
                continue;
            }
            prop_assert!(
                r.w >= MIN_PANE_W,
                "pane {} width {} below MIN_PANE_W={} (rect={:?})",
                id, r.w, MIN_PANE_W, r
            );
            prop_assert!(
                r.h >= MIN_PANE_H,
                "pane {} height {} below MIN_PANE_H={} (rect={:?})",
                id, r.h, MIN_PANE_H, r
            );
        }
    }

    /// Every pane is reachable from every other pane via some sequence of
    /// `navigate` calls. Equivalently: the directional adjacency graph is
    /// connected. If a pane gets "stranded" (no neighbour finds it via
    /// any of L/R/U/D), the user can't focus it without the mouse — a
    /// regression we've hit twice during nested-split refactors.
    #[test]
    fn prop_layout_navigate_reachable(layout in arb_layout(), inner in arb_inner()) {
        let ids = layout.pane_ids();
        if ids.len() < 2 {
            return Ok(());
        }
        let rects = layout.pane_rects(&inner);
        // Skip degenerate inputs: an inner rect too small to render this many
        // panes will produce zero-area rects, and adjacency on zero-area is
        // meaningless. Properties must be vacuously true on degenerate input,
        // not falsely fail.
        if rects.values().any(|r| r.w == 0 || r.h == 0) {
            return Ok(());
        }

        // BFS from ids[0] using all four directions as edges.
        let start = ids[0];
        let mut visited = std::collections::HashSet::new();
        visited.insert(start);
        let mut frontier = vec![start];
        while let Some(cur) = frontier.pop() {
            for dir in [NavDir::Left, NavDir::Right, NavDir::Up, NavDir::Down] {
                if let Some(next) = layout.navigate(cur, dir, &inner) {
                    if visited.insert(next) {
                        frontier.push(next);
                    }
                }
            }
        }
        prop_assert_eq!(
            visited.len(),
            ids.len(),
            "navigate-graph not connected: visited {:?} of {:?}",
            visited, ids
        );
    }
}
