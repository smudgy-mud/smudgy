//! Exact allocation counts per line on smudgy's hot paths, printed as a
//! table — the zero-noise complement to the wall-clock criterion benches.
//! Run with `cargo run -p smudgy_bench --example alloc_stats --release`.
//!
//! Wall-clock numbers on a desktop OS carry scheduler and cache noise;
//! allocation counts from a counting `#[global_allocator]` are exact and
//! reproducible run-to-run, which makes them the regression signal that can
//! later become CI-assertable ceilings (smudgy's hot-path performance
//! commitment explicitly covers allocation behavior). Each workload runs
//! twice after a warmup pass and the run warns when the two passes disagree —
//! a nondeterministic count can never be a ceiling.
//!
//! Workloads, each normalized per line of the shared log corpus
//! (`SMUDGY_BENCH_CORPUS=<path>`, else the first log in `bench/logs/`,
//! name-sorted, via the crate loaders):
//! - `ingest`: the full inbound socket pipeline — telnet preprocessor →
//!   `vtparse` → `VtProcessor` → `StyledLine` — composed exactly as the
//!   connect loop does (`feed_inbound` then `notify_end_of_buffer` per read;
//!   `core/src/session/connection.rs`), fed `AnsiLight`-dressed wire bytes in
//!   16 KiB read-sized chunks, draining the emitted `RuntimeAction`s.
//! - `styled_line`: `StyledLine::new_with_raw` per line
//!   (`core/src/session/styled_line.rs`) — line materialization alone.
//! - `terminal_buffer_whole` / `terminal_buffer_frag4`: the UI scrollback
//!   commit stream (`ui/src/terminal_buffer.rs`): `extend_line` +
//!   `commit_current_line` per whole line, and the same stream with every
//!   line delivered as four fragments glued through `StyledLine::append`
//!   (the partial-line shape prompts and mid-line read boundaries produce).
//! - `trigger_scan`: `Manager::process_incoming_line`
//!   (`core/src/session/runtime/trigger.rs` + `matcher.rs`) carrying ~10k
//!   literal substitution triggers plus the shared `REGEX_TRIGGERS` regex
//!   set — the same engine shape `benches/trigger_engine.rs` times (the run
//!   header prints the exact counts).
//! - `styled_normal_*` / `styled_anti_*`: focused styled-trigger qualification
//!   cells. Normal text/style misses queue nothing; a normal hit exposes the
//!   capture/action allocation boundary. For styled anti patterns, prefilter
//!   and color misses let the positive trigger fire, while a color hit vetoes
//!   it; that outcome naming makes the candidate-bitset allocation visible
//!   without pretending `Noop` actions disappear before dispatch.
//!
//! Env vars: `SMUDGY_BENCH_CORPUS=<path>` selects the corpus and
//! `SMUDGY_BENCH_LINES=n` truncates it (shared loaders);
//! `SMUDGY_BENCH_SKIP_SANITY=1` skips the sanity checks that pin each
//! workload to the real path (exact allocator accounting, line-count and
//! text round-trips through ingest and the scrollback glue, a trigger that
//! must fire).

use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use smudgy_bench::{REGEX_TRIGGERS, default_corpus, load_item_names_10k, wire};
use smudgy_core::models::matchers::{
    MatcherColor, MatcherColorMatch, MatcherRole, MatcherSyntax, TriggerMatcherSource,
};
use smudgy_core::session::{
    connection::{
        feed_inbound,
        responders::{DEFAULT_DIMS, ProtocolState},
        telnet::TelnetParser,
        transcode::Transcode,
        vt_processor::{AnsiColor, VtProcessor},
    },
    runtime::{
        BenchActionQueue, IsolateId, Manager, Origin, PushTriggerParams, RuntimeAction,
        ScriptAction, SharedAutomationRegistry,
    },
    styled_line::{Color, Style, StyledLine, VtSpan},
};
use smudgy_ui::terminal_buffer::TerminalBuffer;
use vtparse::VTParser;

