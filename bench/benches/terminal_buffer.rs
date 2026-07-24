//! The UI thread's per-line cost: `smudgy_ui::terminal_buffer` (scrollback +
//! span baking), the layer every line of server output crosses between the
//! session runtime and the renderer.
//!
//! What it measures (all in `ui/src/terminal_buffer.rs`):
//!   - `extend_line/{whole_lines,frag4,frag16}`: one corpus pass into a fresh
//!     `TerminalBuffer`, fed as whole lines vs. 4/16 network-fragment slices
//!     per line. For an OPEN line, `TerminalBuffer::extend_line` deep-copies
//!     the whole accumulated line for every arriving fragment
//!     (`StyledLine::append`), so fragment arrival costs O(fragments × line
//!     length) per line. Span baking is lazy (a pane bakes a line's spans on
//!     first layout), so these ids measure buffer accounting only. The three
//!     ids share `Throughput::Elements(lines)`, so the per-LINE cost
//!     inflation across them is directly readable.
//!   - `extend_line/at_capacity`: the same whole-line feed into a buffer held
//!     at its 10,000-line default capacity, so every committed line also pays
//!     `pop_front` eviction — steady state for a long session. The fresh-buffer
//!     variants feed at most `capacity` lines and never evict, keeping the two
//!     costs separable.
//!   - `to_spans/by_span_count/{1,8,32}`: `BufferLine::spans()`, the lazy
//!     first-visibility bake — one owned-`String` allocation per span, every
//!     color resolved through `prefs::current()`.
//!   - `line_operations/replace_and_highlight`: `perform_line_operation` with
//!     `Replace`/`Highlight` `LineOperation`s (from
//!     `core/src/session/runtime/line_operation.rs`) against lines sampled
//!     across a full 10,000-line buffer — the trigger-driven line-rewrite
//!     path, including the single-line span re-bake it ends with.
//!
//! Why it matters: this path runs on the iced UI thread for every line (and
//! every partial-line fragment) a server sends; a slow bake or the fragment
//! pathology directly steals frame budget during heavy output.
//!
//! Prefs are the headless defaults: `smudgy_ui::prefs::current()`
//! self-initializes from `Settings::default()` on first use — no window, no
//! settings file.
//!
//! Env vars: `SMUDGY_BENCH_LINES=n` truncates the log corpus (faster runs),
//! `SMUDGY_BENCH_SKIP_SANITY=1` skips the count/reassembly/tiling checks.

use std::{hint::black_box, num::NonZeroUsize, sync::Arc};

use criterion::{
    BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use smudgy_bench::log_corpora;
use smudgy_core::session::{
    connection::vt_processor::AnsiColor,
    runtime::line_operation::LineOperation,
    styled_line::{Color, Style, StyledLine, VtSpan},
};
use smudgy_ui::terminal_buffer::{BufferLine, TerminalBuffer};

/// Styles cycled across tiled spans, mixing the color kinds `to_spans` must
/// resolve (default foreground, indexed ANSI, bold, truecolor RGB) so the
/// palette-resolution cost is representative rather than one hot match arm.
const STYLE_CYCLE: [Style; 4] = [
    Style {
        fg: Color::DefaultForeground { bold: false },
        bg: Color::DefaultBackground,
    },
    Style {
        fg: Color::Ansi {
            color: AnsiColor::Cyan,
            bold: false,
        },
        bg: Color::DefaultBackground,
    },
    Style {
        fg: Color::Ansi {
            color: AnsiColor::Yellow,
            bold: true,
        },
        bg: Color::DefaultBackground,
    },
    Style {
        fg: Color::Rgb {
            r: 180,
            g: 120,
            b: 60,
        },
        bg: Color::DefaultBackground,
    },
];

/// A classic trigger highlight: black-on-yellow over whatever the line has.
const HIGHLIGHT_STYLE: Style = Style {
    fg: Color::Ansi {
        color: AnsiColor::Black,
        bold: false,
    },
    bg: Color::Ansi {
        color: AnsiColor::Yellow,
        bold: false,
    },
};

/// Splits `text` into up to `parts` contiguous, non-empty chunks cut on char
/// boundaries — the shape of network fragments: one line arriving in pieces,
/// each piece a valid UTF-8 slice. Empty text yields one empty chunk so every
/// corpus line still produces at least one `extend_line` call.
fn split_fragments(text: &str, parts: usize) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    let len = text.len();
    let mut cuts = vec![0_usize];
    for i in 1..parts {
        let mut pos = len * i / parts;
        while pos > 0 && !text.is_char_boundary(pos) {
            pos -= 1;
        }
        if pos > *cuts.last().expect("cuts starts non-empty") {
            cuts.push(pos);
        }
    }
    cuts.push(len);
    cuts.windows(2).map(|w| &text[w[0]..w[1]]).collect()
}

