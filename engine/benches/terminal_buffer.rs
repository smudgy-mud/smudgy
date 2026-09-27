use std::{fmt::Write as _, hint::black_box};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use smudgy_engine::TerminalBuffer;

const BATCH_LINES: usize = 1_000;

fn wire_batch(first: usize) -> Vec<u8> {
    let mut wire = String::with_capacity(BATCH_LINES * 80);
    for row in first..first + BATCH_LINES {
        writeln!(
            wire,
            "\x1b[38;5;{}m{row:06}\x1b[0m Smudgy portable terminal benchmark row",
            33 + row % 180
        )
        .unwrap();
    }
    wire.into_bytes()
}

fn terminal_buffer(c: &mut Criterion) {
    let wire = wire_batch(0);
    let mut group = c.benchmark_group("portable_terminal");
    group.throughput(Throughput::Elements(BATCH_LINES as u64));
    group.bench_function("fresh_100k_capacity", |b| {
        b.iter_batched_ref(
            TerminalBuffer::default,
            |buffer| {
                black_box(buffer.feed(black_box(&wire)));
                black_box(buffer.committed_len());
            },
            BatchSize::PerIteration,
        );
    });

    let mut steady = TerminalBuffer::default();
    for batch in 0..100 {
        black_box(steady.feed(&wire_batch(batch * BATCH_LINES)));
    }
    assert_eq!(steady.committed_len(), steady.max_lines());
    group.bench_function("at_100k_capacity", |b| {
        b.iter(|| {
            black_box(steady.feed(black_box(&wire)));
            black_box(steady.committed_len());
        });
    });
    group.finish();
}

criterion_group!(benches, terminal_buffer);
criterion_main!(benches);
