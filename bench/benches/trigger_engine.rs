//! Drives smudgy's **real** trigger engine end-to-end, over every corpus in
//! `bench/logs/`. The engine carries one literal trigger per **representative
//! item substitution** (~10,000, each pattern the regex-escaped name; see
//! `load_item_names_10k`) **plus** the ~100 shared `REGEX_TRIGGERS` — the same
//! mixed literal+regex shape `trigger_matching.rs`'s `scan_mixed` group uses
//! (though that group runs the smaller ~6,350 `load_item_names` corpus), but run
//! through `Manager::process_incoming_line` rather than a matcher reimplementation.
//!
//! What it measures (the full shipped path in `core/src/session/runtime/`):
//!   - `engine_scan/<file>/{lines,bytes}`: per line, the tiered `PatternSet`
//!     match (`matcher.rs`), the per-hit `enabled`/anti-pattern checks, and the
//!     `captures` re-run `Trigger::run` performs to populate `$0`, dispatching
//!     to a `ScriptAction::Noop` (so no JS isolate is constructed or invoked).
//!     The same scan is registered twice per log file under different
//!     `Throughput`s, so criterion reports both lines/sec (the `/lines` id) and
//!     MB/sec (the `/bytes` id) — at the cost of scanning each corpus twice.
//!   - `engine_build/dirty_rebuild/<n>`: the `PatternSet`-rebuild stall. Any
//!     trigger add/remove/enable flips trigger.rs's dirty flag, and the NEXT
//!     incoming line pays `rebuild_trigger_regex_set` — all four tiered
//!     `PatternSet` builds — before it can match. Each iteration pushes one
//!     throwaway trigger, processes one non-matching line (exactly one
//!     full-set rebuild), and removes the throwaway again. The steady-state
//!     single-line cost is negligible against the rebuild (`engine_scan` puts
//!     it at µs scale), so the number IS the stall a session feels whenever a
//!     script mutates triggers mid-stream, parametrized by trigger count.
//!
//! Requires `smudgy_core`'s `bench-api` feature (exposes `Manager` /
//! `PushTriggerParams`); the Cargo dev-dependency enables it.
//!
//! Env vars: `SMUDGY_BENCH_LINES=n` truncates each corpus (faster runs);
//! `SMUDGY_BENCH_SKIP_SANITY=1` skips the `engine_build` check that a pushed
//! trigger really is matchable on the very next processed line.

use std::{hint::black_box, sync::Arc};

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use smudgy_bench::{REGEX_TRIGGERS, load_item_names_10k, log_corpora};
use smudgy_core::session::{
    runtime::{
        BenchActionQueue, IsolateId, Manager, Origin, PushTriggerParams, ScriptAction,
        SharedAutomationRegistry,
    },
    styled_line::StyledLine,
};

/// Feature-gated trigger action observation handle.
type Queue = BenchActionQueue;

/// Pushes one enabled single-pattern trigger carrying `action`. The corpus
/// triggers all carry `ScriptAction::Noop` (no JS engine exists here, and a
/// `Noop` fire enqueues nothing); the `engine_build` sanity probe passes
/// `SendRaw` instead — the cheapest action whose fire lands on the action
/// queue and is therefore observable from outside the engine.
fn push_one_trigger(mgr: &mut Manager, name: String, pattern: String, action: ScriptAction) {
    // Both `name` and `patterns` are passed by reference, so they must
    // outlive the call; bind them to locals.
    let trigger_name = Arc::new(name);
    let patterns = Arc::new(vec![pattern]);
    let empty: Arc<Vec<String>> = Arc::new(Vec::new());
    mgr.push_trigger(PushTriggerParams {
        isolate: IsolateId::Main,
        origin: Origin::User,
        name: &trigger_name,
        patterns: &patterns,
        raw_patterns: &empty,
        anti_patterns: &empty,
        action,
        prompt: false,
        enabled: true,
        priority: 0,
        fallthrough: false,
        fire_limit: None,
        line_limit: None,
        source: None,
    })
    .expect("push_trigger");
}

/// Builds a `Manager` carrying one enabled `Noop` trigger per item name (each
/// pattern the regex-escaped literal → the Aho-Corasick tier) plus one per
/// entry in `regexes` (→ the regex-filtered tier). Returns the engine's action
/// queue so callers can drain it per pass.
fn build_manager(names: &[String], regexes: &[&str]) -> (Manager, Queue) {
    let registry = SharedAutomationRegistry::default();
    let (mut mgr, queue) = Manager::new_for_bench(Arc::new(String::from(";")), registry);

    for (i, name) in names.iter().enumerate() {
        push_one_trigger(
            &mut mgr,
            format!("item_{i}"),
            regex::escape(name),
            ScriptAction::Noop,
        );
    }
    for (i, pattern) in regexes.iter().enumerate() {
        push_one_trigger(
            &mut mgr,
            format!("regex_{i}"),
            (*pattern).to_owned(),
            ScriptAction::Noop,
        );
    }

    (mgr, queue)
}