/// Tiles `text` with up to `n` contiguous spans — each `begin_pos` equal to
/// the previous `end_pos`, the final `end_pos` equal to the text length (the
/// non-overlapping, gap-free invariant the renderer's span slicing relies
/// on) — cycling through [`STYLE_CYCLE`]. Empty text carries no spans.
fn tile_spans(text: &str, n: usize) -> Vec<VtSpan> {
    let mut spans = Vec::new();
    if text.is_empty() {
        return spans;
    }
    let mut begin = 0;
    for frag in split_fragments(text, n) {
        spans.push(VtSpan {
            style: STYLE_CYCLE[spans.len() % STYLE_CYCLE.len()],
            begin_pos: begin,
            end_pos: begin + frag.len(),
        });
        begin += frag.len();
    }
    spans
}

/// Clamps `pos` to `text`'s length and snaps it down to a char boundary, so
/// the byte offsets handed to `LineOperation`s can never slice mid-codepoint.
fn floor_char_boundary(text: &str, mut pos: usize) -> usize {
    pos = pos.min(text.len());
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

/// Every corpus line as `parts` fragment `StyledLine`s, each carrying a single
/// span over its full text — the shape a styled network fragment arrives in.
fn fragment_corpus(texts: &[String], parts: usize) -> Vec<Vec<Arc<StyledLine>>> {
    texts
        .iter()
        .map(|text| {
            split_fragments(text, parts)
                .into_iter()
                .map(|frag| Arc::new(StyledLine::new(frag, tile_spans(frag, 1))))
                .collect()
        })
        .collect()
}

/// One corpus pass of terminated whole lines: `extend_line` then the newline
/// commit, per line — the runtime's delivery sequence when lines arrive whole.
fn feed_whole(buffer: &mut TerminalBuffer, corpus: &[Arc<StyledLine>]) {
    for line in corpus {
        buffer.extend_line(line.clone());
        buffer.commit_current_line();
    }
}

/// One corpus pass of fragmented lines: every fragment through `extend_line`
/// (each one re-copying and re-baking the accumulated open line), the commit
/// only at line end.
fn feed_fragmented(buffer: &mut TerminalBuffer, corpus: &[Vec<Arc<StyledLine>>]) {
    for fragments in corpus {
        for fragment in fragments {
            buffer.extend_line(fragment.clone());
        }
        buffer.commit_current_line();
    }
}

/// Fills `buffer` up to `capacity` lines by cycling `corpus` — the state a
/// long-running session's scrollback sits in.
fn fill_to_capacity(
    buffer: &mut TerminalBuffer,
    corpus: &[Arc<StyledLine>],
    capacity: NonZeroUsize,
) {
    let mut i = 0_usize;
    while buffer.len() < capacity.get() {
        buffer.extend_line(corpus[i % corpus.len()].clone());
        buffer.commit_current_line();
        i += 1;
    }
}

/// Validates the measurement measures the real thing before trusting numbers:
/// a fed buffer holds `min(corpus, capacity)` lines, fragmented feeds
/// reassemble byte-identically to their whole line, and baked `BufferLine`s'
/// concatenated span text equals their `StyledLine` text (the tiling
/// invariant the renderer depends on).
fn sanity_check(
    texts: &[String],
    whole: &[Arc<StyledLine>],
    frag: &[Vec<Arc<StyledLine>>],
    capacity: NonZeroUsize,
) {
    // The fragment splitter loses no bytes: rejoining every line's fragments
    // reproduces the corpus text.
    for (text, fragments) in texts.iter().zip(frag) {
        let rejoined: String = fragments.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(&rejoined, text, "fragment split lost bytes");
    }

    let mut buffer = TerminalBuffer::new_with_max_lines(capacity);
    feed_whole(&mut buffer, whole);
    assert_eq!(
        buffer.len(),
        whole.len().min(capacity.get()),
        "whole-line feed must hold min(corpus, capacity) lines"
    );
    let mut tiled = 0_usize;
    for (_, line) in buffer.iter_rev_with_line_number(None).take(64) {
        let baked: String = line.spans().iter().map(|s| s.text.as_ref()).collect();
        assert_eq!(
            baked, line.styled_line.text,
            "baked spans do not tile the line text"
        );
        tiled += 1;
    }

    let mut frag_buffer = TerminalBuffer::new_with_max_lines(capacity);
    let sample = &frag[..frag.len().min(64)];
    feed_fragmented(&mut frag_buffer, sample);
    assert_eq!(
        frag_buffer.len(),
        sample.len(),
        "fragmented feed must hold one buffer line per corpus line"
    );
    for ((_, got), want) in frag_buffer
        .iter_rev_with_line_number(None)
        .zip(sample.iter().rev())
    {
        let whole_text: String = want.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(
            got.styled_line.text, whole_text,
            "fragmented feed reassembled the wrong text"
        );
        let baked: String = got.spans().iter().map(|s| s.text.as_ref()).collect();
        assert_eq!(
            baked, got.styled_line.text,
            "fragment-accumulated spans do not tile the line text"
        );
    }

    eprintln!(
        "sanity: {} lines buffered, {tiled} baked lines tile their text, {} fragmented lines reassemble byte-identically",
        buffer.len(),
        sample.len()
    );
}

#[allow(clippy::too_many_lines)]
fn terminal_buffer(c: &mut Criterion) {
    // The first (name-sorted) corpus in `bench/logs/` — one synthetic session
    // is enough here, since the buffer cost depends on line shape, not on
    // which triggers a corpus exercises.
    let (corpus_name, raw) = log_corpora()
        .into_iter()
        .next()
        .expect("bench/logs holds at least one corpus");
    assert!(!raw.is_empty(), "empty log corpus");

    // `to_spans` resolves every span color through `prefs::current()`;
    // headless, the LazyLock self-initializes from `Settings::default()` on
    // this first call — no window, no settings file.
    let prefs = smudgy_ui::prefs::current();

    // `TerminalBuffer::new()`'s default scrollback limit.
    let capacity = NonZeroUsize::new(10_000).expect("non-zero capacity");

    // The fresh-buffer variants feed at most `capacity` lines so they never
    // pay eviction; `at_capacity` isolates that cost.
    let fed: Vec<String> = raw.iter().take(capacity.get()).cloned().collect();
    let fed_bytes: u64 = fed.iter().map(|l| l.len() as u64).sum();

    let whole: Vec<Arc<StyledLine>> = fed
        .iter()
        .map(|text| Arc::new(StyledLine::new(text, tile_spans(text, 3))))
        .collect();
    let frag4 = fragment_corpus(&fed, 4);
    let frag16 = fragment_corpus(&fed, 16);

    eprintln!(
        "corpus {corpus_name}: {} lines available, feeding {} lines / {fed_bytes} bytes per pass; buffer capacity {capacity}; palette '{}' (prefs generation {})",
        raw.len(),
        fed.len(),
        prefs.palette.name,
        prefs.generation
    );

    if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
        sanity_check(&fed, &whole, &frag16, capacity);
    }

    let mut group = c.benchmark_group("extend_line");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    // All variants share Elements(lines): frag4/frag16 do 4×/16× the
    // `extend_line` calls for the same line count, so their per-line inflation
    // reads directly off the Kelem/s column.
    group.throughput(Throughput::Elements(fed.len() as u64));
    group.bench_function("whole_lines", |b| {
        b.iter_batched_ref(
            || TerminalBuffer::new_with_max_lines(capacity),
            |buffer| {
                feed_whole(buffer, &whole);
                black_box(buffer.len());
            },
            BatchSize::PerIteration,
        );
    });
    group.bench_function("frag4", |b| {
        b.iter_batched_ref(
            || TerminalBuffer::new_with_max_lines(capacity),
            |buffer| {
                feed_fragmented(buffer, &frag4);
                black_box(buffer.len());
            },
            BatchSize::PerIteration,
        );
    });
    group.bench_function("frag16", |b| {
        b.iter_batched_ref(
            || TerminalBuffer::new_with_max_lines(capacity),
            |buffer| {
                feed_fragmented(buffer, &frag16);
                black_box(buffer.len());
            },
            BatchSize::PerIteration,
        );
    });

    // Steady state for a long session: the buffer sits at capacity, so every
    // committed line also pays `pop_front` eviction. The buffer persists
    // across iterations — it is at capacity before and after every pass.
    let mut steady = TerminalBuffer::new_with_max_lines(capacity);
    fill_to_capacity(&mut steady, &whole, capacity);
    group.bench_function("at_capacity", |b| {
        b.iter(|| {
            feed_whole(&mut steady, &whole);
            black_box(steady.len());
        });
    });
    group.finish();

    // The per-line span bake a pane triggers when a line first becomes
    // visible (`BufferLine::spans()`, lazy since the `OnceCell` change —
    // `extend_line` itself no longer bakes): one owned-`String` per span,
    // colors resolved through the prefs snapshot. The `BufferLine::from`
    // wrapper inside the pass is free; `spans()` forces the bake. Elements =
    // spans, so per-span cost is comparable across the three counts.
    let sample_text = "The quick brown fox jumps over the lazy dog. ".repeat(7);
    let mut group = c.benchmark_group("to_spans");
    for &n in &[1_usize, 8, 32] {
        let spans = tile_spans(&sample_text, n);
        assert_eq!(spans.len(), n, "span tiling produced the requested count");
        let line = Arc::new(StyledLine::new(&sample_text, spans));
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(BenchmarkId::new("by_span_count", n), |b| {
            b.iter(|| {
                let line = BufferLine::from(line.clone());
                black_box(line.spans().clone())
            });
        });
    }
    group.finish();

    // The trigger-driven rewrite path: `perform_line_operation` locates the
    // line by absolute number, applies the transform, and re-bakes that one
    // line's spans. Replacements are same-length, so repeated application is
    // a fixed point; a warm pass below moves every sampled line to it, making
    // iterations measure a steady state rather than the first mutation.
    let mut ops_buffer = TerminalBuffer::new_with_max_lines(capacity);
    fill_to_capacity(&mut ops_buffer, &whole, capacity);
    let stride = (ops_buffer.len() / 64).max(1);
    let mut ops: Vec<(usize, LineOperation)> = Vec::new();
    for (i, (line_number, line)) in ops_buffer.iter_rev_with_line_number(None).enumerate() {
        if ops.len() >= 128 {
            break;
        }
        if i % stride != 0 {
            continue;
        }
        let text = line.styled_line.text.as_str();
        if text.len() < 8 {
            continue;
        }
        let replace_end = floor_char_boundary(text, 12);
        ops.push((
            line_number,
            LineOperation::Replace {
                str: Arc::new("#".repeat(replace_end)),
                begin: 0,
                end: replace_end,
            },
        ));
        ops.push((
            line_number,
            LineOperation::Highlight {
                begin: 0,
                end: floor_char_boundary(text, 20),
                style: HIGHLIGHT_STYLE,
            },
        ));
    }
    assert!(!ops.is_empty(), "no lines long enough for line_operations");
    for (line_number, op) in &ops {
        ops_buffer.perform_line_operation(*line_number, op.clone());
    }
    eprintln!(
        "line_operations: {} operations over {} sampled lines of a {}-line buffer",
        ops.len(),
        ops.len() / 2,
        ops_buffer.len()
    );

    let mut group = c.benchmark_group("line_operations");
    group.throughput(Throughput::Elements(ops.len() as u64));
    group.bench_function("replace_and_highlight", |b| {
        b.iter(|| {
            for (line_number, op) in &ops {
                ops_buffer.perform_line_operation(*line_number, op.clone());
            }
            black_box(ops_buffer.len());
        });
    });
    group.finish();
}

criterion_group!(benches, terminal_buffer);
criterion_main!(benches);
