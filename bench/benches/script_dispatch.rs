//! The cost of REAL JS trigger dispatch — V8 entry plus capture marshalling —
//! measured end-to-end through live spawned sessions. `trigger_engine.rs`
//! deliberately excludes this: its triggers carry `ScriptAction::Noop`, so it
//! prices the match but never a fire. Here each fed line traverses the whole
//! shipped path in `core/src/session/`: `RuntimeAction::HandleIncomingLine`
//! into the trigger cascade (`runtime/trigger.rs`), depth-first action
//! dispatch (`runtime/dispatch.rs`), `CallJavascriptFunction` into the deno
//! isolate (`runtime/script_engine.rs`) with the capture array marshalled into
//! the JS `matches` object, then the line's transform/route/display step and
//! the default-on screen-log append. Triggers are registered by a real module
//! through the `createTrigger` scripting API — nothing is mocked.
//!
//! Groups (one live session each; each server dir carries its own module):
//!   - `baseline`: only the `/^ZZSYNC$/` barrier trigger exists and the fed
//!     lines match nothing — the per-line session overhead with zero JS fires.
//!   - `fire0`/`fire5`/`fire20`: adds one trigger with 0/5/20 capture groups
//!     and an EMPTY JS body that fires on every fed line. The
//!     `fireN − baseline` delta is the per-fire JS tax (V8 entry + call), and
//!     the `fire5`→`fire20` spread is the capture-marshalling slope. This tax
//!     matters because a busy profile fires triggers on a large share of
//!     incoming lines, so it bounds line throughput under automation.
//!
//! One timed pass feeds M lines plus one final ZZSYNC barrier line, then
//! drains the session event stream until the barrier trigger's ZZDONE echo
//! appears. Runtime actions dispatch in order with depth-first expansion
//! (`core/tests/command_ordering.rs`), so the echo proves every prior line's
//! full cascade completed. `Throughput::Elements(M)` counts the fed lines;
//! the barrier line is ~1/M untimed overhead. Every group's fed line is
//! padded to the same byte length, so byte-proportional per-line work (the
//! `PatternSet` scan, the screen-log append) is identical across groups and
//! cancels exactly out of the deltas.
//!
//! Drain discipline: the UI event channel is bounded (1024 in
//! `core/src/session.rs`) and the runtime AWAITS it when full, so the harness
//! drains continuously while waiting for the barrier and sweeps stragglers
//! between passes — backlog never bleeds across samples and the runtime never
//! stalls against an undrained stream.
//!
//! Env vars: `SMUDGY_BENCH_LINES=n` lowers M (default 500; the env value wins
//! only when smaller, mirroring the corpus loaders' truncate-only contract);
//! `SMUDGY_BENCH_SKIP_SANITY=1` skips the warmup assertions that prove the
//! fire trigger actually fires against the fed line (a `fireLimit: 1` twin of
//! the measured trigger echoes ZZFIRED exactly once during warmup, then
//! removes itself so measured passes pay only the empty body).

use std::{
    env,
    fmt::Write as _,
    fs,
    hint::black_box,
    path::Path,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use futures::{FutureExt, Stream, StreamExt};
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, TaggedSessionEvent,
    runtime::RuntimeAction, spawn, styled_line::StyledLine,
};

/// Lines fed per timed pass before the barrier. `SMUDGY_BENCH_LINES` only
/// lowers it: at this size a pass stays in the tens of milliseconds, which
/// `SamplingMode::Flat` handles comfortably.
const DEFAULT_LINES_PER_ITER: u64 = 500;

/// Warmup lines fed (per session) before anything is timed: enough to load
/// the module, exhaust the one-shot sanity twin, and warm the JS call path.
const WARMUP_LINES: usize = 3;

/// Generous ceilings so a wedged session panics instead of hanging criterion.
const READY_TIMEOUT: Duration = Duration::from_mins(2);
const DRAIN_TIMEOUT: Duration = Duration::from_mins(1);