/// Counts every heap allocation (`alloc`, `alloc_zeroed`, `realloc`) in the
/// process, delegating the actual work to [`System`]. Deallocations are
/// deliberately uncounted: the reported quantity is *cumulative allocation
/// traffic* — what a hot path must keep at zero — not live-heap size.
/// `realloc` counts its full new size, since growth traffic is what
/// regresses. Counting is always on; this is an example binary, so the
/// per-allocation overhead is irrelevant.
struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Cumulative (allocation count, allocated bytes) since process start.
fn snapshot() -> (u64, u64) {
    (
        ALLOCATIONS.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed),
    )
}

/// Runs `f` and returns the (allocations, bytes) it performed. The result is
/// passed through [`black_box`] so the work cannot be optimized away, and is
/// dropped only after the closing snapshot (deallocation is uncounted, so the
/// drop is invisible either way). Everything single-threaded, so the deltas
/// belong to `f` alone.
fn measure<R>(f: impl FnOnce() -> R) -> (u64, u64) {
    let (allocs_before, bytes_before) = snapshot();
    let result = f();
    let (allocs_after, bytes_after) = snapshot();
    black_box(result);
    (allocs_after - allocs_before, bytes_after - bytes_before)
}

/// Per-read chunk size fed to the ingest pipeline: the read granularity a
/// busy MUD connection sees. The connect loop reads up to 64 KiB per wakeup;
/// 16 KiB models a busy-but-not-saturated stream (and matches `wire::chunk`'s
/// documented example size).
const READ_CHUNK: usize = 16 * 1024;

/// `TerminalBuffer::new()`'s default scrollback limit, mirrored for the
/// sanity check's eviction math.
const DEFAULT_SCROLLBACK: usize = 10_000;

/// The style an undecorated fragment carries — the parser's initial SGR
/// state, as `VtProcessor::new` seeds it.
const DEFAULT_STYLE: Style = Style {
    fg: Color::DefaultForeground { bold: false },
    bg: Color::DefaultBackground,
    ..Style::DEFAULT
};

/// Feature-gated trigger action observation handle.
type Queue = BenchActionQueue;

/// Marker matched only by the literal-tier sanity probe; never occurs in a
/// log corpus, so the measured passes are unaffected by the probe triggers.
const PROBE_LITERAL: &str = "__ALLOC_STATS_PROBE_LITERAL__";
/// Pattern for the regex-tier sanity probe (the anchor and class keep it off
/// the literal tier); its marker never occurs in a log corpus either.
const PROBE_REGEX: &str = r"^__ALLOC_STATS_PROBE_REGEX__ (\d+)$";

/// Builds a `Manager` carrying one enabled `Noop` trigger per item name (each
/// pattern the regex-escaped literal → the Aho-Corasick tier) plus one per
/// entry in `regexes` (→ the regex-filtered tier), returning the engine's
/// action queue so passes can drain it. Duplicated from
/// `benches/trigger_engine.rs` (cargo targets cannot import from one another;
/// only the corpora live in the crate lib), with one addition: a `SendRaw`
/// probe trigger per matcher tier. Every fire, including `Noop`, first queues a
/// `RunAutomation`; `Noop` becomes no work only when that action is dispatched.
/// The probes use a distinctive action and markers that match no corpus line,
/// keeping the measured passes identical to the engine bench.
fn build_manager(names: &[String], regexes: &[&str]) -> (Manager, Queue) {
    let registry = SharedAutomationRegistry::default();
    let (mut mgr, queue) = Manager::new_for_bench(Arc::new(String::from(";")), registry);
    let empty: Arc<Vec<String>> = Arc::new(Vec::new());

    let push = |mgr: &mut Manager, name: String, pattern: String, action: ScriptAction| {
        // Both `name` and `patterns` are passed by reference, so they must
        // outlive the call; bind them to locals.
        let trigger_name = Arc::new(name);
        let patterns = Arc::new(vec![pattern]);
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
        })
        .expect("push_trigger");
    };

    for (i, name) in names.iter().enumerate() {
        push(
            &mut mgr,
            format!("item_{i}"),
            regex::escape(name),
            ScriptAction::Noop,
        );
    }
    for (i, pattern) in regexes.iter().enumerate() {
        push(
            &mut mgr,
            format!("regex_{i}"),
            (*pattern).to_owned(),
            ScriptAction::Noop,
        );
    }
    let probe_action = || ScriptAction::SendRaw(Arc::new(String::from("probe")));
    push(
        &mut mgr,
        String::from("probe_literal"),
        regex::escape(PROBE_LITERAL),
        probe_action(),
    );
    push(
        &mut mgr,
        String::from("probe_regex"),
        PROBE_REGEX.to_owned(),
        probe_action(),
    );

    (mgr, queue)
}

