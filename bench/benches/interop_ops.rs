//! Interop op calls through the **public JS surface** — the direct
//! before/after measurement for the identity-interning plan. Where
//! `store_fanout` starts below the op layer (pre-parsed `StorePath`s, no V8)
//! and `identity_tax` prices the re-derivation in isolation, this bench pays
//! the whole per-call stack a real script pays: JS sugar (path join,
//! `JSON.stringify`), the op crossing, the creator/producer/path
//! re-derivation, gates, catalogue, and journaling.
//!
//! **Everything is pinned to the public surface** (`handle.set(...)`,
//! `handle.value...`, `handle.emit(...)`) — never op names or signatures — so
//! the identical bench runs unchanged on both sides of an op-signature
//! change. That is the point: `(a JS-surface cell) − (its store_fanout
//! floor)` ≈ the op-layer tax, and the interning work must move exactly that
//! delta while this file stays untouched.
//!
//! Producer cells (one live session each):
//! - `user`: a local module in the MAIN isolate. Its creator descriptor is
//!   the *module* form (`{kind:"module",referrer}`) — the descriptor real
//!   user modules pay, not the bare `{"kind":"user"}`.
//! - `package`: an installed-untrusted package in its OWN sandboxed isolate
//!   (consented `automations`/`echo`/`interop:*`), whose creator descriptor
//!   is the package form — the biggest parse, where interning should win
//!   most visibly.
//!
//! Cases per producer (K ops per timed pass, from a trigger callback so the
//! loop runs in dispatch context like real automation code):
//! - `set128`: K=128 scalar `handle.set(path_i, i)` at 128 distinct
//!   pre-built two-segment paths — one turn, one flush; the store-side floor
//!   is `store_fanout`'s `J128/W0` cell.
//! - `emit128`: K=128 `handle.emit(i)` with **zero subscribers** — the pure
//!   op-layer emit cost (creator parse, stamp+fold, catalogue, registry
//!   miss), no fanout.
//! - `get128`: K=128 reads of one scalar leaf through the producer's
//!   `.value` proxy (one op `get` + one proxy trap each — the public
//!   fine-grained read surface).
//! - `set_per_turn64`: 64 fed lines, each firing a trigger that does ONE
//!   `set` — the per-line turn shape (64 journal flushes), keeping the
//!   in-turn loop honest about batching.
//!
//! Each pass completes on a count-based echo from the callback itself
//! (`ZZOPSDONE`), so the timed window spans exactly the pass's dispatch +
//! flush work. Warmup asserts read-your-writes once per producer
//! (`SANITY_SET:5` — `.value.stat_005` reads back the written 5).
//!
//! Env vars: `SMUDGY_BENCH_SKIP_SANITY=1` skips the warmup assertions.
//! `SMUDGY_BENCH_LINES` does not apply (K is the workload, not a corpus).

