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
//!   - `engine_build/{dirty_rebuild,dirty_rebuild_styled}/<n>`: the
//!     `PatternSet`-rebuild stall for a plain versus styled insertion. Any
//!     trigger add/remove flips trigger.rs's dirty flag, and the NEXT
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
use smudgy_core::models::matchers::{
    MatcherColor, MatcherColorMatch, MatcherHsv, MatcherHsvRange, MatcherRole, MatcherSyntax,
    TriggerMatcherSource,
};
use smudgy_core::session::{
    connection::vt_processor::AnsiColor,
    runtime::{
        BenchActionQueue, IsolateId, Manager, Origin, PushTriggerParams, ScriptAction,
        SharedAutomationRegistry,
    },
    styled_line::{Color, Style, StyledLine, VtSpan},
};

/// Feature-gated trigger action observation handle.
type Queue = BenchActionQueue;

/// Pushes one enabled single-pattern trigger carrying `action`. The corpus
/// triggers all carry `ScriptAction::Noop` (no JS engine exists here). A hit
/// still enqueues `RunAutomation`; the benchmark clears those actions without
/// dispatching them. The `engine_build` sanity probe passes `SendRaw` so its
/// payload is distinct from the corpus triggers as well as observable.
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
        matchers: None,
        action,
        prompt: false,
        enabled: true,
        priority: 0,
        fallthrough: false,
        fire_limit: None,
        line_limit: None,
        source: None,
        outer: None,
        reach: smudgy_core::models::triggers::InnerReach::DEFAULT,
    })
    .expect("push_trigger");
}

fn push_colored_trigger(
    manager: &mut Manager,
    name: String,
    pattern: String,
    color: MatcherColor,
    fallthrough: bool,
) {
    let name = Arc::new(name);
    let patterns = Arc::new(vec![pattern.clone()]);
    let empty = Arc::new(Vec::<String>::new());
    let matchers = [TriggerMatcherSource {
        role: MatcherRole::Match,
        syntax: MatcherSyntax::Regex,
        source: pattern,
        anchor_start: true,
        anchor_end: true,
        color: Some(MatcherColorMatch {
            foreground: Some(color),
            ..Default::default()
        }),
    }];
    manager
        .push_trigger(PushTriggerParams {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: &name,
            patterns: &patterns,
            raw_patterns: &empty,
            anti_patterns: &empty,
            matchers: Some(&matchers),
            action: ScriptAction::Noop,
            prompt: false,
            enabled: true,
            priority: 0,
            fallthrough,
            fire_limit: None,
            line_limit: None,
            source: None,
            outer: None,
            reach: smudgy_core::models::triggers::InnerReach::DEFAULT,
        })
        .expect("colored trigger");
}

fn build_colored_manager(pattern: &str, color: MatcherColor) -> (Manager, Queue) {
    let registry = SharedAutomationRegistry::default();
    let (mut manager, queue) = Manager::new_for_bench(Arc::new(";".to_string()), registry);
    push_colored_trigger(
        &mut manager,
        format!("colored_{pattern}"),
        pattern.to_string(),
        color,
        false,
    );
    (manager, queue)
}

