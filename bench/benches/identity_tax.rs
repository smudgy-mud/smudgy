//! The **per-call identity re-derivation tax** on the interop write/read ops
//! (`core/src/session/runtime/script_engine/ops.rs`), measured in isolation —
//! no V8, no session. Every `op_smudgy_store_set`/`emit` re-parses identity
//! that was constant the moment the JS handle was constructed: the creator
//! descriptor (a full `serde_json` parse via [`Origin::try_from_creator_json`]),
//! the producer key ([`ProducerKey::from_origin`]'s lowercase allocations, or
//! `ProducerKey::parse` on the consumer side), the path
//! ([`StorePath::parse`], one `String` per segment), and the routing fold
//! (`fold_name`, which is exactly `str::to_ascii_lowercase` — mirrored here
//! so the private fn needs no bench-api export).
//!
//! These numbers are the **theoretical ceiling** of what interning those
//! identities at handle construction can save per call. The JS-surface bench
//! (`interop_ops`) measures the realized end-to-end delta; if realized ≪
//! predicted here, something else (op crossing, journaling) dominates and the
//! interning plan should adjust before code is written.
//!
//! Groups:
//! - `creator_parse/{user,package}`: the strict creator-descriptor parse every
//!   write-side op pays, plus `ProducerKey::from_origin` — the package cell
//!   carries the bigger JSON and two lowercase allocations, so interning
//!   should win visibly more there.
//! - `producer_parse/{user,package}`: the consumer-side spec parse
//!   (`get`/`watch`/`bind`/`post`).
//! - `path_parse/{depth1,depth4,bracket}`: the path-grammar parse, per store
//!   op, over representative shapes.
//! - `fold/{lower,mixed}`: the per-`emit`/`on`/`off` routing fold (already-
//!   lowercase names are the common case; a `Cow` fold would make them free).
//! - `per_set_composite/{user,package}`: everything one `set` re-derives
//!   (creator + path), the headline per-write number.
//! - `per_emit_composite/package`: what one `emit` re-derives (creator +
//!   stamp `format!` + fold), excluding subscriber fanout.
//!
//! Before timing, each case's **exact allocation count** is printed via the
//! counting global allocator (`smudgy_bench::alloc`) — same-thread, so the
//! figure is deterministic and survives machines whose timer jitter would
//! swallow a µs-scale before/after delta.
//!
//! Requires `smudgy_core`'s `bench-api` feature (exposes `ProducerKey` /
//! `StorePath`); `Origin` is public API. No corpus, so `SMUDGY_BENCH_LINES`
//! does not apply; there is no sanity gate to skip.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use smudgy_core::session::runtime::{Origin, ProducerKey, StorePath};

#[global_allocator]
static ALLOC: smudgy_bench::alloc::CountingAllocator = smudgy_bench::alloc::CountingAllocator;

/// The two creator descriptors the JS layer passes (stringified once per
/// module there; re-parsed per op call host-side — the tax being measured).
const USER_CREATOR: &str = r#"{"kind":"user"}"#;
const PACKAGE_CREATOR: &str =
    r#"{"kind":"package","owner":"benchmark","name":"synthetic-prompt","version":"1.4.2"}"#;

/// Consumer-side producer specs (`get`/`watch`/`bind`/`post` addressing).
const USER_SPEC: &str = "user";
const PACKAGE_SPEC: &str = "smudgy://benchmark/synthetic-prompt";

/// Representative store paths: the GMCP-ish shallow write, a deep one at the
/// grammar's realistic ceiling, and a bracket-quoted key.
const PATH_DEPTH1: &str = "vitals";
const PATH_DEPTH4: &str = "Char.Vitals.stats.hp";
const PATH_BRACKET: &str = r#"groupies["Mr. Foo"].hp"#;

/// Event-name fold inputs: the common already-lowercase case and a mixed-case
/// one. Mirrors `ops::fold_name` (`to_ascii_lowercase`) exactly.
const FOLD_LOWER: &str = "smudgy://benchmark/synthetic-prompt#prompt";
const FOLD_MIXED: &str = "smudgy://Benchmark/Synthetic-Prompt#Prompt";

