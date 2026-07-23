//! Interop **delivery** — what `store_fanout` deliberately stops short of.
//! That bench prices the store's flush loop up to queuing
//! `CallJavascriptFunction` actions; this one runs the deliveries through V8:
//! per-subscriber capture allocation and payload `String` clones, v8 entry,
//! the `matches`-object build, `JSON.parse` of the payload, and the
//! deep-freeze walk in the handler. These are the costs the
//! `MatchCapture`/`Arc<str>` hygiene items will move, so the grid axes are
//! exactly the fanout count and the payload size.
//!
//! In-isolate cells (one live session; each case has its OWN event/state
//! handle + subscriber population + counter, so cases never cross-feed):
//! - `emit_fanout/S{1,8,64}`: K=32 emits per pass at ~1 KiB payload, S
//!   subscribers each — the completion echo fires on the K×S-th delivery,
//!   so the timed window spans the full fan-out.
//! - `emit_payload/P{64,16k}`: the payload-size slope at S=8 (the ~1 KiB
//!   midpoint is `emit_fanout/S8`).
//! - `watch_coalesced/W{8,64}`: 16 one-set turns per pass, W turn-coalesced
//!   watchers — one snapshot serialization + delivery per watcher per flush.
//! - `watch_per_write/W8`: one turn of J=32 sets, 8 per-write watchers —
//!   the journal-replay cadence, 256 deliveries carrying (path, value).
//!
//! Cross-isolate cell (second session): a sandboxed package's trigger emits
//! K=32 (~1 KiB) and the MAIN isolate consumes through the public
//! `smudgy:events/<owner>/<name>` scheme — the real cross-package topology,
//! adding per-delivery isolate entry to everything above.
//!
//! Emission uses the public producer surface (`handle.emit`/`handle.set`).
//! In-isolate subscriptions use the `__smudgy_interop_consumer` /
//! `__smudgy_store` host hooks — the same seam the `smudgy:events/…` scheme
//! stubs and the core integration tests build on — because the scheme
//! modules address package producers, not the main isolate's own `user`
//! producer. Setup-side only; the measured path (emit → fanout → handler) is
//! identical to scheme-consumer delivery.
//!
//! Env vars: `SMUDGY_BENCH_SKIP_SANITY=1` skips the warmup payload-shape
//! assertion. `SMUDGY_BENCH_LINES` does not apply.

