//! Benchmarks the bytes-to-`StyledLine` **ingest pipeline** — the canonical
//! smudgy hot path every inbound socket byte crosses: telnet/IAC
//! preprocessing (`core/src/session/connection/telnet.rs`), VT/ANSI parsing
//! (`vtparse` driven by `core/src/session/connection/vt_processor.rs`), and
//! per-line `StyledLine` construction (`core/src/session/styled_line.rs`),
//! composed by `feed_inbound` (`core/src/session/connection.rs`) exactly the
//! way the connect loop's read path drives it (connection.rs ~232-263: read
//! up to 64 KiB, `feed_inbound`, flush negotiation replies, then
//! `notify_end_of_buffer`).
//!
//! Groups:
//! - `telnet_receive/{ansi_light,iac_dense}`: `TelnetParser::receive` alone,
//!   against a no-op sink — isolates the memchr IAC-scan fast path from the
//!   IAC-dense worst case (negotiations, GA prompts, subnegotiations,
//!   escaped `IAC IAC` literals).
//! - `ingest_pipeline/{ansi_light,ansi_heavy,iac_dense}`: the real
//!   composition — `feed_inbound` + `notify_end_of_buffer` per 16 KiB chunk,
//!   runtime channel drained per pass — with throughput in raw wire bytes.
//! - `styled_line/new_with_raw/{short_plain,long_plain,long_styled}`: the
//!   per-line cost of `StyledLine::new_with_raw` — the text copy, the
//!   span-vec allocation (`consume_into_pending_line` drains into a fresh
//!   `Vec` per line), and the `from_utf8_lossy` re-validation of `buf_raw`.
//! - `sgr/process/*`: the SGR state machine
//!   (`core/src/session/connection/vt_processor/sgr.rs`) folding
//!   representative `CSI … m` parameter lists — the per-escape style cost.
//!
//! Why it matters: this path is the socket-to-display latency floor and the
//! throughput ceiling for spammy MUD output; triggers, scripts, and the UI
//! all sit downstream of it, so nothing in the client can be faster than
//! this pipeline.
//!
//! Wire corpora are synthesized deterministically from the committed
//! plain-text logs by `smudgy_bench::wire` (the logs themselves carry zero
//! ESC/0xFF bytes, so they cannot exercise these layers undressed).
//! Requires `smudgy_core`'s `bench-api` feature (exposes `feed_inbound` and
//! `sgr_process`); the Cargo dev-dependency enables it.
//!
//! Env vars: `SMUDGY_BENCH_LINES=n` truncates the log corpus (faster runs),
//! `SMUDGY_BENCH_SKIP_SANITY=1` skips the pipeline-fidelity checks.

use std::{hint::black_box, sync::Arc, time::Instant};

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use smudgy_bench::{
    log_corpora,
    wire::{WireProfile, chunk, dress_lines},
};
use smudgy_core::session::{
    connection::{
        feed_inbound,
        responders::{DEFAULT_DIMS, ProtocolState},
        telnet::{TelnetParser, TelnetSink},
        transcode::Transcode,
        vt_processor::{AnsiColor, VtProcessor, sgr_process},
    },
    runtime::RuntimeAction,
    styled_line::{Color, Style, StyledLine, VtSpan},
};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use vtparse::{CsiParam, VTActor, VTParser};

/// Socket-read chunk size fed to `feed_inbound` per iteration. The connect
/// loop reads up to 64 KiB per `try_read_buf`; 16 KiB is a realistic actual
/// read size and guarantees several chunk boundaries (each an
/// `notify_end_of_buffer` partial-line flush) even on a truncated corpus.
const CHUNK_LEN: usize = 16 * 1024;

/// The style `VtProcessor::new` starts a connection with.
const fn default_style() -> Style {
    Style {
        fg: Color::DefaultForeground { bold: false },
        bg: Color::DefaultBackground,
    }
}

/// Minimal `TelnetSink` that `black_box`es and discards everything, so the
/// `telnet_receive` group times `TelnetParser::receive` itself — the memchr
/// bulk scan and the control-sequence state machine — with no VT-layer work.
struct NullSink;