fn build_colored_anti_manager(anti_sources: &[(String, MatcherColor)]) -> (Manager, Queue) {
    let registry = SharedAutomationRegistry::default();
    let (mut manager, queue) = Manager::new_for_bench(Arc::new(";".to_string()), registry);
    let name = Arc::new(format!("colored_anti_{}", anti_sources.len()));
    let patterns = Arc::new(vec!["target".to_string()]);
    let raw_patterns = Arc::new(Vec::<String>::new());
    let anti_patterns = Arc::new(
        anti_sources
            .iter()
            .map(|(source, _)| source.clone())
            .collect::<Vec<_>>(),
    );
    let mut matchers = Vec::with_capacity(anti_sources.len() + 1);
    matchers.push(TriggerMatcherSource {
        role: MatcherRole::Match,
        syntax: MatcherSyntax::Regex,
        source: "target".to_string(),
        anchor_start: true,
        anchor_end: true,
        color: None,
    });
    matchers.extend(
        anti_sources
            .iter()
            .map(|(source, color)| TriggerMatcherSource {
                role: MatcherRole::Anti,
                syntax: MatcherSyntax::Regex,
                source: source.clone(),
                anchor_start: true,
                anchor_end: true,
                color: Some(MatcherColorMatch {
                    foreground: Some(*color),
                    ..Default::default()
                }),
            }),
    );
    manager
        .push_trigger(PushTriggerParams {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: &name,
            patterns: &patterns,
            raw_patterns: &raw_patterns,
            anti_patterns: &anti_patterns,
            matchers: Some(&matchers),
            action: ScriptAction::Noop,
            prompt: false,
            enabled: true,
            priority: 0,
            fallthrough: false,
            fire_limit: None,
            line_limit: None,
            source: None,
            outer: None,
            reach: smudgy_core::models::triggers::InnerReach::DEFAULT,
        })
        .expect("colored anti trigger");
    (manager, queue)
}

fn build_plain_anti_manager(anti_source: &str) -> (Manager, Queue) {
    let registry = SharedAutomationRegistry::default();
    let (mut manager, queue) = Manager::new_for_bench(Arc::new(";".to_string()), registry);
    let name = Arc::new(String::from("plain_anti"));
    let patterns = Arc::new(vec![String::from("target")]);
    let raw_patterns = Arc::new(Vec::<String>::new());
    let anti_patterns = Arc::new(vec![anti_source.to_string()]);
    manager
        .push_trigger(PushTriggerParams {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: &name,
            patterns: &patterns,
            raw_patterns: &raw_patterns,
            anti_patterns: &anti_patterns,
            matchers: None,
            action: ScriptAction::Noop,
            prompt: false,
            enabled: true,
            priority: 0,
            fallthrough: false,
            fire_limit: None,
            line_limit: None,
            source: None,
            outer: None,
            reach: smudgy_core::models::triggers::InnerReach::DEFAULT,
        })
        .expect("plain anti trigger");
    (manager, queue)
}

/// Builds `count` independent styled candidates that all text-match `target`
/// and all reject a default-colored line. Keeping downstream work identical
/// makes this a candidate-qualification scaling measurement rather than an
/// action-dispatch comparison.
fn build_colored_candidate_population(count: usize) -> (Manager, Queue) {
    let registry = SharedAutomationRegistry::default();
    let (mut manager, queue) = Manager::new_for_bench(Arc::new(";".to_string()), registry);
    for index in 0..count {
        // A unique optional capture keeps every PatternSet row distinct while
        // still matching the exact same input.
        let pattern = format!("target(?P<c{index}>x?)");
        push_colored_trigger(
            &mut manager,
            format!("colored_candidate_{index}"),
            pattern,
            MatcherColor::Ansi { index: 1 },
            true,
        );
    }
    (manager, queue)
}

/// Shared red HSV range plus one RGB value on either side of the predicate.
/// Keeping the exact same predicate and miss color across all HSV cells makes
/// their occurrence/span slopes directly comparable.
fn hsv_benchmark_colors() -> (MatcherColor, Color, Color) {
    let range = MatcherHsvRange {
        first: MatcherHsv {
            hue: 0,
            saturation: 160,
            value: 120,
        },
        second: MatcherHsv {
            hue: 20,
            saturation: 255,
            value: 255,
        },
        wrap_hue: false,
    };
    let (outside_r, outside_g, outside_b) = MatcherHsv {
        hue: 200,
        saturation: 220,
        value: 200,
    }
    .to_rgb();
    let (inside_r, inside_g, inside_b) = MatcherHsv {
        hue: 10,
        saturation: 220,
        value: 200,
    }
    .to_rgb();
    (
        MatcherColor::Truecolor {
            r: inside_r,
            g: inside_g,
            b: inside_b,
            range: Some(range),
        },
        Color::Rgb {
            r: inside_r,
            g: inside_g,
            b: inside_b,
        },
        Color::Rgb {
            r: outside_r,
            g: outside_g,
            b: outside_b,
        },
    )
}

