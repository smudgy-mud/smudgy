//! Ingest timing, allocation, and sampling-profiler driver. Run `--help` for arguments.
//! The concurrent consumer observes and drops real `RuntimeAction`s on another thread. It
//! measures channel contention and object lifetime, not scripting, rendering, or socket I/O.
//! Build separately with `ingest-allocations` for allocation counts; leave it off for timing.

use std::{hint::black_box, sync::Arc, time::Instant};

use smudgy_bench::{
    load_log_lines,
    wire::{WireProfile, dress_lines},
};
use smudgy_core::session::{
    connection::{
        feed_inbound, feed_utf8,
        responders::{DEFAULT_DIMS, ProtocolState},
        telnet::TelnetParser,
        transcode::Transcode,
        vt_processor::VtProcessor,
    },
    runtime::RuntimeAction,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use vtparse::VTParser;

#[cfg(feature = "ingest-allocations")]
#[global_allocator]
static ALLOC: smudgy_bench::alloc::CountingAllocator = smudgy_bench::alloc::CountingAllocator;

const BARRIER: u64 = u64::MAX;

struct Options {
    case: String,
    concurrent: bool,
    raw: bool,
    vt_only: bool,
    lines: usize,
    size: usize,
    read: usize,
    batch: usize,
    passes: usize,
}

impl Options {
    fn parse() -> Self {
        let mut result = Self {
            case: "light".into(),
            concurrent: false,
            raw: false,
            vt_only: false,
            lines: 30_000,
            size: 16_384,
            read: 16_384,
            batch: 512 * 1024,
            passes: 20,
        };
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            if key == "--help" {
                println!(
                    "--case light|heavy|plain|unicode|invalid|mixed_invalid|c1\n\
                    --mode queued|concurrent --raw on|off --layer ingest|vt\n\
                    --lines N (ordinary fixtures) --size N (adversarial fixtures)\n\
                    --read N --batch N --passes N\n\
                    JSON lines: setup record, then per-pass wall/stage times and counts.\n\
                    Use a separate --features ingest-allocations build to count allocations."
                );
                std::process::exit(0);
            }
            let value = args.next().expect("each option needs a value");
            match key.as_str() {
                "--case" => result.case = value,
                "--mode" => {
                    result.concurrent = match value.as_str() {
                        "concurrent" => true,
                        "queued" => false,
                        _ => panic!("invalid mode"),
                    }
                }
                "--raw" => {
                    result.raw = match value.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => panic!("invalid raw setting"),
                    }
                }
                "--layer" => {
                    result.vt_only = match value.as_str() {
                        "vt" => true,
                        "ingest" => false,
                        _ => panic!("invalid layer"),
                    }
                }
                "--lines" => result.lines = value.parse().expect("integer lines"),
                "--size" => result.size = value.parse().expect("integer size"),
                "--read" => result.read = value.parse().expect("integer read size"),
                "--batch" => result.batch = value.parse().expect("integer batch size"),
                "--passes" => result.passes = value.parse().expect("integer passes"),
                _ => panic!("unknown option {key}"),
            }
        }
        assert!(result.read > 0 && result.batch > 0 && result.passes > 0);
        result
    }

    fn fixture(&self) -> Vec<u8> {
        match self.case.as_str() {
            "light" | "heavy" | "plain" => {
                let mut lines = load_log_lines();
                lines.truncate(self.lines);
                if self.case == "plain" {
                    let mut bytes = lines.join("\n").into_bytes();
                    bytes.push(b'\n');
                    bytes
                } else {
                    dress_lines(
                        &lines,
                        if self.case == "light" {
                            WireProfile::AnsiLight
                        } else {
                            WireProfile::AnsiHeavy
                        },
                    )
                }
            }
            "unicode" => "café 你好 🙂 — ordinary Unicode text without styling\n"
                .repeat(self.lines)
                .into_bytes(),
            "invalid" | "mixed_invalid" | "c1" => {
                let seed: &[u8] = match self.case.as_str() {
                    "invalid" => b"\xfe",
                    "mixed_invalid" => b"x\xfe",
                    _ => b"\xc2\x85",
                };
                let mut bytes = seed.repeat(self.size.div_ceil(seed.len()));
                bytes.truncate(self.size);
                bytes.extend_from_slice(b"\nrecovered\n");
                bytes
            }
            _ => panic!("unknown fixture"),
        }
    }
}

struct Producer {
    telnet: TelnetParser,
    parser: VTParser,
    processor: VtProcessor,
    replies: Vec<u8>,
    tx: UnboundedSender<RuntimeAction>,
    protocol: ProtocolState,
    transcode: Transcode,
}

impl Producer {
    fn new(tx: UnboundedSender<RuntimeAction>, raw: bool) -> Self {
        let mut processor = VtProcessor::new(tx.clone());
        processor.set_raw_wanted_flag(Arc::new(std::sync::atomic::AtomicBool::new(raw)));
        Self {
            telnet: TelnetParser::new(),
            parser: VTParser::new(),
            processor,
            replies: Vec::new(),
            tx,
            protocol: ProtocolState::with_fixed_dims(DEFAULT_DIMS),
            transcode: Transcode::default(),
        }
    }