use std::{
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use smudgy_bench::session::{BenchPackage, BenchSession, bench_runtime, styled};
use smudgy_core::session::styled_line::StyledLine;
use smudgy_script::{PackagePermissions, SmudgyCapabilities};

/// Ops per timed pass for the in-callback loop cases.
const K: usize = 128;

/// Fed lines per pass for the per-turn shape (each line = one set + flush).
const PER_TURN_LINES: usize = 64;

const DONE_MARKER: &str = "ZZOPSDONE";

/// The bench script, identical for both producers: interop handles plus one
/// trigger per case. TypeScript-free (packages ship plain JS in this
/// harness, and the main module loader takes .js as-is).
fn bench_script() -> String {
    format!(
        r#"
import {{ echo, createTrigger, createState, createEvent }} from "smudgy:core";

const K = {K};
const paths = [];
for (let i = 0; i < K; i++) paths.push("stat_" + String(i).padStart(3, "0"));

const bench = createState("bench");
const tick = createEvent("tick");
let perTurnN = 0;

createTrigger(/^ZZSET$/, () => {{
    for (let i = 0; i < K; i++) bench.set(paths[i], i);
    echo("{DONE_MARKER}");
}}, {{ name: "ops_set" }});

createTrigger(/^ZZEMIT$/, () => {{
    for (let i = 0; i < K; i++) tick.emit(i);
    echo("{DONE_MARKER}");
}}, {{ name: "ops_emit" }});

createTrigger(/^ZZGET$/, () => {{
    let acc = 0;
    for (let i = 0; i < K; i++) acc += bench.value.stat_000;
    if (acc < 0) echo("unreachable");
    echo("{DONE_MARKER}");
}}, {{ name: "ops_get" }});

createTrigger(/^ZZW$/, () => {{
    bench.set("hp", perTurnN++);
}}, {{ name: "ops_per_turn" }});

createTrigger(/^ZZWSYNC$/, () => {{
    echo("{DONE_MARKER}");
}}, {{ name: "ops_per_turn_sync" }});

createTrigger(/^ZZSANITY$/, () => {{
    echo("SANITY_SET:" + bench.value.stat_005);
}}, {{ name: "ops_sanity" }});

echo("OPS_READY");
"#
    )
}

/// The four capabilities the package cell needs: trigger creation (the
/// per-case drivers), echo (the completion channel), and both interop seats.
fn package_consent() -> PackagePermissions {
    PackagePermissions {
        smudgy: SmudgyCapabilities {
            create_triggers: true,
            echo: true,
            interop_read: true,
            interop_write: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

struct Cell {
    id: &'static str,
    session: BenchSession,
}

/// Start one producer cell's session and prove the script loaded + the
/// handles work (read-your-writes on the fifth written path).
fn start_cell(
    rt: &tokio::runtime::Runtime,
    id: &'static str,
    server: &'static str,
    session_id: u32,
    as_package: bool,
    sanity: bool,
) -> Cell {
    let mut session = if as_package {
        BenchSession::start(
            rt,
            server,
            session_id,
            &[],
            &[BenchPackage {
                owner: "bench",
                name: "interop-ops",
                source: bench_script(),
                consent: package_consent(),
            }],
        )
    } else {
        BenchSession::start(rt, server, session_id, &[("bench.js", bench_script())], &[])
    };

    let mut transcript = Vec::new();
    rt.block_on(async {
        assert!(
            session
                .drain_collect_until("OPS_READY", &mut transcript)
                .await,
            "{id}: bench script never loaded; transcript:\n{transcript:#?}"
        );
        // One untimed SET pass so the sanity read has state to see, then the
        // sanity probe itself.
        session.feed(&styled("ZZSET"));
        session.feed(&styled("ZZSANITY"));
        let ok = session
            .drain_collect_until("SANITY_SET:", &mut transcript)
            .await;
        assert!(
            ok,
            "{id}: sanity probe never echoed; transcript:\n{transcript:#?}"
        );
    });
    if sanity {
        assert!(
            transcript.iter().any(|t| t.contains("SANITY_SET:5")),
            "{id}: read-your-writes failed (.value.stat_005 must read back 5); transcript:\n{transcript:#?}"
        );
    }
    session.drain_stragglers();
    Cell { id, session }
}

/// Time `iters` passes of `lines` against `session`, sweeping stragglers
/// between passes so backlog never bleeds across samples.
fn timed_passes(
    rt: &tokio::runtime::Runtime,
    session: &mut BenchSession,
    lines: &[Arc<StyledLine>],
    iters: u64,
) -> Duration {
    rt.block_on(async {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            session.drain_stragglers();
            let start = Instant::now();
            for line in lines {
                session.feed(line);
            }
            session.drain_until(DONE_MARKER).await;
            total += start.elapsed();
        }
        total
    })
}

fn interop_ops(c: &mut Criterion) {
    let sanity = env::var("SMUDGY_BENCH_SKIP_SANITY").is_err();
    eprintln!(
        "interop_ops: K={K} ops/pass via the public JS surface; producers: user module (main \
         isolate) and sandboxed package (own isolate); sanity checks {}",
        if sanity { "on" } else { "off" }
    );

    let rt = bench_runtime();
    let mut cells = vec![
        start_cell(&rt, "user", "ZZIOpsUser", 9201, false, sanity),
        start_cell(&rt, "package", "ZZIOpsPkg", 9202, true, sanity),
    ];

    // The per-turn pass: 64 one-set lines then the sync line whose trigger
    // echoes the marker. Dispatch is in order, so the echo proves all 64
    // set+flush turns completed.
    let per_turn_lines: Vec<Arc<StyledLine>> = std::iter::repeat_with(|| styled("ZZW"))
        .take(PER_TURN_LINES)
        .chain(std::iter::once(styled("ZZWSYNC")))
        .collect();

    let loop_cases: &[(&str, &str, u64)] = &[
        ("set128", "ZZSET", K as u64),
        ("emit128", "ZZEMIT", K as u64),
        ("get128", "ZZGET", K as u64),
    ];

    let mut group = c.benchmark_group("interop_ops");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for cell in &mut cells {
        for (case, line_text, elements) in loop_cases {
            let lines = vec![styled(line_text)];
            group.throughput(Throughput::Elements(*elements));
            group.bench_function(format!("{}/{case}", cell.id), |b| {
                b.iter_custom(|iters| timed_passes(&rt, &mut cell.session, &lines, iters));
            });
        }
        group.throughput(Throughput::Elements(PER_TURN_LINES as u64));
        group.bench_function(format!("{}/set_per_turn64", cell.id), |b| {
            b.iter_custom(|iters| timed_passes(&rt, &mut cell.session, &per_turn_lines, iters));
        });
    }
    group.finish();
}

criterion_group!(benches, interop_ops);
criterion_main!(benches);