const COLOR_SCALING_TEXT_BYTES: usize = 8 * 1024;

/// Partition fixed text into `span_count` real, nonempty spans. Backgrounds
/// differ so `StyledLine` cannot merge adjacent spans; `foreground` selects
/// whether the caller wants an exact-color or RGB/HSV qualification miss.
fn partitioned_line(text: &str, span_count: usize, foreground: Color) -> Arc<StyledLine> {
    assert!(span_count > 0);
    assert!(text.len() >= span_count);
    let mut spans = Vec::with_capacity(span_count);
    for index in 0..span_count {
        let begin_pos = text.len() * index / span_count;
        let end_pos = text.len() * (index + 1) / span_count;
        let shade = u8::try_from(index % 251).expect("bounded shade");
        spans.push(VtSpan {
            style: Style {
                fg: foreground,
                bg: Color::Rgb {
                    r: shade,
                    g: 255 - shade,
                    b: shade / 2,
                },
                ..Style::default()
            },
            begin_pos,
            end_pos,
        });
    }
    Arc::new(StyledLine::new(text, spans))
}

/// Produces a fixed-size line with independently selectable regex-occurrence
/// and span counts. Every occurrence fails the predicate, so each cell walks
/// every occurrence/span it needs without capture or action work. Keeping the
/// subject and match region byte-identical across span counts prevents regex
/// scan length from masquerading as span cost.
fn occurrence_span_miss_line(
    occurrences: usize,
    span_count: usize,
    foreground: Color,
) -> Arc<StyledLine> {
    assert!(occurrences > 0);
    let mut matches = String::with_capacity(occurrences * "target ".len());
    for index in 0..occurrences {
        if index > 0 {
            matches.push(' ');
        }
        matches.push_str("target");
    }
    assert!(matches.len() <= COLOR_SCALING_TEXT_BYTES);
    let text = "x".repeat(COLOR_SCALING_TEXT_BYTES - matches.len()) + matches.as_str();
    partitioned_line(&text, span_count, foreground)
}

/// Process one line, observe how many automations were queued, and restore the
/// queue to the empty state expected by timed iterations.
fn process_and_count(manager: &mut Manager, queue: &Queue, line: &Arc<StyledLine>) -> usize {
    manager
        .process_incoming_line(line)
        .expect("benchmark probe");
    let count = queue.len();
    queue.clear();
    count
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

#[allow(clippy::too_many_lines)]
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
    // lazily — any push/remove marks the set dirty, and the next
    // `process_incoming_line` pays `rebuild_trigger_regex_set` (all four
    // tiered `PatternSet`s) before matching its line. Per iteration: push one
    // throwaway trigger (marks dirty), process one non-matching line (exactly
    // one full-set rebuild), remove the throwaway (leaves the set dirty for
    // the next iteration's push). The probe line's own scan is µs-scale
    // against the rebuild, so the measured time IS the stall.
    let probe = Arc::new(StyledLine::new("zzqx rebuild stall probe zzqx", Vec::new()));
    let styled_probe = Arc::new(StyledLine::new(
        "zzqx styled rebuild sanity zzqx",
        vec![VtSpan {
            style: Style {
                fg: Color::Ansi {
                    color: AnsiColor::Red,
                    bold: false,
                },
                ..Style::default()
            },
            begin_pos: 0,
            end_pos: "zzqx styled rebuild sanity zzqx".len(),
        }],
    ));
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
            push_colored_trigger(
                &mut mgr,
                "zz_styled_sanity".to_owned(),
                regex::escape("zzqx styled rebuild sanity zzqx"),
                MatcherColor::Ansi { index: 1 },
                false,
            );
            assert_eq!(
                process_and_count(&mut mgr, &queue, &styled_probe),
                1,
                "a styled insertion must participate in the same next-line rebuild"
            );
            mgr.remove_trigger(&IsolateId::Main, &Origin::User, "zz_styled_sanity");
            assert_eq!(
                process_and_count(&mut mgr, &queue, &styled_probe),
                0,
                "the styled sanity trigger must be absent after its removal rebuild"
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
        // Identical mutation/rebuild shape, except the inserted row carries a
        // persisted matcher sidecar. One incoming line still performs exactly
        // one aggregate PatternSet rebuild; style metadata must not cause a
        // second build.
        group.bench_function(BenchmarkId::new("dirty_rebuild_styled", size), |b| {
            b.iter(|| {
                push_colored_trigger(
                    &mut mgr,
                    "zz_styled_throwaway".to_owned(),
                    regex::escape("zzqx styled throwaway trigger zzqx"),
                    MatcherColor::Ansi { index: 1 },
                    false,
                );
                mgr.process_incoming_line(&probe).expect("styled rebuild");
                mgr.remove_trigger(&IsolateId::Main, &Origin::User, "zz_styled_throwaway");
                black_box(&mgr);
            });
        });
    }
    group.finish();
}