use std::{
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use smudgy_bench::session::{BenchPackage, BenchSession, bench_runtime, styled};
use smudgy_core::session::styled_line::StyledLine;
use smudgy_script::{PackagePermissions, SmudgyCapabilities};

/// Emits per pass (the fanout multiplies this by S).
const K_EMITS: u64 = 32;
/// One-set turns per pass in the coalesced-watch cells.
const K_TURNS: u64 = 16;
/// Sets in the single per-write turn.
const K_WRITES: u64 = 32;

/// In-isolate cases: (id, fire line, done marker, deliveries per pass).
const IN_ISOLATE_CASES: &[(&str, &str, &str, u64)] = &[
    ("emit_fanout/S1", "ZZDS1", "ZZDELDONE_s1", K_EMITS),
    ("emit_fanout/S8", "ZZDS8", "ZZDELDONE_s8", K_EMITS * 8),
    ("emit_fanout/S64", "ZZDS64", "ZZDELDONE_s64", K_EMITS * 64),
    ("emit_payload/P64", "ZZDP64", "ZZDELDONE_p64", K_EMITS * 8),
    (
        "emit_payload/P16k",
        "ZZDP16K",
        "ZZDELDONE_p16k",
        K_EMITS * 8,
    ),
    ("watch_per_write/W8", "ZZDPW", "ZZDELDONE_pw", K_WRITES * 8),
];

/// The coalesced-watch cases feed `K_TURNS` lines per pass instead of one.
const COALESCED_CASES: &[(&str, &str, &str, u64)] = &[
    ("watch_coalesced/W8", "ZZDW8", "ZZDELDONE_w8", K_TURNS * 8),
    (
        "watch_coalesced/W64",
        "ZZDW64",
        "ZZDELDONE_w64",
        K_TURNS * 64,
    ),
];

const CROSS_DONE: &str = "ZZDELDONE_x";

/// The in-isolate module: one emit/watch rig per case. `rigEmit(name, S,
/// target, payload, marker)` builds an event handle + S counting subscribers
/// whose last delivery echoes the marker and resets the counter (passes are
/// serialized, so the reset is race-free).
fn main_script() -> String {
    format!(
        r#"
import {{ echo, createTrigger, createState, createEvent }} from "smudgy:core";

const consumer = globalThis.__smudgy_interop_consumer("user");
const store = globalThis.__smudgy_store;

const p64 = {{ blob: "x".repeat(40) }};
const p1k = {{ blob: "x".repeat(1000) }};
const p16k = {{ blob: "x".repeat(16000) }};

function rigEmit(name, subs, perPass, payload, fireRe, marker) {{
    const handle = createEvent(name);
    const target = perPass * subs;
    let got = 0;
    for (let s = 0; s < subs; s++) {{
        consumer.event(name).on((p) => {{
            if (p.blob.length < 1) echo("unreachable");
            got++;
            if (got === target) {{ got = 0; echo(marker); }}
        }});
    }}
    createTrigger(fireRe, () => {{
        for (let i = 0; i < perPass; i++) handle.emit(payload);
    }}, {{ name: "del_" + name }});
    return handle;
}}

rigEmit("ticks1", 1, {K_EMITS}, p1k, /^ZZDS1$/, "ZZDELDONE_s1");
rigEmit("ticks8", 8, {K_EMITS}, p1k, /^ZZDS8$/, "ZZDELDONE_s8");
rigEmit("ticks64", 64, {K_EMITS}, p1k, /^ZZDS64$/, "ZZDELDONE_s64");
rigEmit("tickp64", 8, {K_EMITS}, p64, /^ZZDP64$/, "ZZDELDONE_p64");
const sanityHandle = rigEmit("tickp16k", 8, {K_EMITS}, p16k, /^ZZDP16K$/, "ZZDELDONE_p16k");

// Coalesced watch: W watchers on the rig's own state handle; each one-set
// turn flushes one snapshot delivery per watcher.
function rigWatch(name, watchers, perPass, fireRe, marker) {{
    const handle = createState(name);
    handle.set({{ hp: 0, tag: "y".repeat(150) }});
    const target = perPass * watchers;
    let got = 0;
    let n = 0;
    for (let w = 0; w < watchers; w++) {{
        store.watch("user", name, () => {{
            got++;
            if (got === target) {{ got = 0; echo(marker); }}
        }});
    }}
    createTrigger(fireRe, () => {{ handle.set("hp", n++); }}, {{ name: "del_" + name }});
}}

rigWatch("wc8", 8, {K_TURNS}, /^ZZDW8$/, "ZZDELDONE_w8");
rigWatch("wc64", 64, {K_TURNS}, /^ZZDW64$/, "ZZDELDONE_w64");

// Per-write watch: 8 onWrite watchers, one turn of {K_WRITES} sets each pass.
{{
    const handle = createState("wpw");
    handle.set({{ hp: 0 }});
    const target = {K_WRITES} * 8;
    let got = 0;
    let n = 0;
    for (let w = 0; w < 8; w++) {{
        store.onWrite("user", "wpw", () => {{
            got++;
            if (got === target) {{ got = 0; echo("ZZDELDONE_pw"); }}
        }});
    }}
    createTrigger(/^ZZDPW$/, () => {{
        for (let i = 0; i < {K_WRITES}; i++) handle.set("hp", n++);
    }}, {{ name: "del_wpw" }});
}}

createTrigger(/^ZZDSANITY$/, () => {{
    consumer.event("tickp16k").once((p) => echo("SANITY_PAYLOAD:" + p.blob.length));
    sanityHandle.emit(p16k);
}}, {{ name: "del_sanity" }});

echo("DELIVERY_READY");
"#
    )
}

/// Cross-isolate: the package emits from its own sandbox on a trigger line.
fn package_script() -> String {
    format!(
        r#"
import {{ createTrigger, createEvent }} from "smudgy:core";
const tick = createEvent("tick");
const p1k = {{ blob: "x".repeat(1000) }};
createTrigger(/^ZZXEMIT$/, () => {{
    for (let i = 0; i < {K_EMITS}; i++) tick.emit(p1k);
}}, {{ name: "xdel_emit" }});
"#
    )
}

/// The cross-isolate consumer: the public scheme import, counting to K.
fn cross_consumer_script() -> String {
    format!(
        r#"
import {{ echo }} from "smudgy:core";
import {{ tick }} from "smudgy:events/bench/delivery";
let got = 0;
tick.on((p) => {{
    if (p.blob.length < 1) echo("unreachable");
    got++;
    if (got === {K_EMITS}) {{ got = 0; echo("{CROSS_DONE}"); }}
}});
echo("XCONSUMER_READY");
"#
    )
}

fn timed_passes(
    rt: &tokio::runtime::Runtime,
    session: &mut BenchSession,
    lines: &[Arc<StyledLine>],
    marker: &str,
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
            session.drain_until(marker).await;
            total += start.elapsed();
        }
        total
    })
}

