//! Small allocation comparison, also runnable against a baseline vt100 rlib.
//! This measures cell payload reads, not daemon RSS or total parser memory.

#[path = "../src/vt100/mod.rs"]
mod vt100;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

fn count_allocation() {
    // Const-initialized, non-dropping TLS does not allocate or recurse here.
    let _ = TRACK.try_with(|track| {
        if track.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

// SAFETY: Every operation delegates the identical pointer/layout to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation();
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn cell_contents_read_allocation_count() {
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process(b"ASCII and CJK: \xe9\x9f\x93");
    ALLOCATIONS.with(|count| count.set(0));
    TRACK.with(|track| track.set(true));
    for row in 0..24 {
        for col in 0..80 {
            std::hint::black_box(parser.screen().cell(row, col).unwrap().contents());
        }
    }
    TRACK.with(|track| track.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);
    let bytes = std::mem::size_of::<vt100::Cell>();
    eprintln!(
        "cell_bytes={bytes}, cells=1920, visible_cell_bytes={}, contents_allocations={allocations}",
        bytes * 1920
    );
    // Explicit baseline build only; ordinary Cargo tests require zero allocations.
    let baseline = option_env!("EZPN_VT100_ALLOCATION_BASELINE") == Some("0.15.2");
    assert_eq!(allocations, if baseline { 1920 } else { 0 });
}