/// Measures color-filter cost when 99 text matches fail the style filter before
/// the first qualifying match. The adjacent unfiltered benchmark reveals
/// performance changes in the shared hot path.
#[allow(clippy::too_many_lines)]
fn color_filter(c: &mut Criterion) {
    let mut text = "target ".repeat(99);
    let colored_start = text.len();
    text.push_str("target");
    let line = Arc::new(StyledLine::new(
        &text,
        vec![
            VtSpan {
                style: Style::default(),
                begin_pos: 0,
                end_pos: colored_start,
            },
            VtSpan {
                style: Style {
                    fg: Color::Ansi {
                        color: AnsiColor::Red,
                        bold: false,
                    },
                    ..Style::default()
                },
                begin_pos: colored_start,
                end_pos: text.len(),
            },
        ],
    ));

    let registry = SharedAutomationRegistry::default();
    let (mut plain, plain_queue) = Manager::new_for_bench(Arc::new(";".to_string()), registry);
    push_one_trigger(
        &mut plain,
        "plain".to_string(),
        "target".to_string(),
        ScriptAction::Noop,
    );

    let (mut colored, colored_queue) =
        build_colored_manager("target", MatcherColor::Ansi { index: 1 });

    let (hsv_matcher, hsv_inside, hsv_outside) = hsv_benchmark_colors();
    let hsv_line = Arc::new(StyledLine::new(
        &text,
        vec![
            VtSpan {
                style: Style {
                    fg: hsv_outside,
                    ..Style::default()
                },
                begin_pos: 0,
                end_pos: colored_start,
            },
            VtSpan {
                style: Style {
                    fg: hsv_inside,
                    ..Style::default()
                },
                begin_pos: colored_start,
                end_pos: text.len(),
            },
        ],
    ));
    let (mut hsv, hsv_queue) = build_colored_manager("target", hsv_matcher);
    let (mut color_only, color_only_queue) =
        build_colored_manager("", MatcherColor::Ansi { index: 1 });
    let (mut capture_rich, capture_rich_queue) =
        build_colored_manager("(t)(a)(r)(g)(e)(t)", MatcherColor::Ansi { index: 1 });
    let exact_first_line = Arc::new(StyledLine::new(
        "target",
        vec![VtSpan {
            style: Style {
                fg: Color::Ansi {
                    color: AnsiColor::Red,
                    bold: false,
                },
                ..Style::default()
            },
            begin_pos: 0,
            end_pos: "target".len(),
        }],
    ));
    let hsv_first_line = Arc::new(StyledLine::new(
        "target",
        vec![VtSpan {
            style: Style {
                fg: hsv_inside,
                ..Style::default()
            },
            begin_pos: 0,
            end_pos: "target".len(),
        }],
    ));
    let exact_color_miss = Arc::new(StyledLine::new(
        "target",
        vec![VtSpan {
            style: Style::default(),
            begin_pos: 0,
            end_pos: "target".len(),
        }],
    ));
    let hsv_color_miss = Arc::new(StyledLine::new(
        "target",
        vec![VtSpan {
            style: Style {
                fg: hsv_outside,
                ..Style::default()
            },
            begin_pos: 0,
            end_pos: "target".len(),
        }],
    ));
    let text_miss = Arc::new(StyledLine::new(
        "clear",
        vec![VtSpan {
            style: Style::default(),
            begin_pos: 0,
            end_pos: "clear".len(),
        }],
    ));

    plain.process_incoming_line(&line).expect("plain warmup");
    colored
        .process_incoming_line(&line)
        .expect("colored warmup");
    hsv.process_incoming_line(&hsv_line).expect("HSV warmup");
    color_only
        .process_incoming_line(&line)
        .expect("color-only warmup");
    capture_rich
        .process_incoming_line(&line)
        .expect("capture-rich warmup");
    plain_queue.clear();
    colored_queue.clear();
    hsv_queue.clear();
    color_only_queue.clear();
    capture_rich_queue.clear();

    if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
        assert_eq!(
            process_and_count(&mut plain, &plain_queue, &text_miss),
            0,
            "the unfiltered text-miss control must not fire"
        );
        assert_eq!(
            process_and_count(&mut colored, &colored_queue, &exact_color_miss),
            0,
            "an exact-color miss must reject its text candidate"
        );
        assert_eq!(
            process_and_count(&mut hsv, &hsv_queue, &hsv_color_miss),
            0,
            "an HSV-range miss must reject its text candidate"
        );
        assert_eq!(
            process_and_count(&mut colored, &colored_queue, &exact_first_line),
            1,
            "the exact-color hit cell must fire exactly once"
        );
        assert_eq!(
            process_and_count(&mut hsv, &hsv_queue, &hsv_first_line),
            1,
            "the HSV-range hit cell must fire exactly once"
        );
    }

    let mut group = c.benchmark_group("engine_color_filter");
    // Every iteration processes exactly one line. Occurrence counts belong in
    // the benchmark id; reporting 100 elements for first-hit/color-only cells
    // materially overstated their throughput.
    group.throughput(Throughput::Elements(1));
    group.bench_function("unfiltered_text_miss", |b| {
        b.iter(|| {
            plain.process_incoming_line(black_box(&text_miss)).unwrap();
            plain_queue.clear();
        });
    });
    group.bench_function("filtered_text_miss", |b| {
        b.iter(|| {
            colored
                .process_incoming_line(black_box(&text_miss))
                .unwrap();
            colored_queue.clear();
        });
    });
    group.bench_function("unfiltered_first_match", |b| {
        b.iter(|| {
            plain.process_incoming_line(black_box(&line)).unwrap();
            plain_queue.clear();
        });
    });
    group.bench_function("unfiltered_one_occurrence", |b| {
        b.iter(|| {
            plain
                .process_incoming_line(black_box(&exact_first_line))
                .unwrap();
            plain_queue.clear();
        });
    });
    group.bench_function("exact_one_occurrence", |b| {
        b.iter(|| {
            colored
                .process_incoming_line(black_box(&exact_first_line))
                .unwrap();
            colored_queue.clear();
        });
    });
    group.bench_function("exact_one_occurrence_color_miss", |b| {
        b.iter(|| {
            colored
                .process_incoming_line(black_box(&exact_color_miss))
                .unwrap();
            colored_queue.clear();
        });
    });
    group.bench_function("hsv_one_occurrence", |b| {
        b.iter(|| {
            hsv.process_incoming_line(black_box(&hsv_first_line))
                .unwrap();
            hsv_queue.clear();
        });
    });
    group.bench_function("hsv_one_occurrence_color_miss", |b| {
        b.iter(|| {
            hsv.process_incoming_line(black_box(&hsv_color_miss))
                .unwrap();
            hsv_queue.clear();
        });
    });
    group.bench_function("filtered_100_matches", |b| {
        b.iter(|| {
            colored.process_incoming_line(black_box(&line)).unwrap();
            colored_queue.clear();
        });
    });
    group.bench_function("hsv_range_100_matches_two_spans", |b| {
        b.iter(|| {
            hsv.process_incoming_line(black_box(&hsv_line)).unwrap();
            hsv_queue.clear();
        });
    });
    group.bench_function("color_only_empty_pattern_two_spans", |b| {
        b.iter(|| {
            color_only.process_incoming_line(black_box(&line)).unwrap();
            color_only_queue.clear();
        });
    });
    group.bench_function("capture_rich_one_occurrence", |b| {
        b.iter(|| {
            capture_rich
                .process_incoming_line(black_box(&exact_first_line))
                .unwrap();
            capture_rich_queue.clear();
        });
    });
    group.bench_function("capture_rich_100_occurrences_final_hit", |b| {
        b.iter(|| {
            capture_rich
                .process_incoming_line(black_box(&line))
                .unwrap();
            capture_rich_queue.clear();
        });
    });
    group.finish();
}