fn interop_delivery(c: &mut Criterion) {
    let sanity = env::var("SMUDGY_BENCH_SKIP_SANITY").is_err();
    eprintln!(
        "interop_delivery: emit fanout (S subscribers x payload sizes) and watch cadences \
         through live V8 delivery; K={K_EMITS} emits / {K_TURNS} set-turns per pass; sanity \
         checks {}",
        if sanity { "on" } else { "off" }
    );

    let rt = bench_runtime();
    let mut main_session = BenchSession::start(
        &rt,
        "ZZIDelivery",
        9401,
        &[("bench.js", main_script())],
        &[],
    );
    let mut transcript = Vec::new();
    rt.block_on(async {
        assert!(
            main_session
                .drain_collect_until("DELIVERY_READY", &mut transcript)
                .await,
            "delivery script never loaded; transcript:\n{transcript:#?}"
        );
        main_session.feed(&styled("ZZDSANITY"));
        assert!(
            main_session
                .drain_collect_until("SANITY_PAYLOAD:", &mut transcript)
                .await,
            "payload sanity probe never echoed; transcript:\n{transcript:#?}"
        );
    });
    if sanity {
        assert!(
            transcript
                .iter()
                .any(|t| t.contains("SANITY_PAYLOAD:16000")),
            "a delivered 16k payload must arrive intact; transcript:\n{transcript:#?}"
        );
        eprintln!("  sanity: 16k payload delivered intact through the consumer handle");
    }
    main_session.drain_stragglers();

    let cross_package = BenchPackage {
        owner: "bench",
        name: "delivery",
        source: package_script(),
        consent: PackagePermissions {
            smudgy: SmudgyCapabilities {
                create_triggers: true,
                interop_write: true,
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let mut cross_session = BenchSession::start(
        &rt,
        "ZZIDeliveryX",
        9402,
        &[("consumer.js", cross_consumer_script())],
        &[cross_package],
    );
    let mut transcript = Vec::new();
    rt.block_on(async {
        assert!(
            cross_session
                .drain_collect_until("XCONSUMER_READY", &mut transcript)
                .await,
            "cross-isolate consumer never loaded; transcript:\n{transcript:#?}"
        );
    });
    cross_session.drain_stragglers();

    let mut group = c.benchmark_group("interop_delivery");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);

    for (case, line_text, marker, deliveries) in IN_ISOLATE_CASES {
        let lines = vec![styled(line_text)];
        group.throughput(Throughput::Elements(*deliveries));
        group.bench_function(*case, |b| {
            b.iter_custom(|iters| timed_passes(&rt, &mut main_session, &lines, marker, iters));
        });
    }
    for (case, line_text, marker, deliveries) in COALESCED_CASES {
        let lines: Vec<Arc<StyledLine>> = std::iter::repeat_with(|| styled(line_text))
            .take(usize::try_from(K_TURNS).expect("small constant"))
            .collect();
        group.throughput(Throughput::Elements(*deliveries));
        group.bench_function(*case, |b| {
            b.iter_custom(|iters| timed_passes(&rt, &mut main_session, &lines, marker, iters));
        });
    }
    {
        let lines = vec![styled("ZZXEMIT")];
        group.throughput(Throughput::Elements(K_EMITS));
        group.bench_function("emit_cross_isolate/S1", |b| {
            b.iter_custom(|iters| timed_passes(&rt, &mut cross_session, &lines, CROSS_DONE, iters));
        });
    }
    group.finish();
}

criterion_group!(benches, interop_delivery);
criterion_main!(benches);