impl TelnetSink for NullSink {
    fn on_data(&mut self, data: &[u8]) {
        black_box(data);
    }

    fn on_prompt(&mut self) {}

    fn on_send(&mut self, bytes: &[u8]) {
        black_box(bytes);
    }

    fn on_subnegotiation(&mut self, option: u8, payload: &[u8]) {
        black_box((option, payload));
    }
}

/// Counting `TelnetSink` for the sanity pass: proves the dressed corpus
/// actually drives the parser's IAC paths (prompts, subnegotiations,
/// negotiation replies, un-escaped `IAC IAC` literals in the data stream).
#[derive(Default)]
struct CountingSink {
    data_bytes: usize,
    literal_ff_bytes: usize,
    prompts: usize,
    subnegotiations: usize,
    reply_bytes: usize,
}

impl TelnetSink for CountingSink {
    fn on_data(&mut self, data: &[u8]) {
        self.data_bytes += data.len();
        // Sanity-pass-only counting; not worth a `bytecount` dependency.
        #[allow(clippy::naive_bytecount)]
        {
            self.literal_ff_bytes += data.iter().filter(|&&b| b == 0xFF).count();
        }
    }

    fn on_prompt(&mut self) {
        self.prompts += 1;
    }

    fn on_send(&mut self, bytes: &[u8]) {
        self.reply_bytes += bytes.len();
    }

    fn on_subnegotiation(&mut self, _option: u8, _payload: &[u8]) {
        self.subnegotiations += 1;
    }
}

/// The per-connection parser state the connect loop owns (connection.rs
/// ~194-201), plus the receiving end of the runtime channel `VtProcessor`
/// emits `RuntimeAction`s into. The receiver must stay alive — `VtProcessor`
/// panics on a closed channel — and must be drained every pass or lines pile
/// up unboundedly across sample iterations.
struct Pipeline {
    telnet: TelnetParser,
    vt_parser: VTParser,
    vt_processor: VtProcessor,
    replies: Vec<u8>,
    /// The same channel `VtProcessor` emits on — `feed_inbound` forwards GMCP
    /// messages over it in stream order, exactly like the connect loop.
    runtime_tx: tokio::sync::mpsc::UnboundedSender<RuntimeAction>,
    rx: UnboundedReceiver<RuntimeAction>,
    protocol: ProtocolState,
    transcode: Transcode,
}

impl Pipeline {
    fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            telnet: TelnetParser::new(),
            vt_parser: VTParser::new(),
            vt_processor: VtProcessor::new(tx.clone()),
            replies: Vec::new(),
            runtime_tx: tx,
            rx,
            protocol: ProtocolState::with_fixed_dims(DEFAULT_DIMS),
            transcode: Transcode::default(),
        }
    }

    /// One full pass of the corpus through the real read-loop composition:
    /// `feed_inbound` per chunk (telnet strip + VT parse + line commits),
    /// the negotiation replies observed in place of the socket write-back,
    /// then the end-of-read partial-line flush.
    fn feed(&mut self, chunks: &[&[u8]]) {
        for &data in chunks {
            let _ = feed_inbound(
                data,
                &mut self.telnet,
                &mut self.vt_parser,
                &mut self.vt_processor,
                &mut self.replies,
                &self.runtime_tx,
                &mut self.protocol,
                &mut self.transcode,
            );
            black_box(self.replies.as_slice());
            self.vt_processor.notify_end_of_buffer();
        }
    }

    /// Drains the runtime channel, `black_box`ing and dropping every emitted
    /// line. Returns `(complete, partial)` line counts.
    fn drain_counts(&mut self) -> (u64, u64) {
        let mut complete = 0_u64;
        let mut partial = 0_u64;
        while let Ok(action) = self.rx.try_recv() {
            match action {
                RuntimeAction::HandleIncomingLine(line) => {
                    complete += 1;
                    black_box(line);
                }
                RuntimeAction::HandleIncomingPartialLine(line) => {
                    partial += 1;
                    black_box(line);
                }
                _ => {}
            }
        }
        (complete, partial)
    }

    /// Drains the runtime channel keeping the lines, for the sanity pass.
    fn drain_lines(&mut self) -> (Vec<Arc<StyledLine>>, Vec<Arc<StyledLine>>) {
        let mut complete = Vec::new();
        let mut partial = Vec::new();
        while let Ok(action) = self.rx.try_recv() {
            match action {
                RuntimeAction::HandleIncomingLine(line) => complete.push(line),
                RuntimeAction::HandleIncomingPartialLine(line) => partial.push(line),
                _ => {}
            }
        }
        (complete, partial)
    }
}