/// One focused exact-color positive trigger. The persisted matcher sidecar is
/// used here because this target exercises the native engine directly rather
/// than the JS registration bridge.
fn build_styled_normal_manager() -> (Manager, Queue) {
    let registry = SharedAutomationRegistry::default();
    let (mut manager, queue) = Manager::new_for_bench(Arc::new(String::from(";")), registry);
    let name = Arc::new(String::from("styled_normal"));
    let patterns = Arc::new(vec![String::from("target")]);
    let empty = Arc::new(Vec::<String>::new());
    let matchers = [TriggerMatcherSource {
        role: MatcherRole::Match,
        syntax: MatcherSyntax::Regex,
        source: String::from("target"),
        anchor_start: true,
        anchor_end: true,
        color: Some(MatcherColorMatch {
            foreground: Some(MatcherColor::Ansi { index: 1 }),
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
            fallthrough: false,
            fire_limit: None,
            line_limit: None,
            source: None,
        })
        .expect("styled normal trigger");
    (manager, queue)
}

/// One plain positive plus one exact-color anti-pattern. This isolates the
/// colored anti `RegexSet`'s no-hit and candidate-bitset paths.
fn build_styled_anti_manager() -> (Manager, Queue) {
    let registry = SharedAutomationRegistry::default();
    let (mut manager, queue) = Manager::new_for_bench(Arc::new(String::from(";")), registry);
    let name = Arc::new(String::from("styled_anti"));
    let patterns = Arc::new(vec![String::from("target")]);
    let raw_patterns = Arc::new(Vec::<String>::new());
    let anti_patterns = Arc::new(vec![String::from("block")]);
    let matchers = [
        TriggerMatcherSource {
            role: MatcherRole::Match,
            syntax: MatcherSyntax::Regex,
            source: String::from("target"),
            anchor_start: true,
            anchor_end: true,
            color: None,
        },
        TriggerMatcherSource {
            role: MatcherRole::Anti,
            syntax: MatcherSyntax::Regex,
            source: String::from("block"),
            anchor_start: true,
            anchor_end: true,
            color: Some(MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 1 }),
                ..Default::default()
            }),
        },
    ];
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
        })
        .expect("styled anti trigger");
    (manager, queue)
}

fn repeated_styled_line(text: &str, style: Style, count: usize) -> Vec<Arc<StyledLine>> {
    let line = Arc::new(StyledLine::new(
        text,
        vec![VtSpan {
            style,
            begin_pos: 0,
            end_pos: text.len(),
        }],
    ));
    vec![line; count]
}

/// Runs the dressed wire bytes through the inbound pipeline as one oversized
/// "read" and gathers what came out: the complete lines the `VtProcessor`
/// committed, and the count of partial-line flushes (prompt commits). A
/// single buffer keeps read boundaries out of the line texts, so the sanity
/// check can compare them 1:1 against the original corpus; the collected
/// lines carry real SGR-derived spans and feed the terminal-buffer workloads.
fn collect_ingest(wire_bytes: &[u8]) -> (Vec<Arc<StyledLine>>, usize) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeAction>();
    let mut telnet = TelnetParser::new();
    let mut vt_parser = VTParser::new();
    let mut vt_processor = VtProcessor::new(tx.clone());
    let mut replies: Vec<u8> = Vec::new();
    let mut protocol = ProtocolState::with_fixed_dims(DEFAULT_DIMS);
    let mut transcode = Transcode::default();
    let _ = feed_inbound(
        wire_bytes,
        &mut telnet,
        &mut vt_parser,
        &mut vt_processor,
        &mut replies,
        &tx,
        &mut protocol,
        &mut transcode,
    );
    vt_processor.notify_end_of_buffer();

    let mut complete = Vec::new();
    let mut partials = 0_usize;
    while let Ok(action) = rx.try_recv() {
        match action {
            RuntimeAction::HandleIncomingLine(line)
            | RuntimeAction::HandleIncomingFragmentedLine { line, .. } => complete.push(line),
            RuntimeAction::HandleIncomingPartialLine(_) => partials += 1,
            _ => {}
        }
    }
    (complete, partials)
}

