//! Runtime-catalogue costs (`docs/interop.md` §10;
//! `docs/interop.md` §10): the per-sample recording cost at the
//! emit/post choke points and the snapshot build the store tab consumes —
//! pure host-side Rust, no engine, so the numbers are deterministic and
//! isolate exactly the catalogue's own work.
//!
//! Cells:
//! - `sample/{unsubscribed,subscribed}/{small,large}`: one interned-key
//!   sample per iteration (the `emit` hot-path shape — key strings are
//!   pre-shared `Arc`s). Unsubscribed is what every session pays per
//!   emit/post with no store tab open: ring insert + display copy, **no JSON
//!   parse**. Subscribed adds the deferred parse + all-history shape merge
//!   that a snapshot consumer buys. `dynamic/small` is the `procedurePost`
//!   shape (name folded + shared per call).
//! - `snapshot/leaves_{64,4096,65536}`: the full snapshot build against a
//!   committed store of that many scalar leaves (grouped 32 per object) plus
//!   a fixed catalogue population. Producer trees are shared `Node` roots
//!   (`Arc` bumps), so the build should price the *catalogue entries*, not
//!   the store size — flat-ish across this axis is the acceptance shape.
//! - `snapshot/entries_{8,128,512}`: the same build against catalogue entry
//!   count (full sample rings each, 512 = the per-producer entry budget) at a
//!   fixed small store — the snapshot's remaining O(entries) half (per-entry
//!   shape render + sample-ring collection), so a regression in per-entry
//!   cost scales visibly instead of hiding behind the flat store axis.
//!
//! Requires `smudgy_core`'s `bench-api` feature (exposes `SessionStore` and
//! its signature types). No log corpus, so `SMUDGY_BENCH_LINES` does not
//! apply. Env var: `SMUDGY_BENCH_SKIP_SANITY=1` skips the setup assertions.

use std::{env, hint::black_box, sync::Arc};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use smudgy_core::session::runtime::{
    IsolateId, ProducerKey, SessionStore, StorePath,
    catalogue::{CatalogueKind, RuntimeCatalogue},
};

/// Store sizes (scalar leaves) for the snapshot cells.
const LEAVES_GRID: &[usize] = &[64, 4096, 65536];

/// Catalogue entries (with full sample rings) behind every store-axis snapshot cell.
const SNAPSHOT_ENTRIES: usize = 8;

/// Catalogue entry populations for the entry-count snapshot axis; the top is the
/// per-producer entry budget (`MAX_ENTRIES_PER_PRODUCER`).
const ENTRIES_GRID: &[usize] = &[8, 128, 512];

/// A committed store with `leaves` scalar leaves under 32-wide group objects
/// (`group_007.stat_00231: 231`) — the GMCP-ish shape at increasing scale.
fn store_with_leaves(leaves: usize) -> SessionStore {
    let mut store = SessionStore::new();
    for index in 0..leaves {
        let path = StorePath::parse(&format!("group_{:03}.stat_{index:05}", index / 32))
            .expect("generated path is valid");
        let value = u64::try_from(index).expect("grid sizes fit in u64");
        store
            .set(ProducerKey::User, path, value.into(), IsolateId::Main, 0)
            .expect("scalar writes stay far under the default budgets");
    }
    let _ = store.flush();
    store
}