/// `VTActor` that captures the `CsiParam` list of each SGR (`CSI … m`)
/// dispatch — the exact slice `VtProcessor::csi_dispatch` hands to
/// `sgr::process` — so the `sgr` group's inputs come from parsing real escape
/// bytes rather than hand-assembled params.
#[derive(Default)]
struct SgrCapture {
    params: Vec<Vec<CsiParam>>,
}

impl VTActor for SgrCapture {
    fn print(&mut self, _b: char) {}

    fn execute_c0_or_c1(&mut self, _control: u8) {}

    fn dcs_hook(
        &mut self,
        _byte: u8,
        _params: &[i64],
        _intermediates: &[u8],
        _ignored_excess_intermediates: bool,
    ) {
    }

    fn dcs_put(&mut self, _byte: u8) {}

    fn dcs_unhook(&mut self) {}

    fn esc_dispatch(
        &mut self,
        _params: &[i64],
        _intermediates: &[u8],
        _ignored_excess_intermediates: bool,
        _byte: u8,
    ) {
    }

    fn csi_dispatch(&mut self, params: &[CsiParam], _parameters_truncated: bool, byte: u8) {
        if byte == b'm' {
            self.params.push(params.to_vec());
        }
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]]) {}

    fn apc_dispatch(&mut self, _data: Vec<u8>) {}
}

/// Parses one escape sequence's bytes and returns the `CsiParam` list its SGR
/// dispatch carried. Panics unless the bytes produce exactly one dispatch.
fn parse_sgr_params(bytes: &[u8]) -> Vec<CsiParam> {
    let mut parser = VTParser::new();
    let mut capture = SgrCapture::default();
    parser.parse(bytes, &mut capture);
    assert_eq!(
        capture.params.len(),
        1,
        "expected exactly one SGR dispatch from {bytes:?}"
    );
    capture.params.pop().expect("length asserted above")
}

/// The representative SGR vocabulary a MUD stream carries, as raw escape
/// bytes: a bare reset, a 16-color set, a bold+color pair, a 256-color set,
/// and a truecolor set.
const SGR_SEQUENCES: &[(&str, &[u8])] = &[
    ("reset", b"\x1b[0m"),
    ("simple_color", b"\x1b[31m"),
    ("bold_color", b"\x1b[1;31m"),
    ("color_256", b"\x1b[38;5;196m"),
    ("truecolor", b"\x1b[38;2;250;128;64m"),
];

/// Builds an ASCII line of exactly `len` bytes from repeated MUD-ish prose,
/// so span offsets are byte offsets and the measured length is exact.
fn ascii_line(len: usize) -> String {
    let seed = "You hit the hill giant very hard. The hill giant staggers backward. ";
    let mut line = seed.repeat(len / seed.len() + 1);
    line.truncate(len);
    assert!(line.is_ascii());
    line
}

/// A 16-color style cycling through the non-black ANSI foregrounds.
fn ansi_style(i: usize) -> Style {
    const COLORS: [AnsiColor; 6] = [
        AnsiColor::Red,
        AnsiColor::Green,
        AnsiColor::Yellow,
        AnsiColor::Blue,
        AnsiColor::Magenta,
        AnsiColor::Cyan,
    ];
    Style {
        fg: Color::Ansi {
            color: COLORS[i % 6],
            bold: i.is_multiple_of(2),
        },
        bg: Color::DefaultBackground,
    }
}

