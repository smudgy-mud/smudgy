//! The store **read path** through the public JS surface — the regression
//! guard for the leaf-aware (tagged) reads behind `.value` and
//! `.previousValue`. A read resolves per hop: kind first, payload only for
//! leaves and arrays, so a leaf read prices as O(answer), never O(published
//! tree). Whole-tree capture is an explicit materialization through the
//! per-hop view — O(entries) op crossings, deliberately visible as the cost
//! of capture rather than hidden behind a property getter. (The retired
//! `.current` getter read as free while costing the whole tree per read; its
//! `current_leaf/*` cells were re-pointed here when the surface retired —
//! the one sanctioned bench edit, `docs/interop.md` §2 — so
//! renamed cells compare against the `pre-p4` baseline by hand.)
//!
//! Cases (K reads per pass, from a trigger callback; the pass completes on
//! the callback's own `ZZREADDONE` echo):
//! - `value_leaf/{1k,1m}`: `t.value.hp` over published-tree sizes of ~1 KiB
//!   and ~1 MiB serialized — the flatness proof: a leaf read must not scale
//!   with the tree.
//! - `value_leaf/depth1`: `t32k.value.hp` — ONE scalar-leaf proxy read, the
//!   minimal public read (one tagged op + one trap).
//! - `value_leaf/depth4`: `deep.value.a.b.c.leaf` where each of `a`/`b`/`c`
//!   holds ~32 KiB of rows — the proxy-walk depth tax: three object hops
//!   crossing no payload en route to one scalar.
//! - `materialize_32k`: `JSON.stringify(t32k.value)` — explicit whole-tree
//!   capture through the per-hop view. Expected slower per element than the
//!   one-op `.current` serialization it replaced: the cell prices the
//!   explicit capture spelling honestly, not a claim that capture got
//!   cheaper.
//! - `keys_32k`: `Object.keys(t32k.value)` — the enumeration trap.
//!
//! Pinned to the public surface (`.value`, `.previousValue`) so the same
//! bench runs unchanged across internal reworks. Warmup asserts each tree's
//! actual serialized size is within 2× of its label (the grid stays honest),
//! that leaves read back their written values, and that `previousValue`
//! reads a mid-batch base distinct from the live view.
//!
//! Env vars: `SMUDGY_BENCH_SKIP_SANITY=1` skips the warmup assertions.
//! `SMUDGY_BENCH_LINES` does not apply.