/// What one write-side op re-derives from the creator descriptor: the strict
/// JSON parse plus the producer key (with its lowercase allocations).
fn creator_parse(creator: &str) -> ProducerKey {
    let origin = Origin::try_from_creator_json(creator).expect("bench creator is well-formed");
    ProducerKey::from_origin(&origin)
}

/// What one `emit` re-derives beyond `creator_parse`: the stamped canonical
/// name (`format!` + fold), as `op_smudgy_emit` builds it per call.
fn emit_stamp(producer: &ProducerKey, event: &str) -> (String, String) {
    let stamped = format!("{producer}#{event}");
    let canonical = stamped.to_ascii_lowercase();
    (stamped, canonical)
}

/// Print one case's exact per-call allocation figure (see the header: the
/// deterministic metric the timing numbers are cross-checked against).
fn report_allocs(label: &str, f: impl FnMut()) {
    let per_call = smudgy_bench::alloc::per_call(1_000, f);
    eprintln!(
        "  allocs/call {label}: {} ({} bytes)",
        per_call.count, per_call.bytes
    );
}

fn identity_tax(c: &mut Criterion) {
    eprintln!("identity_tax: per-call identity re-derivation on the interop ops (no V8)");
    report_allocs("creator_parse/user", || {
        black_box(creator_parse(black_box(USER_CREATOR)));
    });
    report_allocs("creator_parse/package", || {
        black_box(creator_parse(black_box(PACKAGE_CREATOR)));
    });
    report_allocs("producer_parse/package", || {
        black_box(ProducerKey::parse(black_box(PACKAGE_SPEC)));
    });
    report_allocs("path_parse/depth4", || {
        black_box(StorePath::parse(black_box(PATH_DEPTH4)).expect("valid path"));
    });
    report_allocs("per_set_composite/package", || {
        let producer = creator_parse(black_box(PACKAGE_CREATOR));
        let path = StorePath::parse(black_box(PATH_DEPTH4)).expect("valid path");
        black_box((producer, path));
    });
    report_allocs("per_emit_composite/package", || {
        let producer = creator_parse(black_box(PACKAGE_CREATOR));
        let stamp = emit_stamp(&producer, black_box("prompt"));
        black_box((producer, stamp));
    });

    let mut group = c.benchmark_group("creator_parse");
    for (id, creator) in [("user", USER_CREATOR), ("package", PACKAGE_CREATOR)] {
        group.bench_function(id, |b| {
            b.iter(|| black_box(creator_parse(black_box(creator))));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("producer_parse");
    for (id, spec) in [("user", USER_SPEC), ("package", PACKAGE_SPEC)] {
        group.bench_function(id, |b| {
            b.iter(|| black_box(ProducerKey::parse(black_box(spec))));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("path_parse");
    for (id, path) in [
        ("depth1", PATH_DEPTH1),
        ("depth4", PATH_DEPTH4),
        ("bracket", PATH_BRACKET),
    ] {
        group.bench_function(id, |b| {
            b.iter(|| black_box(StorePath::parse(black_box(path)).expect("valid path")));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("fold");
    for (id, name) in [("lower", FOLD_LOWER), ("mixed", FOLD_MIXED)] {
        group.bench_function(id, |b| {
            b.iter(|| black_box(black_box(name).to_ascii_lowercase()));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("per_set_composite");
    for (id, creator) in [("user", USER_CREATOR), ("package", PACKAGE_CREATOR)] {
        group.bench_function(BenchmarkId::from_parameter(id), |b| {
            b.iter(|| {
                let producer = creator_parse(black_box(creator));
                let path = StorePath::parse(black_box(PATH_DEPTH4)).expect("valid path");
                black_box((producer, path))
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("per_emit_composite");
    group.bench_function("package", |b| {
        b.iter(|| {
            let producer = creator_parse(black_box(PACKAGE_CREATOR));
            let stamp = emit_stamp(&producer, black_box("prompt"));
            black_box((producer, stamp))
        });
    });
    group.finish();
}

criterion_group!(benches, identity_tax);
criterion_main!(benches);