/// Splits a line into four roughly equal fragments at char boundaries, each
/// a `StyledLine` with one default-style span tiling its full text (spans
/// must tile gap-free — the display invariant). An empty line yields one
/// empty fragment so the per-line commit accounting stays 1:1.
fn split_frag4(text: &str) -> Vec<Arc<StyledLine>> {
    if text.is_empty() {
        return vec![Arc::new(StyledLine::new("", Vec::new()))];
    }
    let len = text.len();
    let mut cuts = [0_usize; 5];
    for (i, cut) in cuts.iter_mut().enumerate() {
        let mut pos = len * i / 4;
        while pos < len && !text.is_char_boundary(pos) {
            pos += 1;
        }
        *cut = pos;
    }
    cuts[4] = len;

    let mut fragments = Vec::with_capacity(4);
    for window in cuts.windows(2) {
        let piece = &text[window[0]..window[1]];
        if piece.is_empty() {
            continue;
        }
        let span = VtSpan {
            style: DEFAULT_STYLE,
            begin_pos: 0,
            end_pos: piece.len(),
        };
        fragments.push(Arc::new(StyledLine::new(piece, vec![span])));
    }
    fragments
}

/// One measured ingest pass: fresh per-connection parser state, the dressed
/// corpus fed chunk-by-chunk exactly as the connect loop composes a read —
/// `feed_inbound`, then `notify_end_of_buffer`, then drain the emitted
/// actions (the consumption the runtime channel performs).
fn ingest_pass(chunks: &[&[u8]]) -> (u64, u64) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeAction>();
    let mut telnet = TelnetParser::new();
    let mut vt_parser = VTParser::new();
    let mut vt_processor = VtProcessor::new(tx.clone());
    let mut replies: Vec<u8> = Vec::new();
    let mut protocol = ProtocolState::with_fixed_dims(DEFAULT_DIMS);
    let mut transcode = Transcode::default();
    measure(|| {
        let mut drained = 0_usize;
        for chunk in chunks {
            let _ = feed_inbound(
                chunk,
                &mut telnet,
                &mut vt_parser,
                &mut vt_processor,
                &mut replies,
                &tx,
                &mut protocol,
                &mut transcode,
            );
            vt_processor.notify_end_of_buffer();
            while let Ok(action) = rx.try_recv() {
                drained += 1;
                black_box(&action);
            }
        }
        drained
    })
}

/// One measured `StyledLine::new_with_raw` pass. The destination `Vec` is
/// pre-sized outside the measurement so only the constructor's own
/// allocations are counted; the raw bytes stand in for the pre-strip wire
/// form the processor accumulates.
fn styled_line_pass(lines: &[String]) -> (u64, u64) {
    let mut out: Vec<StyledLine> = Vec::with_capacity(lines.len());
    measure(|| {
        for line in lines {
            out.push(StyledLine::new_with_raw(
                line,
                Vec::new(),
                Some(line.as_bytes()),
            ));
        }
        out.len()
    })
}

/// One measured whole-line commit stream into a fresh scrollback buffer:
/// `extend_line` + `commit_current_line` per line, the order core emits
/// (`BufferUpdate::Append` then `EnsureNewLine`). Buffer construction (a
/// 10k-slot `VecDeque`) stays outside the measurement.
fn whole_line_pass(styled: &[Arc<StyledLine>]) -> (u64, u64) {
    let mut buffer = TerminalBuffer::new();
    measure(|| {
        for line in styled {
            buffer.extend_line(line.clone());
            buffer.commit_current_line();
        }
        buffer.last_line_number()
    })
}