/// Crosses regex-occurrence count, span count, and fixed subject length as
/// independent axes. This distinguishes the intended additive
/// occurrence/span walk from an accidental multiplicative scan. It also
/// measures color-only span scaling and candidate-count scaling on misses,
/// where every case performs identical downstream work (none).
fn color_filter_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_color_filter_scaling");
    group.throughput(Throughput::Elements(1));
    let (hsv_matcher, _hsv_inside, hsv_outside) = hsv_benchmark_colors();
    for (mode, matcher, miss_foreground) in [
        (
            "exact",
            MatcherColor::Ansi { index: 1 },
            Color::DefaultForeground { bold: false },
        ),
        ("hsv", hsv_matcher, hsv_outside),
    ] {
        for occurrences in [1_usize, 10, 100, 1_000] {
            for spans in [1_usize, 32, 100] {
                let line = occurrence_span_miss_line(occurrences, spans, miss_foreground);
                assert_eq!(line.text.len(), COLOR_SCALING_TEXT_BYTES);
                assert_eq!(line.spans.len(), spans);
                let (mut manager, queue) = build_colored_manager("target", matcher);
                manager.process_incoming_line(&line).expect("scale warmup");
                if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
                    assert!(
                        queue.is_empty(),
                        "{mode} occurrence/span miss cell unexpectedly fired"
                    );
                }
                queue.clear();
                group.bench_function(
                    BenchmarkId::new(mode, format!("occurrences_{occurrences}_spans_{spans}")),
                    |b| {
                        b.iter(|| {
                            manager.process_incoming_line(black_box(&line)).unwrap();
                            queue.clear();
                        });
                    },
                );
            }
        }
    }

    // Empty-source styled patterns bypass regex boundary iteration and scan
    // the actual span vector. Every cell is an all-span miss; crossing bytes
    // and spans independently exposes any accidental O(text length) behavior.
    for text_bytes in [64_usize, 8 * 1024, 64 * 1024] {
        let text = "x".repeat(text_bytes);
        for spans in [1_usize, 2, 32] {
            let line = partitioned_line(&text, spans, Color::DefaultForeground { bold: false });
            assert_eq!(line.text.len(), text_bytes);
            assert_eq!(line.spans.len(), spans);
            let (mut manager, queue) = build_colored_manager("", MatcherColor::Ansi { index: 1 });
            manager
                .process_incoming_line(&line)
                .expect("color-only scale warmup");
            if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
                assert!(queue.is_empty(), "color-only miss cell unexpectedly fired");
            }
            queue.clear();
            group.bench_function(
                BenchmarkId::new(
                    "color_only_miss",
                    format!("bytes_{text_bytes}_spans_{spans}"),
                ),
                |b| {
                    b.iter(|| {
                        manager.process_incoming_line(black_box(&line)).unwrap();
                        queue.clear();
                    });
                },
            );
        }
    }

    let default_line = Arc::new(StyledLine::new(
        "target",
        vec![VtSpan {
            style: Style::default(),
            begin_pos: 0,
            end_pos: "target".len(),
        }],
    ));
    for candidates in [1_usize, 100, 1_000] {
        let (mut manager, queue) = build_colored_candidate_population(candidates);
        manager
            .process_incoming_line(&default_line)
            .expect("candidate warmup");
        if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
            assert!(
                queue.is_empty(),
                "all candidate-population styles must miss"
            );
        }
        queue.clear();
        group.bench_function(BenchmarkId::new("color_miss_candidates", candidates), |b| {
            b.iter(|| {
                manager
                    .process_incoming_line(black_box(&default_line))
                    .unwrap();
                queue.clear();
            });
        });
    }
    group.finish();
}