/// `n` spans tiling `[0, len)` contiguously — the non-overlapping, gap-free
/// shape `VtProcessor` emits (and the display renderer relies on).
fn tiling_spans(len: usize, n: usize) -> Vec<VtSpan> {
    (0..n)
        .map(|i| VtSpan {
            style: ansi_style(i),
            begin_pos: i * len / n,
            end_pos: (i + 1) * len / n,
        })
        .collect()
}

/// The raw wire form of `text` under `spans` — each span's slice prefixed
/// with a 16-color SGR, a trailing reset — which is what `buf_raw` holds when
/// a styled line commits (escapes kept, CR/LF excluded).
fn raw_for(text: &str, spans: &[VtSpan]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, span) in spans.iter().enumerate() {
        out.extend_from_slice(format!("\x1b[3{}m", 1 + (i % 6)).as_bytes());
        out.extend_from_slice(&text.as_bytes()[span.begin_pos..span.end_pos]);
    }
    out.extend_from_slice(b"\x1b[0m");
    out
}

/// Asserts the span-tiling invariant: spans cover `[0, text.len())`
/// contiguously — non-overlapping and gap-free. The renderer tiles the
/// on-screen line by slicing `text[span]` per span, so a violation corrupts
/// the display even when `.text` is right.
fn assert_spans_tile(line: &StyledLine) {
    let mut cursor = 0;
    for span in &line.spans {
        assert!(
            span.end_pos >= span.begin_pos,
            "inverted span {span:?} in {:?}",
            line.spans
        );
        assert_eq!(
            span.begin_pos, cursor,
            "gap/overlap before span {span:?} in {:?}",
            line.spans
        );
        cursor = span.end_pos;
    }
    assert_eq!(
        cursor,
        line.text.len(),
        "spans do not reach end of text: {:?}",
        line.spans
    );
}