/// One measured fragmented commit stream: each line arrives as ~4 partial
/// deliveries glued by `extend_line`'s append path (pop the open tail,
/// `StyledLine::append`, re-bake spans), then commits.
fn frag4_pass(fragged: &[Vec<Arc<StyledLine>>]) -> (u64, u64) {
    let mut buffer = TerminalBuffer::new();
    measure(|| {
        for fragments in fragged {
            for fragment in fragments {
                buffer.extend_line(fragment.clone());
            }
            buffer.commit_current_line();
        }
        buffer.last_line_number()
    })
}

/// One measured trigger scan over the corpus, draining the action queue at the
/// end of the pass. A matching `Noop` trigger still contributes one deferred
/// `RunAutomation`; dispatch (which this native engine target intentionally
/// omits) is where it becomes no work. `clear` keeps the deque's capacity and
/// the engine's regex caches stay warm from the warmup pass, so pass-to-pass
/// counts are steady-state and comparable.
fn trigger_pass(mgr: &mut Manager, queue: &Queue, styled: &[Arc<StyledLine>]) -> (u64, u64) {
    measure(|| {
        for line in styled {
            mgr.process_incoming_line(line)
                .expect("process_incoming_line");
        }
        let fired = queue.len();
        queue.clear();
        fired
    })
}

/// One table row, reporting the second measured pass of a workload.
struct Row {
    name: &'static str,
    lines: usize,
    allocs: u64,
    bytes: u64,
    stable: bool,
}

/// Warmup pass (uncounted: lazy statics, regex scratch/DFA caches, capacity
/// growth in reused state), then two measured passes. The passes must agree
/// exactly — these numbers are meant to become CI-assertable ceilings, and a
/// nondeterministic count can never be one — so any disagreement is reported
/// loudly and flagged in the summary.
fn run_workload(name: &'static str, lines: usize, mut pass: impl FnMut() -> (u64, u64)) -> Row {
    eprintln!("running {name} (warmup + 2 measured passes)...");
    pass();
    let (first_allocs, first_bytes) = pass();
    let (allocs, bytes) = pass();
    let stable = first_allocs == allocs && first_bytes == bytes;
    if !stable {
        eprintln!(
            "WARNING: {name} allocated differently across passes: \
             {first_allocs} vs {allocs} allocs, {first_bytes} vs {bytes} bytes \
             — nondeterministic, not usable as a ceiling"
        );
    }
    Row {
        name,
        lines,
        allocs,
        bytes,
        stable,
    }
}

/// Pins the allocator instrumentation itself: a `Vec::with_capacity(4096)`
/// must register as exactly one allocation of exactly 4096 bytes, or every
/// number in the table is garbage.
fn sanity_allocator() {
    let (allocs, bytes) = measure(|| Vec::<u8>::with_capacity(4096));
    assert_eq!(
        allocs, 1,
        "allocator sanity: expected 1 alloc, saw {allocs}"
    );
    assert_eq!(
        bytes, 4096,
        "allocator sanity: expected 4096 bytes, saw {bytes}"
    );
    eprintln!("sanity: allocator counts a 4096-byte Vec as exactly (1 alloc, 4096 bytes)");
}

/// The printable text a corpus line displays as: CSI (`ESC [ … final`) and
/// OSC (`ESC ] … BEL|ST`) sequences removed, then control characters dropped —
/// what the VT parser routes away from the line text. A corpus may carry its
/// own escapes (a captured session, an OSC 8 feature log) on top of the wire
/// dressing.
fn display_text(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if !c.is_control() {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                // Parameter/intermediate bytes, then one final byte in 0x40..=0x7E.
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                // Runs to BEL or ST (`ESC \`).
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Any other escape is ESC plus one byte.
            _ => {}
        }
    }
    out
}