/// Scans one representative log through the production-sized mixed trigger
/// population while varying how many never-matching rows carry style metadata.
/// This prices the plain-profile sentinel and aggregate-set shape separately
/// from the candidate-heavy focused cells above.
fn mixed_styled_population(c: &mut Criterion) {
    let names = load_item_names_10k();
    let (corpus_name, lines) = log_corpora()
        .into_iter()
        .next()
        .expect("bench/logs has at least one log file");
    assert!(!lines.is_empty(), "empty corpus (SMUDGY_BENCH_LINES=0?)");
    let styled: Vec<Arc<StyledLine>> = lines
        .iter()
        .map(|line| Arc::new(StyledLine::new(line, Vec::new())))
        .collect();
    let marker_prefix = "__SMUDGY_STYLED_POPULATION_NEVER_";
    assert!(
        lines.iter().all(|line| !line.contains(marker_prefix)),
        "the benchmark's never-match marker occurs in its corpus"
    );
    eprintln!(
        "engine_mixed_styled_population: {} literal + {} regex triggers; {} lines from {corpus_name}",
        names.len(),
        REGEX_TRIGGERS.len(),
        styled.len()
    );

    let mut group = c.benchmark_group("engine_mixed_styled_population");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Elements(styled.len() as u64));
    for styled_entries in [0_usize, 1, 100] {
        let (mut manager, queue) = build_manager(&names, REGEX_TRIGGERS);
        for index in 0..styled_entries {
            push_colored_trigger(
                &mut manager,
                format!("styled_population_{index}"),
                format!("^{marker_prefix}{index:03}__$"),
                MatcherColor::Ansi { index: 1 },
                true,
            );
        }
        manager
            .process_incoming_line(&styled[0])
            .expect("mixed population warmup");
        queue.clear();
        group.bench_function(BenchmarkId::new("styled_entries", styled_entries), |b| {
            b.iter(|| {
                for line in &styled {
                    manager.process_incoming_line(black_box(line)).unwrap();
                }
                queue.clear();
            });
        });
    }
    group.finish();
}