/// Echoed by the barrier trigger; observing it proves every prior line's
/// cascade completed (actions dispatch in order, depth-first).
const DONE_MARKER: &str = "ZZDONE";
/// Echoed exactly once (warmup) by the `fireLimit: 1` sanity twin.
const FIRED_MARKER: &str = "ZZFIRED";

/// One benched session. `captures: None` is the no-fire baseline; `Some(n)`
/// registers the measured empty-body trigger with `n` capture groups.
struct GroupSpec {
    id: &'static str,
    server: &'static str,
    session_id: u32,
    captures: Option<usize>,
}

const GROUPS: &[GroupSpec] = &[
    GroupSpec {
        id: "baseline",
        server: "ZZBenchBaseline",
        session_id: 9101,
        captures: None,
    },
    GroupSpec {
        id: "fire0",
        server: "ZZBenchFire0",
        session_id: 9102,
        captures: Some(0),
    },
    GroupSpec {
        id: "fire5",
        server: "ZZBenchFire5",
        session_id: 9103,
        captures: Some(5),
    },
    GroupSpec {
        id: "fire20",
        server: "ZZBenchFire20",
        session_id: 9104,
        captures: Some(20),
    },
];

/// M for this run: the default, lowered by `SMUDGY_BENCH_LINES` when set and
/// smaller (the same truncate-only semantics as the shared corpus loaders).
fn lines_per_iter() -> u64 {
    match env::var("SMUDGY_BENCH_LINES") {
        Ok(v) => {
            DEFAULT_LINES_PER_ITER.min(v.parse().expect("SMUDGY_BENCH_LINES must be a number"))
        }
        Err(_) => DEFAULT_LINES_PER_ITER,
    }
}

/// The per-session module, registered through the real `createTrigger`
/// scripting API. Every module carries the ZZSYNC barrier trigger; fire
/// groups add the measured empty-body trigger plus its one-shot sanity twin
/// (same pattern, `fireLimit: 1`), which proves during warmup that the fed
/// line really fires the pattern and then removes itself.
fn module_source(captures: Option<usize>) -> String {
    let mut source = String::from(
        "import { echo, createTrigger } from \"smudgy:core\";\n\
         createTrigger(/^ZZSYNC$/, () => { echo(\"ZZDONE\"); }, { name: \"sync\" });\n",
    );
    if let Some(n) = captures {
        let groups = r" (\w+)".repeat(n);
        // The non-capturing ` ZPAD\w+$` tail absorbs the padding that
        // equalizes fed-line byte lengths across groups (see `feed_text`).
        write!(
            source,
            "createTrigger(/^ZZFIRE{groups} ZPAD\\w+$/, () => {{}}, {{ name: \"fire\" }});\n\
             createTrigger(/^ZZFIRE{groups} ZPAD\\w+$/, () => {{ echo(\"ZZFIRED\"); }}, {{ name: \"fire_sanity\", fireLimit: 1 }});\n"
        )
        .expect("write to String");
    }
    source
}

/// Every group's fed line is padded to this byte length, so per-line work
/// that scales with line bytes is identical across groups. Must exceed the
/// longest natural prefix (fire20 at 76 bytes) plus the ` ZPAD` token.
const FEED_LINE_LEN: usize = 96;

/// The line fed M times per pass: for fire groups it matches the ZZFIRE
/// pattern with one word per capture group; for the baseline it matches
/// nothing the module registered. All groups pad to [`FEED_LINE_LEN`] bytes
/// (the fire patterns absorb the pad via their non-capturing ` ZPAD\w+$`
/// tail) so the `fireN − baseline` deltas isolate the JS fire alone.
fn feed_text(captures: Option<usize>) -> String {
    let mut text = match captures {
        None => String::from("the fountain bubbles quietly in the empty plaza"),
        Some(n) => {
            let mut t = String::from("ZZFIRE");
            for i in 0..n {
                write!(t, " w{i}").expect("write to String");
            }
            t
        }
    };
    write!(text, " ZPAD").expect("write to String");
    assert!(text.len() < FEED_LINE_LEN, "FEED_LINE_LEN too small");
    while text.len() < FEED_LINE_LEN {
        text.push('x');
    }
    text
}

