#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

#[path = "../src/layout.rs"]
mod layout;
#[path = "../src/pane.rs"]
mod pane;
#[path = "../src/render.rs"]
mod render;
#[path = "../src/theme.rs"]
mod theme;

use layout::{Layout, Rect};
use pane::{Pane, PaneLaunch};
use render::BorderStyle;
use theme::{AdaptedTheme, TermCaps};

const TERM_W: u16 = 160;
const TERM_H: u16 = 48;
const SCROLLBACK: usize = 10_000;

fn make_inner(tw: u16, th: u16, show_status_bar: bool) -> Rect {
    let sh = if show_status_bar { 1u16 } else { 0 };
    Rect {
        x: 1,
        y: 1,
        w: tw.saturating_sub(2),
        h: th.saturating_sub(sh + 2),
    }
}

fn wait_for_initial_output(pane: &mut Pane) {
    for _ in 0..50 {
        if pane.read_output() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = pane.read_output();
}

struct PaneFixture {
    panes: HashMap<usize, Pane>,
}

impl Drop for PaneFixture {
    fn drop(&mut self) {
        for pane in self.panes.values_mut() {
            pane.kill();
        }
    }
}

fn build_panes(layout: &Layout, show_status_bar: bool) -> PaneFixture {
    let inner = make_inner(TERM_W, TERM_H, show_status_bar);
    let rects = layout.pane_rects(&inner);
    let mut panes = HashMap::new();

    for (&pid, rect) in &rects {
        let command = format!("printf 'pane {}\\nline 1\\nline 2\\n'; exec sleep 60", pid);
        let mut pane = Pane::with_scrollback(
            "/bin/sh",
            PaneLaunch::Command(command),
            rect.w.max(1),
            rect.h.max(1),
            SCROLLBACK,
        )
        .expect("spawn pane fixture");
        wait_for_initial_output(&mut pane);
        panes.insert(pid, pane);
    }

    PaneFixture { panes }
}

fn bench_border_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("border_cache");
    for (name, layout) in [
        ("2x2", Layout::from_grid(2, 2)),
        ("3x3", Layout::from_grid(3, 3)),
    ] {
        group.bench_with_input(BenchmarkId::from_parameter(name), &layout, |b, layout| {
            b.iter(|| {
                let cache = render::build_border_cache_with_style(
                    layout,
                    false,
                    TERM_W,
                    TERM_H,
                    BorderStyle::Rounded,
                );
                criterion::black_box(cache);
            });
        });
    }
    group.finish();
}

fn bench_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("render");
    for (name, layout) in [
        ("2x2", Layout::from_grid(2, 2)),
        ("3x3", Layout::from_grid(3, 3)),
    ] {
        let fixture = build_panes(&layout, false);
        let cache = render::build_border_cache_with_style(
            &layout,
            false,
            TERM_W,
            TERM_H,
            BorderStyle::Rounded,
        );
        let active = *cache
            .pane_order()
            .first()
            .expect("pane order should not be empty");

        let bench_theme: AdaptedTheme = theme::default_theme().adapt(TermCaps::TRUECOLOR);
        group.bench_function(BenchmarkId::new("full_redraw", name), |b| {
            let dirty = HashSet::new();
            let mut buf = Vec::with_capacity(64 * 1024);
            b.iter(|| {
                buf.clear();
                render::render_panes(
                    &mut buf,
                    &fixture.panes,
                    &layout,
                    active,
                    BorderStyle::Rounded,
                    false,
                    TERM_W,
                    TERM_H,
                    false,
                    &cache,
                    &dirty,
                    true,
                    None,
                    false,
                    &bench_theme,
                )
                .expect("full redraw render");
                criterion::black_box(&buf);
            });
        });

        group.bench_function(BenchmarkId::new("partial_redraw", name), |b| {
            let mut dirty = HashSet::new();
            dirty.insert(active);
            let mut buf = Vec::with_capacity(64 * 1024);
            b.iter(|| {
                buf.clear();
                render::render_panes(
                    &mut buf,
                    &fixture.panes,
                    &layout,
                    active,
                    BorderStyle::Rounded,
                    false,
                    TERM_W,
                    TERM_H,
                    false,
                    &cache,
                    &dirty,
                    false,
                    None,
                    false,
                    &bench_theme,
                )
                .expect("partial redraw render");
                criterion::black_box(&buf);
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
    targets = bench_border_cache, bench_render
}
criterion_main!(benches);
