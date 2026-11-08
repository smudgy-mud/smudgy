//! Deterministic Callgrind counters for the synchronous socket-ingest CPU
//! path. This deliberately complements, rather than replaces, Criterion:
//! instruction/cache counts cannot represent readiness scheduling, TLS,
//! allocator contention, or end-to-end latency.

#[cfg(target_os = "linux")]
mod linux {
    use std::hint::black_box;

    use gungraun::prelude::*;
    use smudgy_bench::graphics::{DecodeQueue, enable_full_graphics};
    use smudgy_bench::wire::{WireProfile, chunk, dress_lines};
    use smudgy_core::session::{
        connection::{
            feed_inbound,
            responders::{DEFAULT_DIMS, ProtocolState},
            telnet::{TelnetParser, TelnetSink},
            transcode::Transcode,
            vt_processor::VtProcessor,
        },
        runtime::RuntimeAction,
    };
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use vtparse::VTParser;

    const CHUNK_LEN: usize = 4 * 1024;

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

    struct Pipeline {
        telnet: TelnetParser,
        vt_parser: VTParser,
        vt_processor: VtProcessor,
        replies: Vec<u8>,
        runtime_tx: tokio::sync::mpsc::UnboundedSender<RuntimeAction>,
        rx: UnboundedReceiver<RuntimeAction>,
        protocol: ProtocolState,
        transcode: Transcode,
        /// The stub graphics host's pending decodes, when the processor runs
        /// the full graphics protocol.
        decodes: Option<DecodeQueue>,
    }

    impl Pipeline {
        fn new(full_graphics: bool) -> Self {
            let (runtime_tx, rx) = unbounded_channel();
            let mut vt_processor = VtProcessor::new(runtime_tx.clone());
            let decodes = full_graphics.then(|| enable_full_graphics(&mut vt_processor));
            Self {
                telnet: TelnetParser::new(),
                vt_parser: VTParser::new(),
                vt_processor,
                replies: Vec::new(),
                runtime_tx,
                rx,
                protocol: ProtocolState::with_fixed_dims(DEFAULT_DIMS),
                transcode: Transcode::default(),
                decodes,
            }
        }

        fn feed(mut self, bytes: &[u8]) -> (usize, usize) {
            for data in chunk(bytes, CHUNK_LEN) {
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
                // Between batches, as the connection's `select!` would:
                // decodes land and the replies they settled are written.
                if let Some(decodes) = &mut self.decodes {
                    decodes.complete_all(&mut self.vt_processor);
                    self.replies.clear();
                    self.vt_processor.drain_replies(&mut self.replies);
                    black_box(self.replies.as_slice());
                }
            }

            let mut complete = 0;
            let mut partial = 0;
            while let Ok(action) = self.rx.try_recv() {
                match action {
                    RuntimeAction::HandleIncomingLine(line) => {
                        complete += 1;
                        black_box(line);
                    }
                    RuntimeAction::HandleIncomingFragmentedLine { line, .. } => {
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
    }

    fn lines() -> Vec<String> {
        (0..64)
            .map(|index| {
                format!("The training construct number {index} turns, attacks, and reports status.")
            })
            .collect()
    }

    fn setup_wire(profile: WireProfile) -> Vec<u8> {
        dress_lines(&lines(), profile)
    }

    fn ansi_light() -> Vec<u8> {
        setup_wire(WireProfile::AnsiLight)
    }

    fn ansi_heavy() -> Vec<u8> {
        setup_wire(WireProfile::AnsiHeavy)
    }

    fn iac_dense() -> Vec<u8> {
        setup_wire(WireProfile::IacDense)
    }

    fn ansi_graphics() -> Vec<u8> {
        setup_wire(WireProfile::AnsiGraphics)
    }

    fn ansi_graphics_full() -> Vec<u8> {
        setup_wire(WireProfile::AnsiGraphicsFull)
    }

    #[library_benchmark]
    #[bench::ansi_light(ansi_light())]
    #[bench::iac_dense(iac_dense())]
    fn telnet_receive(bytes: Vec<u8>) {
        let mut parser = TelnetParser::new();
        let mut sink = NullSink;
        black_box(parser.receive(black_box(&bytes), &mut sink));
    }

    // `full_graphics` runs the processor in `GraphicsMode::Full` with the
    // stub host (`ansi_graphics_full`); every other profile leaves graphics
    // off, as a processor without a server configuration does. (Gungraun's
    // attribute macro takes no doc comment.)
    #[library_benchmark]
    #[bench::ansi_light(ansi_light(), false)]
    #[bench::ansi_heavy(ansi_heavy(), false)]
    #[bench::iac_dense(iac_dense(), false)]
    #[bench::ansi_graphics(ansi_graphics(), false)]
    #[bench::ansi_graphics_full(ansi_graphics_full(), true)]
    fn ingest_pipeline(bytes: Vec<u8>, full_graphics: bool) -> (usize, usize) {
        black_box(Pipeline::new(full_graphics).feed(black_box(&bytes)))
    }

    library_benchmark_group!(
        name = ingest,
        benchmarks = [telnet_receive, ingest_pipeline]
    );

    gungraun::main!(library_benchmark_groups = ingest);

    pub fn run() {
        main();
    }
}

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ingest_callgrind requires Linux and Valgrind");
}