fn trigger_engine(c: &mut Criterion) {
    let names = load_item_names_10k();
    let corpora = log_corpora();
    eprintln!(
        "{} representative item substitutions + {} complex regex triggers; {} log file(s)",
        names.len(),
        REGEX_TRIGGERS.len(),
        corpora.len()
    );

    let (mut mgr, queue) = build_manager(&names, REGEX_TRIGGERS);
    // First incoming line triggers the one-time PatternSet rebuild; warm it up
    // outside the timed loop so per-file scans measure steady-state matching.
    mgr.process_incoming_line(&Arc::new(StyledLine::new("warmup", Vec::new())))
        .expect("warmup");
    queue.clear();

    let mut group = c.benchmark_group("engine_scan");
    group.sample_size(10);
    // Flat sampling: criterion's recommended mode for benches that run many ms
    // per iteration. Avoids the "unable to complete 10 samples" warning and is
    // statistically more appropriate than the default linear sampling here.
    group.sampling_mode(SamplingMode::Flat);
    for (name, lines) in &corpora {
        let bytes: u64 = lines.iter().map(|l| l.len() as u64).sum();
        eprintln!("  {name}: {} lines / {bytes} bytes", lines.len());
        let styled: Vec<Arc<StyledLine>> = lines
            .iter()
            .map(|l| Arc::new(StyledLine::new(l, Vec::new())))
            .collect();

        // One full scan of the corpus. criterion attaches a single `Throughput`
        // per benchmark, so to report both lines/sec and MB/sec we register the
        // identical work twice under different throughputs:
        //   `engine_scan/<file>/lines` → `Throughput::Elements`     (Kelem/sec)
        //   `engine_scan/<file>/bytes` → `Throughput::BytesDecimal` (MB/sec)
        // Each id is timed independently, so this scans the corpus twice; the two
        // times should agree within noise and cross-check each other.
        let mut one_pass = || {
            for line in &styled {
                mgr.process_incoming_line(line)
                    .expect("process_incoming_line");
            }
            // Drop the matched-trigger actions the engine enqueues, else they
            // pile up over a sample iteration.
            queue.clear();
        };

        group.throughput(Throughput::Elements(styled.len() as u64));
        group.bench_function(BenchmarkId::new(name.as_str(), "lines"), |b| {
            b.iter(&mut one_pass);
        });
        group.throughput(Throughput::BytesDecimal(bytes));
        group.bench_function(BenchmarkId::new(name.as_str(), "bytes"), |b| {
            b.iter(&mut one_pass);
        });
    }
    group.finish();

    // The scan engine is done; free it before building the engine_build
    // managers so peak memory stays flat (each carries its own compiled
    // pattern tiers).
    drop(mgr);
    drop(queue);

    // engine_build: the dirty-flag rebuild stall. The `Manager` rebuilds
    // lazily — any push/remove/enable marks the set dirty, and the next
    // `process_incoming_line` pays `rebuild_trigger_regex_set` (all four
    // tiered `PatternSet`s) before matching its line. Per iteration: push one
    // throwaway trigger (marks dirty), process one non-matching line (exactly
    // one full-set rebuild), remove the throwaway (leaves the set dirty for
    // the next iteration's push). The probe line's own scan is µs-scale
    // against the rebuild, so the measured time IS the stall.
    let probe = Arc::new(StyledLine::new("zzqx rebuild stall probe zzqx", Vec::new()));
    let mut group = c.benchmark_group("engine_build");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for size in [1_000usize, 10_000] {
        let size = size.min(names.len());
        let (mut mgr, queue) = build_manager(&names[..size], REGEX_TRIGGERS);
        eprintln!(
            "  engine_build: {size} literal + {} regex triggers",
            REGEX_TRIGGERS.len()
        );
        // Pay the initial (cold) build outside the loop; iterations then time
        // pure dirty-flag rebuilds.
        mgr.process_incoming_line(&probe).expect("initial build");
        queue.clear();

        if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
            // The measurement is honest only if (a) a trigger pushed before a
            // line really is matchable on that line — i.e. the lazy rebuild
            // ran and integrated it — and (b) the probe line fires nothing by
            // itself, so timed iterations neither accrue queued actions nor
            // pay dispatch work. `SendRaw` (not `Noop`) so the sanity fire is
            // observable on the action queue.
            push_one_trigger(
                &mut mgr,
                "zz_sanity".to_owned(),
                regex::escape("zzqx rebuild stall probe zzqx"),
                ScriptAction::SendRaw(Arc::new(String::from("zz"))),
            );
            mgr.process_incoming_line(&probe).expect("sanity rebuild");
            assert!(
                !queue.is_empty(),
                "a trigger pushed before the line must fire on it: the lazy rebuild did not run"
            );
            mgr.remove_trigger(&IsolateId::Main, &Origin::User, "zz_sanity");
            queue.clear();
            mgr.process_incoming_line(&probe).expect("sanity probe");
            assert!(
                queue.is_empty(),
                "the probe line must not fire any corpus trigger"
            );
            eprintln!("  engine_build sanity: rebuilds integrate pushes; probe line is inert");
        }

        // Each timed iteration brackets the rebuild with one push (`Trigger`
        // construction + one regex compile) and one remove (`remove_named`'s
        // O(n) name-index rebuild, ~n String clones) — real mutation
        // bookkeeping, but a single mid-session mutation pays push OR remove
        // plus one rebuild, whereas the iteration pays both. At 10k triggers
        // that bookkeeping is a small single-digit share of the multi-ms
        // four-tier rebuild.
        group.bench_function(BenchmarkId::new("dirty_rebuild", size), |b| {
            b.iter(|| {
                push_one_trigger(
                    &mut mgr,
                    "zz_throwaway".to_owned(),
                    regex::escape("zzqx throwaway trigger zzqx"),
                    ScriptAction::Noop,
                );
                mgr.process_incoming_line(&probe).expect("rebuild");
                mgr.remove_trigger(&IsolateId::Main, &Origin::User, "zz_throwaway");
                black_box(&mgr);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, trigger_engine);
criterion_main!(benches);
