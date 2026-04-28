//! IPC frame encode/decode benches (issue #99).
//!
//! Tracked metrics:
//! - `frame_encode/4kb` < 50 µs
//! - `frame_decode/4kb` < 50 µs
//!
//! Mounting `protocol.rs` directly via `#[path]` follows the same pattern
//! as `render_hotpaths.rs` — the bench crate avoids pulling in the rest
//! of the binary and stays under the bench-suite cap of 5 minutes per
//! issue #99 acceptance.

// The bench only uses a handful of items from `protocol.rs`; the rest
// (handshake structs, command tags, etc.) are exercised by the lib/bin
// targets but legitimately look dead from the bench's narrow view.
#![allow(dead_code, unused_imports)]

use std::io::Cursor;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[path = "../src/protocol.rs"]
mod protocol;

use protocol::{decode_resize, encode_resize, read_msg, write_msg, C_EVENT};

fn payload(size: usize) -> Vec<u8> {
    // Pseudo-random but deterministic — incompressible enough for the
    // codec to do real work, fixed seed so bench numbers are stable run
    // to run.
    let mut buf = Vec::with_capacity(size);
    let mut state = 0x9e3779b1u32;
    for _ in 0..size {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        buf.push((state >> 16) as u8);
    }
    buf
}

fn bench_frame_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_encode");
    for size in [256usize, 4 * 1024, 64 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        let p = payload(size);
        let mut buf = Vec::with_capacity(size + 5);
        group.bench_with_input(
            BenchmarkId::from_parameter(format_size(size)),
            &p,
            |b, p| {
                b.iter(|| {
                    buf.clear();
                    write_msg(&mut buf, C_EVENT, p).expect("encode");
                    black_box(&buf);
                });
            },
        );
    }
    group.finish();
}

fn bench_frame_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_decode");
    for size in [256usize, 4 * 1024, 64 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        let p = payload(size);
        let mut framed = Vec::with_capacity(size + 5);
        write_msg(&mut framed, C_EVENT, &p).expect("encode");

        group.bench_with_input(
            BenchmarkId::from_parameter(format_size(size)),
            &framed,
            |b, framed| {
                b.iter(|| {
                    let mut cursor = Cursor::new(framed.as_slice());
                    let result = read_msg(&mut cursor).expect("decode");
                    black_box(result);
                });
            },
        );
    }
    group.finish();
}

fn bench_resize_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("resize_codec");
    group.bench_function("encode", |b| {
        b.iter(|| {
            let bytes = encode_resize(black_box(80), black_box(24));
            black_box(bytes);
        });
    });
    let bytes = encode_resize(80, 24);
    group.bench_function("decode", |b| {
        b.iter(|| {
            let r = decode_resize(black_box(&bytes));
            black_box(r);
        });
    });
    group.finish();
}

fn format_size(bytes: usize) -> String {
    if bytes >= 1024 {
        format!("{}kb", bytes / 1024)
    } else {
        format!("{}b", bytes)
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(50)
        .measurement_time(Duration::from_secs(2));
    targets = bench_frame_encode, bench_frame_decode, bench_resize_codec
}
criterion_main!(benches);
