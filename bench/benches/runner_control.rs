//! Small, revision-independent wall-clock controls for detecting a noisy
//! runner. PR reports do not treat these as product benchmarks: inconsistent
//! control deltas make the whole wall-clock comparison inconclusive.

use std::{hint::black_box, time::Duration};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

const WORDS: usize = 32 * 1024;

fn integer_mix(mut value: u64) -> u64 {
    for round in 0..16_384_u64 {
        value = value
            .wrapping_add(round ^ 0x9E37_79B9_7F4A_7C15)
            .rotate_left(17)
            .wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value ^= value >> 29;
    }
    value
}

fn memory_scan(words: &[u64]) -> u64 {
    words.iter().fold(0_u64, |state, value| {
        state.rotate_left(7).wrapping_add(*value)
    })
}

fn runner_control(c: &mut Criterion) {
    let mut group = c.benchmark_group("runner_control");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("integer_mix", |b| {
        b.iter(|| black_box(integer_mix(black_box(0x1234_5678_9ABC_DEF0))));
    });

    let word_count = u64::try_from(WORDS).expect("control word count fits u64");
    let words: Vec<u64> = (0..word_count)
        .map(|value| value.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(11))
        .collect();
    group.throughput(Throughput::Bytes(
        u64::try_from(words.len() * size_of::<u64>()).expect("control buffer length fits u64"),
    ));
    group.bench_function("memory_scan_256k", |b| {
        b.iter(|| black_box(memory_scan(black_box(&words))));
    });

    group.finish();
}

criterion_group!(benches, runner_control);
criterion_main!(benches);
