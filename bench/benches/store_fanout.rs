//! Drives the session store's write→flush→fanout machinery
//! (`core/src/session/runtime/store.rs`) with **no script engine behind it**:
//! [`SessionStore::set`] journals turn writes (normalize + budget probe +
//! turn-projection upkeep + journal push — the same path `op_smudgy_store_set`
//! takes), and [`SessionStore::flush`] commits the journal and fans deliveries
//! out to watchers — exactly what the runtime's `flush_session_store`
//! (`core/src/session/runtime.rs`) wraps at every turn boundary. Delivery
//! never enters V8: each one just queues a
//! `RuntimeAction::CallJavascriptFunction`, so the numbers isolate the
//! store-side cost.
//!
//! Workload shape: one producer (the `user` subtree), J scalar writes per
//! turn at distinct two-segment paths under one shared group segment (the
//! GMCP-ish shape — `vitals.stat_007: 7`), W watchers all on that group
//! segment, so every watcher is path-comparable to every write. Groups, per
//! `J writes/turn × W watchers` grid point (J ∈ {1,16,128}, W ∈ {0,8,64}):
//!
//! - `flush_per_write/J*/W*`: the journal **replay** loop (`onWrite`
//!   cadence) — one delivery per (write × comparable-path watcher), i.e.
//!   O(J × W) `path`/`value` stringifications + capture allocations.
//! - `flush_coalesced/J*/W*`: one delivery per watcher (`watch` cadence),
//!   each carrying the watched subtree's committed snapshot serialized per
//!   delivery.
//! - `flush_mixed/J*/W*` (W ∈ {8,64}; W=0 would repeat the zero-watcher cell
//!   above): W/2 watchers of each cadence in one flush.
//! - `write_and_flush/J16/W8_mixed`: `set` + `flush` timed together — the
//!   real per-turn shape (the flush-only groups fill the journal in untimed
//!   `iter_batched` setup).
//!
//! Why it matters: fanout is O(journal × watchers) per flushed turn, and the
//! GMCP direction (`docs/interop.md` §8) moves per-write watches onto the
//! per-line path — every server line becomes a store turn. These numbers are
//! the before-picture. Widget-binding invalidation (the flush's path-trie
//! walk) is NOT exercised: no bindings are registered, so its cost here is
//! one empty-trie lookup per journal entry.
//!
//! Requires `smudgy_core`'s `bench-api` feature (exposes [`SessionStore`] and
//! its signature types); the Cargo dev-dependency enables it. JSON values are
//! built through `set`'s own parameter type (`u64: Into<serde_json::Value>`),
//! so the bench crate needs no direct `serde_json` dependency.
//!
//! No log corpus is involved, so `SMUDGY_BENCH_LINES` does not apply here.
//! Env var: `SMUDGY_BENCH_SKIP_SANITY=1` skips the delivery-shape check.

use std::{cell::RefCell, collections::HashSet, hint::black_box};