/// Validates that the benches measure the real thing before any numbers are
/// trusted. Skippable via `SMUDGY_BENCH_SKIP_SANITY=1`.
///
/// - Telnet layer (`IacDense`): the dressed corpus must actually drive the
///   parser's IAC paths — one prompt per line, the expected subnegotiation
///   count, negotiation replies, and un-escaped `IAC IAC` literals emitted as
///   data.
/// - Full pipeline (`AnsiLight`): every dressed line commits exactly once;
///   line text survives the dress/strip round trip (modulo lines a chunk
///   boundary split into partial + tail — at most one per boundary); no ESC
///   or mangled 0xFF leaks into display text; every emitted line satisfies
///   the span-tiling invariant.
/// - SGR: the captured param lists drive `sgr::process` to the styles the
///   real `csi_dispatch` would produce.
#[allow(clippy::too_many_lines)]
fn sanity_check(lines: &[String]) {
    // -- Telnet layer against the IAC-dense corpus --
    let dressed = dress_lines(lines, WireProfile::IacDense);
    let mut parser = TelnetParser::new();
    let mut counts = CountingSink::default();
    let _ = parser.receive(&dressed, &mut counts);
    assert_eq!(
        counts.prompts,
        lines.len(),
        "IacDense sends IAC GA after every line"
    );
    assert_eq!(
        counts.subnegotiations,
        lines.len() / 25,
        "IacDense interleaves a subnegotiation every 25 lines"
    );
    assert!(
        counts.reply_bytes > 0,
        "the opening negotiation burst must produce replies"
    );
    // Literal-IAC insertion is probabilistic (~1-in-8 lines), so a heavily
    // truncated corpus (tiny SMUDGY_BENCH_LINES) can legitimately contain
    // none; only assert once the corpus is large enough that zero would mean
    // the escaping path itself is broken.
    if lines.len() >= 64 {
        assert!(
            counts.literal_ff_bytes > 0,
            "escaped IAC IAC literals must surface as 0xFF data bytes"
        );
    }

    // -- Full pipeline against the AnsiLight corpus --
    let dressed = dress_lines(lines, WireProfile::AnsiLight);
    let chunks = chunk(&dressed, CHUNK_LEN);
    let mut pipeline = Pipeline::new();
    pipeline.feed(&chunks);
    let (complete, partial) = pipeline.drain_lines();

    // Every LF commits exactly one complete line; prompts and chunk-boundary
    // flushes ride the partial-line path, so the counts separate cleanly.
    assert_eq!(
        complete.len(),
        lines.len(),
        "every dressed line must commit exactly once"
    );
    let prompts = lines.len() / 20;
    assert!(
        partial.len() >= prompts && partial.len() <= prompts + chunks.len(),
        "partial lines ({}) must be the {prompts} prompts plus at most one \
         chunk-boundary flush per chunk ({})",
        partial.len(),
        chunks.len()
    );

    // Text fidelity. VtProcessor drops control characters other than LF and
    // CR (`execute_c0_or_c1`; a bare CR followed by text restarts the open
    // line — carriage-return overprint), so for corpus lines, which carry no
    // bare CR, the expectation is the source line with controls filtered. A
    // line split by a chunk boundary emits its head as a partial and only the
    // tail as the complete line, so a bounded number of suffix-only matches
    // is tolerated.
    let mut boundary_split = 0_usize;
    for (line, source) in complete.iter().zip(lines) {
        let expected: String = source.chars().filter(|c| !c.is_control()).collect();
        if line.text == expected {
            continue;
        }
        assert!(
            expected.ends_with(line.text.as_str()),
            "line text corrupted: got {:?}, expected {expected:?}",
            line.text
        );
        boundary_split += 1;
    }
    assert!(
        boundary_split <= chunks.len(),
        "{boundary_split} split lines exceeds the {} chunk boundaries",
        chunks.len()
    );

    // No framing may leak into display text: ESC would mean SGR bytes escaped
    // the VT parser; U+FFFD would mean a raw 0xFF reached the UTF-8 decoder
    // (only asserted when the source corpus itself is clean of it).
    let corpus_clean = lines.iter().all(|l| !l.contains('\u{FFFD}'));
    for line in complete.iter().chain(partial.iter()) {
        assert!(
            !line.text.bytes().any(|b| b == 0x1b),
            "ESC leaked into display text: {:?}",
            line.text
        );
        if corpus_clean {
            assert!(
                !line.text.contains('\u{FFFD}'),
                "replacement char (mangled raw byte) in display text: {:?}",
                line.text
            );
        }
        assert_spans_tile(line);
    }

    // -- SGR: captured params must reproduce the real style transitions --
    let red = |bold| Color::Ansi {
        color: AnsiColor::Red,
        bold,
    };
    assert_eq!(
        sgr_process(default_style(), &parse_sgr_params(b"\x1b[31m")).fg,
        red(false)
    );
    assert_eq!(
        sgr_process(default_style(), &parse_sgr_params(b"\x1b[1;31m")).fg,
        red(true)
    );
    assert_eq!(
        sgr_process(default_style(), &parse_sgr_params(b"\x1b[38;2;1;2;3m")).fg,
        Color::Rgb { r: 1, g: 2, b: 3 }
    );
    assert!(matches!(
        sgr_process(default_style(), &parse_sgr_params(b"\x1b[38;5;196m")).fg,
        Color::Rgb { .. }
    ));
    let styled = Style {
        fg: red(true),
        bg: Color::DefaultBackground,
    };
    assert_eq!(
        sgr_process(styled, &parse_sgr_params(b"\x1b[0m")),
        default_style()
    );
    // Backgrounds apply, and a directive with no Style representation skips
    // only itself — co-located colors still land.
    assert_eq!(
        sgr_process(default_style(), &parse_sgr_params(b"\x1b[41m")).bg,
        red(false)
    );
    assert_eq!(
        sgr_process(default_style(), &parse_sgr_params(b"\x1b[1;4;31m")).fg,
        red(true)
    );

    eprintln!(
        "sanity: telnet decoded {} data bytes / {} prompts / {} subnegotiations; \
         pipeline committed {} complete + {} partial lines over {} chunks, \
         {boundary_split} split by chunk boundaries; span tiling + SGR transitions verified",
        counts.data_bytes,
        counts.prompts,
        counts.subnegotiations,
        complete.len(),
        partial.len(),
        chunks.len()
    );
}