/// A live session plus everything a timed pass needs, built once in setup.
/// Sessions are intentionally never shut down: they idle between groups and
/// the process exit reaps their threads, which sidesteps any engine-teardown
/// race after the numbers are already in.
struct BenchSession {
    events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>>,
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeAction>,
    feed_line: Arc<StyledLine>,
    sync_line: Arc<StyledLine>,
}

impl BenchSession {
    /// Writes the server dir (module + logs), spawns the session, and blocks
    /// until the runtime hands back its action sender.
    fn start(rt: &tokio::runtime::Runtime, home: &Path, spec: &GroupSpec) -> Self {
        let modules_dir = home.join(spec.server).join("modules");
        fs::create_dir_all(&modules_dir).expect("create modules dir");
        fs::create_dir_all(home.join(spec.server).join("logs")).expect("create logs dir");
        fs::write(modules_dir.join("bench.ts"), module_source(spec.captures))
            .expect("write bench module");

        let params = Arc::new(SessionParams {
            session_id: SessionId::from(spec.session_id),
            server_name: Arc::new(spec.server.to_string()),
            profile_name: Arc::new("Bench".to_string()),
            profile_subtext: Arc::new(String::new()),
            mapper: None,
            package_client: None,
            extra_script_extensions: Arc::new(Vec::new),
            on_engine_rebuild: None,
        });

        let mut events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>> = Box::pin(spawn(params));

        let tx = rt.block_on(async {
            loop {
                let event = tokio::time::timeout(READY_TIMEOUT, events.next())
                    .await
                    .expect("timed out waiting for RuntimeReady")
                    .expect("session event stream ended before RuntimeReady");
                if let SessionEvent::RuntimeReady(tx) = event.event {
                    break tx;
                }
            }
        });

        Self {
            events,
            tx,
            feed_line: Arc::new(StyledLine::new(&feed_text(spec.captures), Vec::new())),
            sync_line: Arc::new(StyledLine::new("ZZSYNC", Vec::new())),
        }
    }

    fn feed(&self, line: &Arc<StyledLine>) {
        self.tx
            .send(RuntimeAction::HandleIncomingLine(line.clone()))
            .expect("session runtime channel closed");
    }

    /// Feeds warmup lines + the barrier and, unless skipped, asserts the fire
    /// trigger really fired: the one-shot sanity twin must echo ZZFIRED
    /// exactly once (zero for the baseline), and the fed line must reach the
    /// display path. Runs even with sanity skipped so every session enters
    /// measurement in the same state (module warm, twin exhausted).
    async fn warm_up(&mut self, spec: &GroupSpec, check: bool) {
        let mut texts = Vec::new();
        for _ in 0..WARMUP_LINES {
            self.feed(&self.feed_line);
        }
        self.feed(&self.sync_line);
        let completed = self.drain_collect(&mut texts).await;
        assert!(
            completed,
            "warmup barrier never arrived (module failed to load?); transcript:\n{}",
            texts.join("\n")
        );
        if !check {
            return;
        }
        let fired = texts.iter().filter(|t| t.as_str() == FIRED_MARKER).count();
        if spec.captures.is_some() {
            assert_eq!(
                fired,
                1,
                "the fire trigger must fire against the fed line (once, via its \
                 fireLimit:1 sanity twin); transcript:\n{}",
                texts.join("\n")
            );
        } else {
            assert_eq!(
                fired,
                0,
                "the baseline must not fire anything; transcript:\n{}",
                texts.join("\n")
            );
        }
        assert!(
            texts.iter().any(|t| *t == self.feed_line.text),
            "fed lines must reach the display path; transcript:\n{}",
            texts.join("\n")
        );
    }

