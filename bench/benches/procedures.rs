//! Paired controls always emit the same five cases, including without `.call()`.
//! On that baseline the call transitions are idle; on the candidate they expose
//! idle cost after first use, a parked call, and the return to idle in one process.

#[path = "support/procedures.rs"]
mod support;

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use smudgy_bench::session::styled;

fn procedures(c: &mut Criterion) {
    let (rt, mut session, calls) = support::start();
    let lines: Vec<_> = (0..500).map(|_| styled("ZZLINE")).collect();
    let mut group = c.benchmark_group("procedures");
    group.sample_size(15);
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Elements(128));
    group.bench_function("post_cross_isolate", |b| {
        b.iter_custom(|n| support::passes(&rt, &mut session, &[styled("ZZPOST")], "POST_DONE", n));
    });
    group.throughput(Throughput::Elements(500));
    group.bench_function("triggers", |b| {
        b.iter_custom(|n| support::passes(&rt, &mut session, &lines, "LINES_DONE", n));
    });
    if calls {
        rt.block_on(async {
            session.feed(&styled("ZZCALL 1 0"));
            session.drain_until("CALLS_DONE").await;
        });
    }
    group.bench_function("triggers_after_call", |b| {
        b.iter_custom(|n| support::passes(&rt, &mut session, &lines, "LINES_DONE", n));
    });
    if calls {
        rt.block_on(async {
            session.feed(&styled("ZZWAIT"));
            session.drain_until("WAIT_STARTED").await;
        });
    }
    group.bench_function("triggers_while_call_waits", |b| {
        b.iter_custom(|n| support::passes(&rt, &mut session, &lines, "LINES_DONE", n));
    });
    if calls {
        rt.block_on(async {
            session.feed(&styled("ZZFINISH"));
            session.drain_until("WAIT_DONE").await;
        });
    }
    group.bench_function("triggers_after_wait", |b| {
        b.iter_custom(|n| support::passes(&rt, &mut session, &lines, "LINES_DONE", n));
    });
    group.finish();
}

criterion_group!(benches, procedures);
criterion_main!(benches);