/// `TelnetParser::receive` over the whole dressed corpus with a no-op sink:
/// the cost of the IAC layer alone. `ansi_light` is the near-pure-data case
/// (one memchr scan per read, IAC only at prompts); `iac_dense` pays the
/// control-sequence state machine on every line.
fn bench_telnet_receive(c: &mut Criterion, lines: &[String]) {
    let mut group = c.benchmark_group("telnet_receive");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for (name, profile) in [
        ("ansi_light", WireProfile::AnsiLight),
        ("iac_dense", WireProfile::IacDense),
    ] {
        let dressed = dress_lines(lines, profile);
        eprintln!("  telnet_receive/{name}: {} wire bytes", dressed.len());
        let mut parser = TelnetParser::new();
        let mut sink = NullSink;
        // Warmup pass: settles accepted-option negotiation. The few refused
        // options in the opening burst still draw a tiny, constant refusal
        // reply on every pass (the parser records no state for refusals) —
        // the same traffic a re-asserting server would generate.
        let _ = parser.receive(&dressed, &mut sink);
        group.throughput(Throughput::Bytes(dressed.len() as u64));
        group.bench_function(name, |b| {
            b.iter(|| black_box(parser.receive(&dressed, &mut sink)));
        });
    }
    group.finish();
}

/// The full composed ingest path, per profile: chunked `feed_inbound` +
/// `notify_end_of_buffer` mirroring the read loop, with the runtime channel
/// drained (lines counted, `black_box`ed, dropped) inside the timed pass.
/// Throughput is raw dressed wire bytes — the number that maps directly to
/// socket goodput.
fn bench_ingest_pipeline(c: &mut Criterion, lines: &[String]) {
    let mut group = c.benchmark_group("ingest_pipeline");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for (name, profile) in [
        ("ansi_light", WireProfile::AnsiLight),
        ("ansi_heavy", WireProfile::AnsiHeavy),
        ("iac_dense", WireProfile::IacDense),
    ] {
        let dressed = dress_lines(lines, profile);
        let chunks = chunk(&dressed, CHUNK_LEN);
        let mut pipeline = Pipeline::new();
        // Pre-measured pass: settles accepted-option negotiation (refused
        // options in the opening burst still cost a constant few reply bytes
        // per pass), proves lines actually flow, and reports an absolute
        // line rate alongside criterion's bytes/sec.
        let started = Instant::now();
        pipeline.feed(&chunks);
        let (complete, partial) = pipeline.drain_counts();
        let elapsed = started.elapsed();
        assert!(complete > 0, "pipeline emitted no complete lines");
        #[allow(clippy::cast_precision_loss)]
        let rate = complete as f64 / elapsed.as_secs_f64();
        eprintln!(
            "  ingest_pipeline/{name}: {} wire bytes in {} chunks; \
             {complete} complete + {partial} partial lines in {elapsed:.2?} (~{rate:.0} lines/sec)",
            dressed.len(),
            chunks.len()
        );
        group.throughput(Throughput::Bytes(dressed.len() as u64));
        group.bench_function(name, |b| {
            b.iter(|| {
                pipeline.feed(&chunks);
                black_box(pipeline.drain_counts())
            });
        });

        // The same pass with raw capture gated off — the shipped default for
        // profiles with no raw-pattern trigger (the connection latches the
        // trigger manager's raw-wanted flag per line).
        let mut pipeline = Pipeline::new();
        let raw_off = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        pipeline.vt_processor.set_raw_wanted_flag(raw_off);
        group.bench_function(BenchmarkId::new(name, "no_raw"), |b| {
            b.iter(|| {
                pipeline.feed(&chunks);
                black_box(pipeline.drain_counts())
            });
        });
    }
    group.finish();
}