    /// Drains until the ZZDONE barrier echo, collecting every displayed
    /// line's text. Returns `false` on timeout so the caller owns the panic
    /// (and can put the collected transcript in the message).
    async fn drain_collect(&mut self, texts: &mut Vec<String>) -> bool {
        let events = &mut self.events;
        tokio::time::timeout(DRAIN_TIMEOUT, async {
            loop {
                let event = events.next().await.expect("session event stream ended");
                let mut done = false;
                if let SessionEvent::UpdateBuffer(updates) = &event.event {
                    for update in updates.as_slice() {
                        if let BufferUpdate::Append(line) = update {
                            done |= line.text == DONE_MARKER;
                            texts.push(line.text.clone());
                        }
                    }
                }
                if done {
                    break;
                }
            }
        })
        .await
        .is_ok()
    }

    /// The timed drain: consume events until the barrier echo. A single
    /// timeout spans the whole drain so the timed window pays one timer
    /// registration, not one per event.
    async fn drain_until_done(&mut self) {
        let events = &mut self.events;
        tokio::time::timeout(DRAIN_TIMEOUT, async {
            loop {
                let event = events.next().await.expect("session event stream ended");
                let mut done = false;
                if let SessionEvent::UpdateBuffer(updates) = &event.event {
                    for update in updates.as_slice() {
                        if let BufferUpdate::Append(line) = update {
                            done |= line.text == DONE_MARKER;
                        }
                    }
                }
                black_box(&event);
                if done {
                    break;
                }
            }
        })
        .await
        .expect("timed out draining to the ZZDONE barrier");
    }

    /// One timed pass: feed M group lines + the barrier, drain to the barrier
    /// echo. Lines were built in setup, so each send costs one `Arc` bump.
    async fn timed_pass(&mut self, m: u64) -> Duration {
        let start = Instant::now();
        for _ in 0..m {
            self.feed(&self.feed_line);
        }
        self.feed(&self.sync_line);
        self.drain_until_done().await;
        start.elapsed()
    }

    /// Non-blocking sweep of anything still queued (the barrier line's own
    /// display can trail its ZZDONE echo), so backlog never bleeds into the
    /// next timed pass.
    fn drain_stragglers(&mut self) {
        loop {
            match self.events.next().now_or_never() {
                Some(Some(event)) => {
                    black_box(event);
                }
                Some(None) => panic!("session event stream ended"),
                None => break,
            }
        }
    }
}

fn script_dispatch(c: &mut Criterion) {
    let m = lines_per_iter();
    let sanity = env::var("SMUDGY_BENCH_SKIP_SANITY").is_err();
    eprintln!(
        "script_dispatch: {m} lines/pass + 1 barrier line; sanity checks {}",
        if sanity { "on" } else { "off" }
    );

    // Hermetic smudgy home for the whole process (first `set_smudgy_home`
    // wins). The session threads keep log files open under it for the process
    // lifetime, so the tempdir is leaked rather than racing its cleanup
    // against them — the same pattern as core's session integration tests.
    let home = tempfile::tempdir().expect("create temp smudgy home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let mut sessions: Vec<(&'static str, BenchSession)> = Vec::new();
    for spec in GROUPS {
        let mut session = BenchSession::start(&rt, &home_path, spec);
        rt.block_on(session.warm_up(spec, sanity));
        eprintln!(
            "  {}: session ready ({} capture groups), warmup + fire sanity done",
            spec.id,
            spec.captures
                .map_or_else(|| String::from("no fire trigger, 0"), |n| n.to_string())
        );
        sessions.push((spec.id, session));
    }

    let mut group = c.benchmark_group("script_dispatch");
    group.sample_size(10);
    // Flat sampling: criterion's recommended mode for benches that run many ms
    // per iteration (each pass pushes M lines through a live V8 runtime).
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Elements(m));
    for (id, session) in &mut sessions {
        group.bench_function(*id, |b| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        session.drain_stragglers();
                        total += session.timed_pass(m).await;
                    }
                    total
                })
            });
        });
    }
    group.finish();
}

criterion_group!(benches, script_dispatch);
criterion_main!(benches);
