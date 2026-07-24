//! The per-line cost of the **trigger-body verbs** — what a busy profile's
//! handlers actually do on a large share of incoming lines. `script_dispatch`
//! deliberately prices EMPTY bodies (the fire tax + capture marshalling);
//! nothing there would catch a regression in the line-edit or echo ops, which
//! run per fire in real profiles. Each cell here is `script_dispatch`'s shape
//! (M fed lines + a ZZSYNC barrier per timed pass, one live session per cell)
//! with a body that exercises one verb family:
//!
//! - `empty`: the control — same trigger, `() => {}` body. Deltas against
//!   this isolate the verb; the control also cross-checks against
//!   `script_dispatch/fire0` (different fed-line text, so compare deltas,
//!   not absolutes).
//! - `gag`: `line.gag()` — one fast op. NOTE the delta can run *negative*:
//!   a gagged line skips display emission + the screen-log append, so the
//!   verb's op cost is partly paid back downstream. That is the honest
//!   per-line effect of gagging.
//! - `read_echo`: `echo(line.text)` — a current-line text read plus an echo
//!   (doubles the display traffic; also honest).
//! - `highlight`: `line.highlight("fountain", …)` — a text read, the JS
//!   `indexOf`/UTF-8 offset math, and the highlight op against the in-flight
//!   line.
//!
//! Every cell's fed line is byte-identical (same text, same length), so
//! byte-proportional per-line work cancels exactly out of the deltas.
//!
//! Env vars: `SMUDGY_BENCH_LINES=n` lowers M (default 500, truncate-only);
//! `SMUDGY_BENCH_SKIP_SANITY=1` skips the warmup assertions (each verb
//! proves its observable effect once: the gag cell's fed line must NOT
//! reach the display, the echo cell's echo must).

use std::{
    env,
    time::{Duration, Instant},
};

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use smudgy_bench::session::{BenchSession, bench_runtime, styled};

const DEFAULT_LINES_PER_ITER: u64 = 500;
const DONE_MARKER: &str = "ZZDONE";
const ECHO_MARKER: &str = "ZZECHOED";

/// The one fed line, shared by every cell (the trigger matches its prefix;
/// the tail pads it to a fixed length so cells are byte-identical).
const FEED_TEXT: &str =
    "ZZVERB the fountain bubbles quietly in the empty plaza xxxxxxxxxxxxxxxxxxxxxxxx";

/// (cell id, session id, JS body). Bodies run per fed line.
const CELLS: &[(&str, u32, &str)] = &[
    ("empty", 9501, ""),
    ("gag", 9502, "line.gag();"),
    ("read_echo", 9503, r#"echo("ZZECHOED " + line.text);"#),
    (
        "highlight",
        9504,
        r##"line.highlight("fountain", { fg: "#ff0000" });"##,
    ),
];

fn lines_per_iter() -> u64 {
    match env::var("SMUDGY_BENCH_LINES") {
        Ok(v) => {
            DEFAULT_LINES_PER_ITER.min(v.parse().expect("SMUDGY_BENCH_LINES must be a number"))
        }
        Err(_) => DEFAULT_LINES_PER_ITER,
    }
}

fn module_source(body: &str) -> String {
    format!(
        r#"
import {{ echo, createTrigger, line }} from "smudgy:core";
createTrigger(/^ZZSYNC$/, () => {{ echo("{DONE_MARKER}"); }}, {{ name: "sync" }});
createTrigger(/^ZZVERB /, () => {{ {body} }}, {{ name: "verb" }});
echo("VERBS_READY");
"#
    )
}

fn trigger_verbs(c: &mut Criterion) {
    let m = lines_per_iter();
    let sanity = env::var("SMUDGY_BENCH_SKIP_SANITY").is_err();
    eprintln!(
        "trigger_verbs: {m} fed lines/pass + 1 barrier; every cell's fed line is byte-identical; \
         sanity checks {}",
        if sanity { "on" } else { "off" }
    );

    let rt = bench_runtime();
    let feed_line = styled(FEED_TEXT);
    let sync_line = styled("ZZSYNC");

    let mut sessions: Vec<(&'static str, BenchSession)> = Vec::new();
    for (id, session_id, body) in CELLS {
        let server = format!("ZZVerbs{id}");
        let mut session = BenchSession::start(
            &rt,
            &server,
            *session_id,
            &[("bench.js", module_source(body))],
            &[],
        );
        let mut transcript = Vec::new();
        rt.block_on(async {
            assert!(
                session
                    .drain_collect_until("VERBS_READY", &mut transcript)
                    .await,
                "{id}: module never loaded; transcript:\n{transcript:#?}"
            );
            session.feed(&feed_line);
            session.feed(&sync_line);
            assert!(
                session
                    .drain_collect_until(DONE_MARKER, &mut transcript)
                    .await,
                "{id}: warmup barrier never arrived; transcript:\n{transcript:#?}"
            );
        });
        if sanity {
            let fed_displayed = transcript.iter().any(|t| t == FEED_TEXT);
            let echoed = transcript.iter().any(|t| t.starts_with(ECHO_MARKER));
            match *id {
                // A gagged line must not reach the display.
                "gag" => assert!(
                    !fed_displayed,
                    "gag: the fed line must be gagged; transcript:\n{transcript:#?}"
                ),
                "read_echo" => assert!(
                    fed_displayed && echoed,
                    "read_echo: fed line + echo must both display; transcript:\n{transcript:#?}"
                ),
                _ => assert!(
                    fed_displayed,
                    "{id}: the fed line must reach the display; transcript:\n{transcript:#?}"
                ),
            }
        }
        session.drain_stragglers();
        sessions.push((id, session));
    }
    if sanity {
        eprintln!("  sanity: each verb's observable effect confirmed once during warmup");
    }

    let mut group = c.benchmark_group("trigger_verbs");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Elements(m));
    for (id, session) in &mut sessions {
        group.bench_function(*id, |b| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        session.drain_stragglers();
                        let start = Instant::now();
                        for _ in 0..m {
                            session.feed(&feed_line);
                        }
                        session.feed(&sync_line);
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

criterion_group!(benches, trigger_verbs);
criterion_main!(benches);