/// `StyledLine::new_with_raw` over representative single lines. This is the
/// per-committed-line constant: it copies `text`, re-validates `buf_raw` via
/// `from_utf8_lossy`, and (in the shipped flow) receives a freshly collected
/// span vec — the per-iteration `spans.clone()` stands in for the
/// `span_info.drain(..).collect()` allocation `consume_into_pending_line`
/// performs. Throughput counts text + raw bytes, the payload actually copied.
fn bench_styled_line(c: &mut Criterion) {
    let short_text = ascii_line(40);
    let long_text = ascii_line(200);
    let styled_spans = tiling_spans(long_text.len(), 12);
    let styled_raw = raw_for(&long_text, &styled_spans);
    let cases: [(&str, &str, Vec<VtSpan>, Vec<u8>); 3] = [
        (
            "short_plain",
            &short_text,
            tiling_spans(short_text.len(), 1),
            short_text.as_bytes().to_vec(),
        ),
        (
            "long_plain",
            &long_text,
            tiling_spans(long_text.len(), 1),
            long_text.as_bytes().to_vec(),
        ),
        ("long_styled", &long_text, styled_spans, styled_raw),
    ];

    let mut group = c.benchmark_group("styled_line");
    for (name, text, spans, raw) in &cases {
        eprintln!(
            "  styled_line/new_with_raw/{name}: {} text bytes, {} spans, {} raw bytes",
            text.len(),
            spans.len(),
            raw.len()
        );
        group.throughput(Throughput::Bytes((text.len() + raw.len()) as u64));
        group.bench_function(BenchmarkId::new("new_with_raw", *name), |b| {
            b.iter(|| {
                black_box(StyledLine::new_with_raw(
                    black_box(text),
                    spans.clone(),
                    Some(raw),
                ))
            });
        });
        // The raw-capture-off form — what every line costs for profiles with no
        // raw-pattern triggers (the common case). Throughput stays text-only.
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_function(BenchmarkId::new("new_no_raw", *name), |b| {
            b.iter(|| {
                black_box(StyledLine::new_with_raw(
                    black_box(text),
                    spans.clone(),
                    None,
                ))
            });
        });
    }
    group.finish();
}

/// `sgr::process` folding one captured parameter list per iteration — the
/// style-transition cost `csi_dispatch` pays for every `CSI … m` in the
/// stream (6-10 per line under heavy styling).
fn bench_sgr(c: &mut Criterion) {
    let sequences: Vec<(&str, Vec<CsiParam>)> = SGR_SEQUENCES
        .iter()
        .map(|&(name, bytes)| (name, parse_sgr_params(bytes)))
        .collect();
    let initial = default_style();

    let mut group = c.benchmark_group("sgr");
    group.throughput(Throughput::Elements(1));
    for (name, params) in &sequences {
        group.bench_function(BenchmarkId::new("process", *name), |b| {
            b.iter(|| {
                black_box(sgr_process(
                    black_box(initial),
                    black_box(params.as_slice()),
                ))
            });
        });
    }
    group.finish();
}

fn ingest(c: &mut Criterion) {
    // First log in `bench/logs/` (name-sorted, so deterministic): one corpus
    // is enough here because the wire profiles, not the prose, are the axis
    // under test.
    let (corpus_name, lines) = log_corpora()
        .into_iter()
        .next()
        .expect("bench/logs has at least one log file");
    let corpus_bytes: u64 = lines.iter().map(|l| l.len() as u64).sum();
    eprintln!(
        "corpus {corpus_name}: {} lines / {corpus_bytes} bytes of display text",
        lines.len()
    );

    if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
        sanity_check(&lines);
    }

    bench_telnet_receive(c, &lines);
    bench_ingest_pipeline(c, &lines);
    bench_styled_line(c);
    bench_sgr(c);
}

criterion_group!(benches, ingest);
criterion_main!(benches);