    // Keep a named frame for sampling profilers even under release optimization.
    #[inline(never)]
    fn feed(&mut self, bytes: &[u8], options: &Options) {
        let mut pending = 0;
        for data in bytes.chunks(options.read) {
            if options.vt_only {
                feed_utf8(&mut self.parser, &mut self.processor, data);
            } else {
                let _ = feed_inbound(
                    data,
                    &mut self.telnet,
                    &mut self.parser,
                    &mut self.processor,
                    &mut self.replies,
                    &self.tx,
                    &mut self.protocol,
                    &mut self.transcode,
                );
                black_box(&self.replies);
            }
            pending += data.len();
            if pending >= options.batch {
                self.processor.notify_end_of_buffer();
                pending = 0;
            }
        }
        if pending > 0 {
            self.processor.notify_end_of_buffer();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    lines: usize,
    partials: usize,
    text: usize,
    raw: usize,
    spans: usize,
}

impl Counts {
    fn observe(&mut self, action: RuntimeAction) -> bool {
        let line = match action {
            RuntimeAction::HandleIncomingLine(line)
            | RuntimeAction::HandleIncomingFragmentedLine { line, .. } => {
                self.lines += 1;
                line
            }
            RuntimeAction::HandleIncomingPartialLine(line) => {
                self.partials += 1;
                line
            }
            RuntimeAction::IncomingPacketProcessed {
                connection_generation: BARRIER,
                ..
            } => return true,
            _ => return false,
        };
        self.text += line.text.len();
        self.raw += line.raw().map_or(0, str::len);
        self.spans += line.spans.len();
        black_box(line);
        false
    }
}

#[inline(never)]
fn drain(rx: &mut UnboundedReceiver<RuntimeAction>) -> Counts {
    let mut counts = Counts::default();
    while let Ok(action) = rx.try_recv() {
        counts.observe(action);
    }
    counts
}

fn fixture_bytes(options: &Options) -> Vec<u8> {
    let bytes = options.fixture();
    // Telnet stripping is deliberately outside a VT-only measurement. These fixture
    // generators emit no literal FF and use only IAC GA, including across read boundaries.
    if options.vt_only {
        bytes
            .into_iter()
            .filter(|&byte| byte != 0xff && byte != 0xf9)
            .collect()
    } else {
        bytes
    }
}

fn main() {
    let options = Options::parse();
    let bytes = fixture_bytes(&options);
    let (tx, rx) = unbounded_channel();
    let mut producer = Producer::new(tx, options.raw);
    let mut queued = Some(rx);
    let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(0);
    let consumer = if options.concurrent {
        let mut rx = queued.take().expect("receiver");
        Some(
            std::thread::Builder::new()
                .name("ingest-consumer".into())
                .spawn(move || {
                    let mut counts = Counts::default();
                    while let Some(action) = rx.blocking_recv() {
                        if counts.observe(action) {
                            if ack_tx.send(counts).is_err() {
                                break;
                            }
                            counts = Counts::default();
                        }
                    }
                })
                .expect("consumer thread"),
        )
    } else {
        None
    };

    println!(
        "{{\"case\":\"{}\",\"concurrent\":{},\"raw\":{},\"vt_only\":{},\"bytes\":{},\"read\":{},\"batch\":{},\"allocation_build\":{}}}",
        options.case,
        options.concurrent,
        options.raw,
        options.vt_only,
        bytes.len(),
        options.read,
        options.batch,
        cfg!(feature = "ingest-allocations")
    );
    let mut expected = None;
    // Two complete warmup passes settle allocations and synchronization before measurement.
    for pass in 0..options.passes + 2 {
        #[cfg(feature = "ingest-allocations")]
        let allocation_start = smudgy_bench::alloc::snapshot();
        let start = Instant::now();
        producer.feed(black_box(&bytes), &options);
        let feed_time = start.elapsed();
        let counts = if let Some(rx) = &mut queued {
            drain(rx)
        } else {
            producer
                .tx
                .send(RuntimeAction::IncomingPacketProcessed {
                    connection_generation: BARRIER,
                    has_displayable_text: false,
                })
                .expect("consumer alive");
            ack_rx.recv().expect("consumer acknowledgement")
        };
        let elapsed = start.elapsed();
        #[cfg(feature = "ingest-allocations")]
        let allocations = smudgy_bench::alloc::since(allocation_start);
        if let Some(previous) = expected {
            assert_eq!(counts, previous);
        }
        expected = Some(counts);
        assert!(counts.lines > 0, "fixture must commit a line");
        if !options.raw {
            assert_eq!(counts.raw, 0);
        }
        if pass >= 2 {
            #[cfg(feature = "ingest-allocations")]
            let (alloc_count, alloc_bytes) = (allocations.count, allocations.bytes);
            #[cfg(not(feature = "ingest-allocations"))]
            let (alloc_count, alloc_bytes) = (0, 0);
            println!(
                "{{\"pass\":{},\"total_ns\":{},\"producer_ns\":{},\"drain_wait_ns\":{},\"lines\":{},\"partials\":{},\"text_bytes\":{},\"raw_bytes\":{},\"spans\":{},\"alloc_count\":{},\"alloc_bytes\":{}}}",
                pass - 2,
                elapsed.as_nanos(),
                feed_time.as_nanos(),
                elapsed.saturating_sub(feed_time).as_nanos(),
                counts.lines,
                counts.partials,
                counts.text,
                counts.raw,
                counts.spans,
                alloc_count,
                alloc_bytes
            );
        }
    }
    drop(producer);
    if let Some(consumer) = consumer {
        consumer.join().expect("consumer finished");
    }
}