/// Pins the ingest workload to the real pipeline: every corpus line must come
/// back out as a committed `HandleIncomingLine` whose text matches the
/// original's display text (see [`display_text`]), and the `AnsiLight`
/// prompts must surface as exactly one partial-line flush per 20 lines.
fn sanity_ingest(styled: &[Arc<StyledLine>], partials: usize, lines: &[String]) {
    assert_eq!(
        styled.len(),
        lines.len(),
        "ingest sanity: committed-line count must match the corpus"
    );
    for (produced, original) in styled.iter().zip(lines) {
        let expected = display_text(original);
        assert_eq!(
            produced.text, expected,
            "ingest sanity: line text corrupted in the pipeline"
        );
    }
    assert_eq!(
        partials,
        lines.len() / 20,
        "ingest sanity: AnsiLight emits one prompt flush per 20 lines"
    );
    eprintln!(
        "sanity: ingest reproduced all {} lines byte-for-byte (+{partials} prompt flushes)",
        lines.len()
    );
}

/// Pins the trigger workload to a live engine: the `SendRaw` probe trigger on
/// each matcher tier (literal / regex-filtered) must enqueue an action when
/// its marker line arrives. The queue observes `RunAutomation` before dispatch,
/// including for `Noop`; dedicated probe markers make each tier's liveness
/// check independent of incidental corpus-pattern matches.
fn sanity_trigger(mgr: &mut Manager, queue: &Queue) {
    let literal_probe = Arc::new(StyledLine::new(
        &format!("loot: {PROBE_LITERAL} acquired"),
        Vec::new(),
    ));
    mgr.process_incoming_line(&literal_probe)
        .expect("literal probe line");
    let literal_fired = queue.len();
    assert!(
        literal_fired >= 1,
        "trigger sanity: the literal-tier probe must fire"
    );
    queue.clear();

    let regex_probe = Arc::new(StyledLine::new(
        "__ALLOC_STATS_PROBE_REGEX__ 4242",
        Vec::new(),
    ));
    mgr.process_incoming_line(&regex_probe)
        .expect("regex probe line");
    let regex_fired = queue.len();
    assert!(
        regex_fired >= 1,
        "trigger sanity: the regex-tier probe must fire"
    );
    queue.clear();
    eprintln!(
        "sanity: trigger engine fired on both tiers ({literal_fired} literal / {regex_fired} regex action(s))"
    );
}

/// Pins the focused styled rows to their intended control flow. Allocation
/// comparisons are meaningless if a supposed miss accidentally fires, or if
/// an anti hit fails to veto and changes capture/action work.
fn sanity_styled_outcomes(
    family: &str,
    manager: &mut Manager,
    queue: &Queue,
    cases: &[(&str, &Arc<StyledLine>, usize)],
) {
    for (case, line, expected_actions) in cases {
        assert!(queue.is_empty(), "{family}/{case}: dirty action queue");
        manager
            .process_incoming_line(line)
            .expect("styled trigger sanity line");
        let actual_actions = queue.len();
        queue.clear();
        assert_eq!(
            actual_actions, *expected_actions,
            "{family}/{case}: unexpected queued RunAutomation count"
        );
    }
    eprintln!("sanity: {family} text/color miss and hit outcomes are correct");
}

/// Pins the terminal-buffer workloads to real scrollback behavior: line
/// numbering must match the corpus for both delivery shapes (whole lines and
/// fragments), eviction must cap the buffer at the default scrollback, and
/// the fragmented tail line must reassemble to its original text.
fn sanity_terminal_buffer(
    styled: &[Arc<StyledLine>],
    fragged: &[Vec<Arc<StyledLine>>],
    lines: &[String],
) {
    let mut buffer = TerminalBuffer::new();
    for line in styled {
        buffer.extend_line(line.clone());
        buffer.commit_current_line();
    }
    assert_eq!(
        buffer.last_line_number(),
        styled.len(),
        "terminal-buffer sanity: whole-line numbering must match the corpus"
    );
    assert_eq!(
        buffer.len(),
        styled.len().min(DEFAULT_SCROLLBACK),
        "terminal-buffer sanity: scrollback must cap at the default limit"
    );

    let mut buffer = TerminalBuffer::new();
    for fragments in fragged {
        for fragment in fragments {
            buffer.extend_line(fragment.clone());
        }
        buffer.commit_current_line();
    }
    assert_eq!(
        buffer.last_line_number(),
        fragged.len(),
        "terminal-buffer sanity: fragmented numbering must match the corpus"
    );
    let tail = buffer
        .iter_rev()
        .next()
        .expect("buffer holds the tail line");
    assert_eq!(
        tail.styled_line.text,
        *lines.last().expect("non-empty corpus"),
        "terminal-buffer sanity: fragments must reassemble the original text"
    );
    eprintln!("sanity: terminal buffer accounting matches for whole-line and fragmented delivery");
}