use criterion::{
    BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use smudgy_core::session::runtime::{
    FunctionId, IsolateId, ProducerKey, RuntimeAction, SessionStore, StorePath, WatchCadence,
};

/// Writes per flushed turn — the journal length the fanout loops replay.
const WRITES_GRID: &[usize] = &[1, 16, 128];

/// Registered watchers per store. 0 isolates the journal-commit floor, 8 is a
/// plausible live session, 64 is a heavily-scripted one.
const WATCHERS_GRID: &[usize] = &[0, 8, 64];

/// The shared first path segment: every write lands under it and every
/// watcher watches it, making all J × W (write, watcher) pairs comparable.
const NAMESPACE: &str = "vitals";

/// `count` distinct two-segment write paths (`vitals.stat_000`, ...).
fn write_paths(count: usize) -> Vec<StorePath> {
    (0..count)
        .map(|i| {
            StorePath::parse(&format!("{NAMESPACE}.stat_{i:03}")).expect("generated path is valid")
        })
        .collect()
}

/// The path every watcher watches: the shared group segment, an
/// ancestor-or-equal of every write path.
fn watched_path() -> StorePath {
    StorePath::parse(NAMESPACE).expect("the namespace segment is a valid path")
}

/// A fresh store carrying `per_write` + `coalesced` watchers, all on
/// [`watched_path`]. Handler ids are minted with `FunctionId::from_raw`:
/// delivery only queues the id inside a `RuntimeAction`; nothing dereferences
/// it without a live engine.
fn store_with_watchers(per_write: usize, coalesced: usize) -> SessionStore {
    let mut store = SessionStore::new();
    let watched = watched_path();
    for index in 0..(per_write + coalesced) {
        let cadence = if index < per_write {
            WatchCadence::PerWrite
        } else {
            WatchCadence::Coalesced
        };
        store.watch(
            ProducerKey::User,
            watched.clone(),
            IsolateId::Main,
            FunctionId::from_raw(index),
            cadence,
        );
    }
    store
}

/// Journals one scalar `set` per path — the exact write path
/// `op_smudgy_store_set` takes. Values are small integers keyed to the write
/// index; paths clone from a precomputed template (the real op layer
/// re-parses the path string per write, so this slightly *under*-counts the
/// write side).
fn fill_journal(store: &mut SessionStore, paths: &[StorePath]) {
    for (index, path) in paths.iter().enumerate() {
        let value = u64::try_from(index).expect("grid sizes fit in u64");
        store
            .set(
                ProducerKey::User,
                path.clone(),
                value.into(),
                IsolateId::Main,
                0,
            )
            .expect("scalar writes stay far under the default budgets");
    }
}

/// Validates the measured machinery delivers the documented shape before any
/// number is trusted: the per-write replay yields exactly J × W deliveries in
/// journal order (each carrying `(path, snapshot)` at depth 1 with the
/// written path's canonical spelling and that write's value), coalesced
/// yields exactly W (each carrying the watched subtree's final committed
/// snapshot), the per-write stream queues ahead of the coalesced one in a
/// mixed flush, and a zero-watcher flush commits the writes while delivering
/// nothing.
fn sanity_check() {
    const J: usize = 16;
    const W: usize = 8;
    let paths = write_paths(J);

    // Per-write cadence: J × W deliveries replaying the journal in write
    // order (watchers fan out per journal entry, so the write index is the
    // delivery index / W).
    let mut store = store_with_watchers(W, 0);
    fill_journal(&mut store, &paths);
    let actions = store.flush();
    assert_eq!(
        actions.len(),
        J * W,
        "per-write replay is one delivery per (write x comparable-path watcher)"
    );
    let mut seen_ids = HashSet::new();
    for (index, action) in actions.iter().enumerate() {
        let RuntimeAction::CallJavascriptFunction {
            id, matches, depth, ..
        } = action
        else {
            panic!("store deliveries must be CallJavascriptFunction actions");
        };
        assert_eq!(*depth, 1, "a depth-0 write delivers at depth 1");
        assert_eq!(matches[0].name.as_deref(), Some("path"));
        assert_eq!(matches[1].name.as_deref(), Some("snapshot"));
        let write = index / W;
        assert_eq!(matches[0].value, format!("{NAMESPACE}.stat_{write:03}"));
        assert_eq!(matches[1].value, format!("{write}"));
        seen_ids.insert(usize::from(*id));
    }
    let expected_ids: HashSet<usize> = (0..W).collect();
    assert_eq!(seen_ids, expected_ids, "every registered watcher delivered");

    // Coalesced cadence: one delivery per watcher, snapshot of the watched
    // subtree's final committed state.
    let mut store = store_with_watchers(0, W);
    fill_journal(&mut store, &paths);
    let actions = store.flush();
    assert_eq!(
        actions.len(),
        W,
        "coalesced is one delivery per watcher per flush"
    );
    for action in &actions {
        let RuntimeAction::CallJavascriptFunction { matches, depth, .. } = action else {
            panic!("store deliveries must be CallJavascriptFunction actions");
        };
        assert_eq!(*depth, 1);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name.as_deref(), Some("snapshot"));
        assert!(
            matches[0].value.contains(r#""stat_000":0"#)
                && matches[0].value.contains(r#""stat_015":15"#),
            "the coalesced snapshot carries the watched subtree's final state"
        );
    }

    // Mixed population: the per-write stream (J × W/2) queues ahead of the
    // coalesced one (W/2); the capture shape (2 vs 1) tells them apart.
    let mut store = store_with_watchers(W / 2, W / 2);
    fill_journal(&mut store, &paths);
    let actions = store.flush();
    assert_eq!(actions.len(), J * (W / 2) + W / 2);
    for (index, action) in actions.iter().enumerate() {
        let RuntimeAction::CallJavascriptFunction { matches, .. } = action else {
            panic!("store deliveries must be CallJavascriptFunction actions");
        };
        let expected_captures: usize = if index < J * (W / 2) { 2 } else { 1 };
        assert_eq!(
            matches.len(),
            expected_captures,
            "per-write deliveries precede coalesced ones"
        );
    }

    // Zero watchers: the flush still completes and commits the journal,
    // delivering nothing.
    let mut store = store_with_watchers(0, 0);
    fill_journal(&mut store, &paths);
    let actions = store.flush();
    assert!(actions.is_empty(), "no watchers means no deliveries");
    assert!(
        !store.has_pending_writes(),
        "the flush consumed the journal"
    );
    assert_eq!(
        store
            .get(&ProducerKey::User, &paths[3], &IsolateId::Main)
            .expect("the zero-watcher flush still committed the write")
            .to_string(),
        "3"
    );

    eprintln!(
        "sanity: per-write {J}x{W}={} deliveries in journal order, coalesced {W}, \
         mixed {}+{}, zero-watcher flush commits with no deliveries",
        J * W,
        J * (W / 2),
        W / 2
    );
}

/// Registers the flush-only grid for one watcher mix: `iter_batched` setup
/// (untimed) journals J scalar writes; the timed routine is exactly one
/// [`SessionStore::flush`], its delivery vector black-boxed and returned so
/// criterion drops it outside the timed region (a live runtime forwards the
/// actions to its queue rather than dropping them). The store persists across
/// iterations — after a warm flush, every timed flush overwrites existing
/// keys (steady state) instead of first-inserting them. `BatchSize::PerIteration`
/// is required: each fill must pair with exactly one flush, because the flush
/// consumes the journal.
fn bench_flush(
    c: &mut Criterion,
    group_name: &str,
    watcher_grid: &[usize],
    split: fn(usize) -> (usize, usize),
) {
    let mut group = c.benchmark_group(group_name);
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for &j in WRITES_GRID {
        let paths = write_paths(j);
        for &w in watcher_grid {
            let (per_write, coalesced) = split(w);
            // RefCell so the setup and routine closures (which criterion
            // holds simultaneously, but calls sequentially) can share one
            // mutable store.
            let store = RefCell::new(store_with_watchers(per_write, coalesced));
            fill_journal(&mut store.borrow_mut(), &paths);
            drop(store.borrow_mut().flush());
            group.throughput(Throughput::Elements(j as u64));
            group.bench_function(BenchmarkId::new(format!("J{j}"), format!("W{w}")), |b| {
                b.iter_batched(
                    || fill_journal(&mut store.borrow_mut(), &paths),
                    |()| black_box(store.borrow_mut().flush()),
                    BatchSize::PerIteration,
                );
            });
        }
    }
    group.finish();
}

/// The real per-turn shape: J=16 scalar `set`s + the flush, timed together
/// against the mixed watcher population (4 per-write + 4 coalesced). The
/// flush-only groups isolate fanout; this one adds the write-side journaling
/// cost (normalize + budget probe + turn-projection upkeep) the same turn
/// pays in a live session.
fn bench_write_and_flush(c: &mut Criterion) {
    const J: usize = 16;
    let mut group = c.benchmark_group("write_and_flush");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    let paths = write_paths(J);
    let mut store = store_with_watchers(4, 4);
    fill_journal(&mut store, &paths);
    drop(store.flush());
    group.throughput(Throughput::Elements(J as u64));
    group.bench_function(BenchmarkId::new("J16", "W8_mixed"), |b| {
        b.iter(|| {
            fill_journal(&mut store, &paths);
            black_box(store.flush())
        });
    });
    group.finish();
}

fn store_fanout(c: &mut Criterion) {
    eprintln!(
        "store fanout: J writes/turn in {WRITES_GRID:?} x W watchers in {WATCHERS_GRID:?}; \
         one producer (user), scalar writes at distinct two-segment paths under `{NAMESPACE}`, \
         every watcher on `{NAMESPACE}` (ancestor of every write, so all J x W pairs are comparable)"
    );
    if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
        sanity_check();
    }
    bench_flush(c, "flush_per_write", WATCHERS_GRID, |w| (w, 0));
    bench_flush(c, "flush_coalesced", WATCHERS_GRID, |w| (0, w));
    // W=0 mixed would repeat the zero-watcher cell the two groups above
    // already measure, so the mixed grid starts at W=8.
    bench_flush(c, "flush_mixed", &[8, 64], |w| (w / 2, w / 2));
    bench_write_and_flush(c);
}

criterion_group!(benches, store_fanout);
criterion_main!(benches);