/// Measures colored anti-pattern sets of 1/8/64 in three deliberately distinct
/// shapes: no text hit (the allocation-free `is_match` prefilter), exactly one
/// matched index whose color misses, and all indices matched with only the last
/// color qualifying. The first two both let the positive trigger fire, keeping
/// downstream capture/action work identical while candidate count changes.
#[allow(clippy::too_many_lines)]
fn colored_anti_filter(c: &mut Criterion) {
    let prefilter_miss = Arc::new(StyledLine::new(
        "target clear",
        vec![VtSpan {
            style: Style::default(),
            begin_pos: 0,
            end_pos: "target clear".len(),
        }],
    ));
    let color_start = "target ".len();
    let last_qualifies = Arc::new(StyledLine::new(
        "target block",
        vec![
            VtSpan {
                style: Style::default(),
                begin_pos: 0,
                end_pos: color_start,
            },
            VtSpan {
                style: Style {
                    fg: Color::Ansi {
                        color: AnsiColor::Red,
                        bold: false,
                    },
                    ..Style::default()
                },
                begin_pos: color_start,
                end_pos: "target block".len(),
            },
        ],
    ));

    let mut group = c.benchmark_group("engine_colored_anti_filter");
    group.throughput(Throughput::Elements(1));
    for anti_count in [1_usize, 8, 64] {
        // Every regex is mutually exclusive; the input names only the final
        // token. `RegexSet::matches` therefore yields exactly one bit at every
        // set size, isolating bitset/set-width overhead from qualification.
        let one_match_rows: Vec<_> = (0..anti_count)
            .map(|index| {
                (
                    format!(r"\bblock_{index:03}\b"),
                    MatcherColor::Ansi { index: 1 },
                )
            })
            .collect();
        let one_match_text = format!("target block_{:03}", anti_count - 1);
        let one_match_color_miss = Arc::new(StyledLine::new(
            &one_match_text,
            vec![VtSpan {
                style: Style::default(),
                begin_pos: 0,
                end_pos: one_match_text.len(),
            }],
        ));
        let (mut one_match, one_match_queue) = build_colored_anti_manager(&one_match_rows);

        // All sources are text-distinct (unique capture names) but match the
        // same `block` occurrence. Blue predicates reject the red span; only
        // the final red predicate qualifies, forcing candidate iteration to
        // visit all 1/8/64 matched indices before vetoing.
        let all_match_rows: Vec<_> = (0..anti_count)
            .map(|index| {
                let color = if index + 1 == anti_count {
                    MatcherColor::Ansi { index: 1 }
                } else {
                    MatcherColor::Ansi { index: 4 }
                };
                (format!("block(?P<a{index}>x?)"), color)
            })
            .collect();
        let (mut all_match, all_match_queue) = build_colored_anti_manager(&all_match_rows);

        assert_eq!(
            process_and_count(&mut one_match, &one_match_queue, &prefilter_miss),
            1,
            "anti prefilter miss must leave the positive trigger runnable"
        );
        assert_eq!(
            process_and_count(&mut one_match, &one_match_queue, &one_match_color_miss,),
            1,
            "a matched anti whose color misses must leave the positive trigger runnable"
        );
        assert_eq!(
            process_and_count(&mut all_match, &all_match_queue, &last_qualifies),
            0,
            "the final qualifying anti candidate must veto the positive trigger"
        );

        group.bench_function(BenchmarkId::new("prefilter_miss", anti_count), |b| {
            b.iter(|| {
                one_match
                    .process_incoming_line(black_box(&prefilter_miss))
                    .unwrap();
                one_match_queue.clear();
            });
        });
        group.bench_function(
            BenchmarkId::new("one_matched_index_color_miss", anti_count),
            |b| {
                b.iter(|| {
                    one_match
                        .process_incoming_line(black_box(&one_match_color_miss))
                        .unwrap();
                    one_match_queue.clear();
                });
            },
        );
        group.bench_function(
            BenchmarkId::new("all_matched_last_qualifies", anti_count),
            |b| {
                b.iter(|| {
                    all_match
                        .process_incoming_line(black_box(&last_qualifies))
                        .unwrap();
                    all_match_queue.clear();
                });
            },
        );
    }

    // Outcome-matched comparator for the qualifying styled veto. Do not use
    // it as the comparator for color-miss candidate scaling: those cells fire
    // the positive trigger and intentionally pay capture/action work.
    let (mut plain_veto, plain_veto_queue) = build_plain_anti_manager("block");
    assert_eq!(
        process_and_count(&mut plain_veto, &plain_veto_queue, &last_qualifies),
        0,
        "the equivalent plain anti must veto"
    );
    group.bench_function("equivalent_plain_veto", |b| {
        b.iter(|| {
            plain_veto
                .process_incoming_line(black_box(&last_qualifies))
                .unwrap();
            plain_veto_queue.clear();
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    trigger_engine,
    color_filter,
    color_filter_scaling,
    mixed_styled_population,
    colored_anti_filter
);
criterion_main!(benches);