/// Per-line rate for the table. Corpus sizes are far below 2^52, so the
/// conversion is exact.
#[allow(clippy::cast_precision_loss)]
fn per_line(value: u64, lines: usize) -> f64 {
    value as f64 / lines as f64
}

/// Renders the aligned result table to stdout (diagnostics go to stderr, so
/// the table is what a pipeline consumer captures) plus the stability
/// verdict the ceilings depend on.
fn print_table(rows: &[Row]) {
    println!(
        "{:<24} {:>10} {:>12} {:>13} {:>13}",
        "workload", "lines", "allocs", "allocs/line", "bytes/line"
    );
    for row in rows {
        println!(
            "{:<24} {:>10} {:>12} {:>13.2} {:>13.1}",
            row.name,
            row.lines,
            row.allocs,
            per_line(row.allocs, row.lines),
            per_line(row.bytes, row.lines),
        );
    }
    if rows.iter().all(|row| row.stable) {
        println!(
            "\nstability: every workload allocated identically across both measured passes; \
             the numbers above are candidates for CI-assertable ceilings"
        );
    } else {
        println!(
            "\nstability: some workloads were NONDETERMINISTIC across passes (see warnings \
             above); their numbers cannot become ceilings yet"
        );
    }
}

#[allow(clippy::too_many_lines)]
fn main() {
    let skip_sanity = std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_ok();
    // `SMUDGY_BENCH_CORPUS=<path>`, else the first log in `bench/logs/`
    // (name-sorted, so deterministic), matching the criterion benches;
    // `SMUDGY_BENCH_LINES` is honored by the loader.
    let (corpus_name, lines) = default_corpus();
    assert!(!lines.is_empty(), "empty corpus (SMUDGY_BENCH_LINES=0?)");
    let corpus_bytes: usize = lines.iter().map(String::len).sum();

    // Force the UI prefs snapshot (a LazyLock seeded from default settings)
    // so its one-time construction never lands inside a measured pass; the
    // palette name doubles as a diagnostic that the default snapshot is live.
    let prefs = smudgy_ui::prefs::current();
    eprintln!(
        "corpus {corpus_name}: {} lines / {corpus_bytes} bytes; palette: {}",
        lines.len(),
        prefs.palette.name
    );

    let wire_bytes = wire::dress_lines(&lines, wire::WireProfile::AnsiLight);
    let chunks = wire::chunk(&wire_bytes, READ_CHUNK);
    eprintln!(
        "wire: {} bytes dressed (AnsiLight), {} read chunks of {READ_CHUNK} bytes",
        wire_bytes.len(),
        chunks.len()
    );

    let (styled, partials) = collect_ingest(&wire_bytes);
    let fragged: Vec<Vec<Arc<StyledLine>>> = lines.iter().map(|l| split_frag4(l)).collect();
    let fragment_total: usize = fragged.iter().map(Vec::len).sum();
    eprintln!(
        "display corpora: {} committed lines, {fragment_total} fragments",
        styled.len()
    );

    let names = load_item_names_10k();
    eprintln!(
        "trigger engine: {} literal + {} regex triggers (building)...",
        names.len(),
        REGEX_TRIGGERS.len()
    );
    let (mut mgr, queue) = build_manager(&names, REGEX_TRIGGERS);
    // The first incoming line pays the one-time PatternSet rebuild; spend it
    // here so measured passes see steady-state matching (as trigger_engine
    // does).
    mgr.process_incoming_line(&Arc::new(StyledLine::new("warmup", Vec::new())))
        .expect("warmup");
    queue.clear();

    if skip_sanity {
        eprintln!("sanity checks SKIPPED (SMUDGY_BENCH_SKIP_SANITY set)");
    } else {
        sanity_allocator();
        sanity_ingest(&styled, partials, &lines);
        sanity_trigger(&mut mgr, &queue);
        sanity_terminal_buffer(&styled, &fragged, &lines);
    }

    // The trigger scan takes span-less lines, exactly what trigger_engine
    // feeds `process_incoming_line` (matching reads only `.text`).
    let styled_plain: Vec<Arc<StyledLine>> = lines
        .iter()
        .map(|l| Arc::new(StyledLine::new(l, Vec::new())))
        .collect();

    let default_style = Style::default();
    let red_style = Style {
        fg: Color::Ansi {
            color: AnsiColor::Red,
            bold: false,
        },
        ..Style::default()
    };
    let styled_text_miss = repeated_styled_line("clear", default_style, lines.len());
    let styled_color_miss = repeated_styled_line("target", default_style, lines.len());
    let styled_color_hit = repeated_styled_line("target", red_style, lines.len());
    let anti_text_miss = repeated_styled_line("target clear", default_style, lines.len());
    let anti_color_miss = repeated_styled_line("target block", default_style, lines.len());
    let anti_color_hit = repeated_styled_line("target block", red_style, lines.len());
    let (mut styled_normal, styled_normal_queue) = build_styled_normal_manager();
    let (mut styled_anti, styled_anti_queue) = build_styled_anti_manager();
    styled_normal
        .process_incoming_line(&styled_text_miss[0])
        .expect("styled normal warmup");
    styled_anti
        .process_incoming_line(&anti_text_miss[0])
        .expect("styled anti warmup");
    styled_normal_queue.clear();
    styled_anti_queue.clear();
    if !skip_sanity {
        sanity_styled_outcomes(
            "styled normal",
            &mut styled_normal,
            &styled_normal_queue,
            &[
                ("text miss", &styled_text_miss[0], 0),
                ("color miss", &styled_color_miss[0], 0),
                ("color hit", &styled_color_hit[0], 1),
            ],
        );
        sanity_styled_outcomes(
            "styled anti",
            &mut styled_anti,
            &styled_anti_queue,
            &[
                ("prefilter miss / positive fires", &anti_text_miss[0], 1),
                ("color miss / positive fires", &anti_color_miss[0], 1),
                ("color hit / veto", &anti_color_hit[0], 0),
            ],
        );
    }

    let rows = vec![
        run_workload("ingest", lines.len(), || ingest_pass(&chunks)),
        run_workload("styled_line", lines.len(), || styled_line_pass(&lines)),
        run_workload("terminal_buffer_whole", lines.len(), || {
            whole_line_pass(&styled)
        }),
        run_workload("terminal_buffer_frag4", lines.len(), || {
            frag4_pass(&fragged)
        }),
        run_workload("trigger_scan", lines.len(), || {
            trigger_pass(&mut mgr, &queue, &styled_plain)
        }),
        run_workload("styled_normal_text_miss", lines.len(), || {
            trigger_pass(&mut styled_normal, &styled_normal_queue, &styled_text_miss)
        }),
        run_workload("styled_normal_color_miss", lines.len(), || {
            trigger_pass(&mut styled_normal, &styled_normal_queue, &styled_color_miss)
        }),
        run_workload("styled_normal_color_hit", lines.len(), || {
            trigger_pass(&mut styled_normal, &styled_normal_queue, &styled_color_hit)
        }),
        run_workload(
            "styled_anti_prefilter_miss_positive_fires",
            lines.len(),
            || trigger_pass(&mut styled_anti, &styled_anti_queue, &anti_text_miss),
        ),
        run_workload("styled_anti_color_miss_positive_fires", lines.len(), || {
            trigger_pass(&mut styled_anti, &styled_anti_queue, &anti_color_miss)
        }),
        run_workload("styled_anti_color_hit_veto", lines.len(), || {
            trigger_pass(&mut styled_anti, &styled_anti_queue, &anti_color_hit)
        }),
    ];

    print_table(&rows);
}
