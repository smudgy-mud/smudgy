//! Candidate-only RPC timings: no baseline delta exists before `.call()` lands.

#[path = "support/procedures.rs"]
mod support;

use std::sync::Arc;

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use smudgy_bench::session::styled;

fn procedure_calls(c: &mut Criterion) {
    let (rt, mut session, calls) = support::start();
    // The trusted runner requires an explicit availability report before it
    // accepts an empty suite, so a broken harness cannot silently skip timing.
    if let Some(path) = std::env::var_os("SMUDGY_BENCH_AVAILABILITY_REPORT") {
        std::fs::write(path, format!("{{\"available\":{calls}}}\n"))
            .expect("write benchmark availability report");
    }
    if !calls {
        eprintln!("Procedure .call() is unavailable; skipping RPC timings.");
        return;
    }
    let mut group = c.benchmark_group("procedure_calls");
    group.sample_size(15);
    group.sampling_mode(SamplingMode::Flat);
    for count in [1, 64, 256] {
        for size in [0, 1024, 8192] {
            let command = styled(&format!("ZZCALL {count} {size}"));
            group.throughput(Throughput::Elements(count));
            group.bench_function(format!("cross_isolate/C{count}/P{size}"), |b| {
                b.iter_custom(|n| {
                    support::passes(&rt, &mut session, &[Arc::clone(&command)], "CALLS_DONE", n)
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, procedure_calls);
criterion_main!(benches);