use std::{
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use smudgy_bench::session::{BenchSession, bench_runtime, styled};
use smudgy_core::session::styled_line::StyledLine;

const DONE_MARKER: &str = "ZZREADDONE";

/// (case id, fire line, reads per pass). Leaf reads are size-flat, so the
/// 1k/1m cells share K; the materialization pass keeps a small K because it
/// genuinely walks the tree.
const CASES: &[(&str, &str, u64)] = &[
    ("value_leaf/1k", "ZZRC1K", 128),
    ("value_leaf/1m", "ZZRC1M", 128),
    ("materialize_32k", "ZZRC32K", 8),
    ("value_leaf/depth1", "ZZRVD1", 128),
    ("value_leaf/depth4", "ZZRVD4", 32),
    ("keys_32k", "ZZRKEYS", 32),
];

/// Rows sized so one entry serializes to ~55 bytes; counts chosen to land
/// each tree near its size label (asserted at warmup, not assumed).
fn bench_script() -> String {
    format!(
        r#"
import {{ echo, createTrigger, createState }} from "smudgy:core";

// Rows are FAT (a ~96-byte blob each) so the size grid is hit with few
// store entries: the per-producer budget is 100k entries / 16 MiB, and a
// tree of thousands of small objects would breach the ENTRY budget long
// before the byte one (each row object is ~5 accounted entries).
function makeRows(n) {{
    const blob = "x".repeat(96);
    const rows = {{}};
    for (let i = 0; i < n; i++) {{
        rows["r" + String(i).padStart(5, "0")] = {{ hp: i, blob }};
    }}
    return rows;
}}

const t1k = createState("t1k");
const t32k = createState("t32k");
const t1m = createState("t1m");
const deep = createState("deep");

t1k.set({{ hp: 7, rows: makeRows(8) }});
t32k.set({{ hp: 7, rows: makeRows(250) }});
t1m.set({{ hp: 7, rows: makeRows(8000) }});
// Each of a/b/c carries the ~32 KiB rows, so every intermediate proxy hop
// in value_leaf/depth4 reads (and discards) ~32 KiB.
deep.set({{ a: {{ rows: makeRows(250), b: {{ rows: makeRows(250), c: {{ rows: makeRows(250), leaf: 7 }} }} }} }});

function loop(k, f) {{
    let acc = 0;
    for (let i = 0; i < k; i++) acc += f();
    if (acc < 0) echo("unreachable");
    echo("{DONE_MARKER}");
}}

createTrigger(/^ZZRC1K$/, () => loop(128, () => t1k.value.hp), {{ name: "read_v1k" }});
createTrigger(/^ZZRC32K$/, () => loop(8, () => JSON.stringify(t32k.value).length), {{ name: "read_mat32k" }});
createTrigger(/^ZZRC1M$/, () => loop(128, () => t1m.value.hp), {{ name: "read_v1m" }});
createTrigger(/^ZZRVD1$/, () => loop(128, () => t32k.value.hp), {{ name: "read_vd1" }});
createTrigger(/^ZZRVD4$/, () => loop(32, () => deep.value.a.b.c.leaf), {{ name: "read_vd4" }});
createTrigger(/^ZZRKEYS$/, () => loop(32, () => Object.keys(t32k.value).length), {{ name: "read_keys" }});

createTrigger(/^ZZRSANITY$/, () => {{
    echo("SIZE:t1k:" + JSON.stringify(t1k.value).length);
    echo("SIZE:t32k:" + JSON.stringify(t32k.value).length);
    echo("SIZE:t1m:" + JSON.stringify(t1m.value).length);
    // previousValue reads the open batch's base while value reads the journal.
    t1k.set("hp", 8);
    echo("PREV:" + t1k.previousValue.hp + ":" + t1k.value.hp);
    t1k.set("hp", 7);
    echo("LEAF:" + t1m.value.hp + ":" + deep.value.a.b.c.leaf + ":" + Object.keys(t32k.value).length);
}}, {{ name: "read_sanity" }});

echo("READ_READY");
"#
    )
}

/// Assert one `SIZE:<label>:<n>` echo is within 2× of its size label.
fn assert_size(transcript: &[String], label: &str, target: usize) {
    let line = transcript
        .iter()
        .find_map(|t| t.strip_prefix(&format!("SIZE:{label}:")).map(str::to_owned))
        .unwrap_or_else(|| panic!("{label}: no SIZE echo; transcript:\n{transcript:#?}"));
    let actual: usize = line.trim().parse().expect("SIZE echo carries a number");
    assert!(
        actual >= target / 2 && actual <= target * 2,
        "{label}: serialized size {actual} is not within 2x of the {target}-byte label"
    );
}

fn interop_read(c: &mut Criterion) {
    let sanity = env::var("SMUDGY_BENCH_SKIP_SANITY").is_err();
    eprintln!(
        "interop_read: public-surface reads over published-tree sizes ~1k/~32k/~1m; sanity \
         checks {}",
        if sanity { "on" } else { "off" }
    );

    let rt = bench_runtime();
    let mut session =
        BenchSession::start(&rt, "ZZIRead", 9301, &[("bench.js", bench_script())], &[]);

    let mut transcript = Vec::new();
    rt.block_on(async {
        assert!(
            session
                .drain_collect_until("READ_READY", &mut transcript)
                .await,
            "bench script never loaded; transcript:\n{transcript:#?}"
        );
        session.feed(&styled("ZZRSANITY"));
        assert!(
            session.drain_collect_until("LEAF:", &mut transcript).await,
            "sanity probe never echoed; transcript:\n{transcript:#?}"
        );
    });
    if sanity {
        assert_size(&transcript, "t1k", 1024);
        assert_size(&transcript, "t32k", 32 * 1024);
        assert_size(&transcript, "t1m", 1024 * 1024);
        assert!(
            transcript.iter().any(|t| t.contains("PREV:7:8")),
            "previousValue must read the open batch's base (7) while value reads the \
             journal (8); transcript:\n{transcript:#?}"
        );
        assert!(
            transcript.iter().any(|t| t.contains("LEAF:7:7:2")),
            "leaves must read back their written values (t1m.hp=7, deep leaf=7, t32k has 2 \
             root keys); transcript:\n{transcript:#?}"
        );
        eprintln!(
            "  sanity: tree sizes within 2x of labels; leaves and previousValue read back \
             correctly"
        );
    }
    session.drain_stragglers();

    let mut group = c.benchmark_group("interop_read");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for (case, line_text, k) in CASES {
        let lines: Vec<Arc<StyledLine>> = vec![styled(line_text)];
        group.throughput(Throughput::Elements(*k));
        group.bench_function(*case, |b| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        session.drain_stragglers();
                        let start = Instant::now();
                        for line in &lines {
                            session.feed(line);
                        }
                        session.drain_until(DONE_MARKER).await;
                        total += start.elapsed();
                    }
                    total
                })
            });
        });
    }
    group.finish();
}

criterion_group!(benches, interop_read);
criterion_main!(benches);