/// A catalogue with `entries` event entries, each ring filled with parsed-shape
/// samples, as the entry population behind the snapshot cells.
fn populated_catalogue(subscribed: bool, entries: usize) -> RuntimeCatalogue {
    let mut catalogue = RuntimeCatalogue::new();
    catalogue.set_subscribed(subscribed);
    let producer: Arc<str> = Arc::from("user");
    for entry in 0..entries {
        for sample in 0..24 {
            catalogue.sample_dynamic(
                &producer,
                CatalogueKind::Event,
                &format!("event_{entry}"),
                "user",
                &format!(r#"{{"hp":{sample},"tag":"x"}}"#),
            );
        }
    }
    catalogue
}

fn payload_small() -> String {
    r#"{"hp":100,"mp":50,"tag":"idle"}"#.to_string()
}

fn payload_large() -> String {
    let fields: Vec<String> = (0..64).map(|i| format!(r#""stat_{i:03}":{i}"#)).collect();
    format!("{{{}}}", fields.join(","))
}

fn sanity_check_sample(producer: &Arc<str>, name: &Arc<str>) {
    // The measured call records exactly one occurrence per invocation and
    // defers parsing while unsubscribed (shape only appears at snapshot).
    let mut probe = RuntimeCatalogue::new();
    probe.sample_interned(
        producer,
        CatalogueKind::Event,
        name,
        name,
        producer,
        &payload_small(),
    );
    let snap = probe.snapshot(&SessionStore::new());
    assert_eq!(snap.entries.len(), 1);
    assert_eq!(snap.entries[0].occurrences, 1);
    assert_eq!(
        snap.entries[0].inferred_shape.as_deref(),
        Some("{ hp: number; mp: number; tag: string }"),
        "the snapshot catch-up parses the unsubscribed backlog"
    );
}

fn catalogue_bench(c: &mut Criterion) {
    let sanity = env::var("SMUDGY_BENCH_SKIP_SANITY").is_err();

    let mut group = c.benchmark_group("catalogue");

    // --- sample cells -----------------------------------------------------
    let producer: Arc<str> = Arc::from("smudgy://bench/catalogue");
    let name: Arc<str> = Arc::from("tick");
    let cases: &[(&str, String)] = &[("small", payload_small()), ("large", payload_large())];
    if sanity {
        sanity_check_sample(&producer, &name);
    }
    for (case, payload) in cases {
        for subscribed in [false, true] {
            let mode = if subscribed {
                "subscribed"
            } else {
                "unsubscribed"
            };
            let mut catalogue = RuntimeCatalogue::new();
            catalogue.set_subscribed(subscribed);
            group.throughput(Throughput::Elements(1));
            group.bench_function(BenchmarkId::new(format!("sample/{mode}"), case), |b| {
                b.iter(|| {
                    catalogue.sample_interned(
                        &producer,
                        CatalogueKind::Event,
                        &name,
                        &name,
                        &producer,
                        black_box(payload),
                    );
                });
            });
        }
    }
    // The dynamic-name path (`procedurePost` shape): fold + share per call.
    let mut catalogue = RuntimeCatalogue::new();
    group.throughput(Throughput::Elements(1));
    group.bench_function(BenchmarkId::new("sample/dynamic", "small"), |b| {
        let payload = payload_small();
        b.iter(|| {
            catalogue.sample_dynamic(
                &producer,
                CatalogueKind::Procedure,
                black_box("refresh"),
                "user",
                black_box(&payload),
            );
        });
    });

    // --- snapshot cells: store-size axis ----------------------------------
    for &leaves in LEAVES_GRID {
        let store = store_with_leaves(leaves);
        let mut catalogue = populated_catalogue(true, SNAPSHOT_ENTRIES);
        if sanity {
            let snap = catalogue.snapshot(&store);
            assert_eq!(snap.producers.len(), 1);
            assert_eq!(snap.producers[0].entries, {
                // leaves + one group object per 32 + the root object.
                (leaves + leaves.div_ceil(32) + 1) as u64
            });
            assert_eq!(snap.entries.len(), SNAPSHOT_ENTRIES);
        }
        group.throughput(Throughput::Elements(1));
        group.bench_function(
            BenchmarkId::new("snapshot", format!("leaves_{leaves}")),
            |b| {
                b.iter(|| black_box(catalogue.snapshot(&store)));
            },
        );
    }

    // --- snapshot cells: entry-count axis ----------------------------------
    for &entries in ENTRIES_GRID {
        let store = store_with_leaves(64);
        let mut catalogue = populated_catalogue(true, entries);
        if sanity {
            let snap = catalogue.snapshot(&store);
            assert_eq!(
                snap.entries.len(),
                entries,
                "all entries admitted at the budget"
            );
        }
        group.throughput(Throughput::Elements(1));
        group.bench_function(
            BenchmarkId::new("snapshot", format!("entries_{entries}")),
            |b| {
                b.iter(|| black_box(catalogue.snapshot(&store)));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, catalogue_bench);
criterion_main!(benches);
