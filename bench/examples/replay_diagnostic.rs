//! Headless replay diagnostic. Inputs stay outside the source tree.
//! Usage: `replay_diagnostic <ingest|manager|session> <wire-log> <spec-or-module> <passes>`
//! Manager specs are TSV: name, optional ANSI foreground index, then regex patterns.
//! Use `10k` for the existing engine benchmark's trigger population.
//! Session modules must echo ZZREPLAYREADY after ZZREPLAYCHECK and ZZREPLAYDONE after ZZREPLAYEND.

use std::{fs, hint::black_box, sync::Arc, time::Instant};

use smudgy_bench::session::{BenchSession, bench_runtime, styled};
use smudgy_core::{
    models::matchers::{
        MatcherColor, MatcherColorMatch, MatcherRole, MatcherSyntax, TriggerMatcherSource,
    },
    session::{
        connection::{
            feed_inbound,
            responders::{DEFAULT_DIMS, ProtocolState},
            telnet::TelnetParser,
            transcode::Transcode,
            vt_processor::VtProcessor,
        },
        runtime::{
            BenchActionQueue, IsolateId, Manager, Origin, PushTriggerParams, RuntimeAction,
            ScriptAction, SharedAutomationRegistry,
        },
        styled_line::StyledLine,
    },
};
use tokio::sync::mpsc::unbounded_channel;
use vtparse::VTParser;

#[cfg(feature = "ingest-allocations")]
#[global_allocator]
static ALLOC: smudgy_bench::alloc::CountingAllocator = smudgy_bench::alloc::CountingAllocator;

#[inline(never)]
fn parse(bytes: &[u8]) -> Vec<Arc<StyledLine>> {
    let (tx, mut rx) = unbounded_channel();
    let mut vt = VtProcessor::new(tx.clone());
    vt.set_raw_wanted_flag(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let mut parser = VTParser::new();
    let mut telnet = TelnetParser::new();
    let mut protocol = ProtocolState::with_fixed_dims(DEFAULT_DIMS);
    let mut transcode = Transcode::default();
    let mut replies = Vec::new();
    // Preserve complete logical lines for a matched-input session/manager comparison.
    // Read chunking is real, but provisional display flushes are excluded here.
    for data in bytes.chunks(16 * 1024) {
        let _ = feed_inbound(
            data,
            &mut telnet,
            &mut parser,
            &mut vt,
            &mut replies,
            &tx,
            &mut protocol,
            &mut transcode,
        );
    }
    vt.notify_end_of_buffer();
    let mut lines = Vec::new();
    while let Ok(action) = rx.try_recv() {
        match action {
            RuntimeAction::HandleIncomingLine(line) => lines.push(line),
            RuntimeAction::HandleIncomingFragmentedLine { .. }
            | RuntimeAction::HandleIncomingPartialLine(_) => {
                panic!("diagnostic requires newline-terminated complete input")
            }
            _ => (),
        }
    }
    lines
}

fn push(manager: &mut Manager, name: &str, patterns: Vec<String>, color: Option<u8>) {
    let name = Arc::new(name.to_owned());
    let matchers: Vec<_> = patterns
        .iter()
        .map(|source| TriggerMatcherSource {
            role: MatcherRole::Match,
            syntax: MatcherSyntax::Regex,
            source: source.clone(),
            anchor_start: true,
            anchor_end: true,
            color: color.map(|index| MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index }),
                ..Default::default()
            }),
        })
        .collect();
    let patterns = Arc::new(patterns);
    let empty = Arc::new(Vec::new());
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
            fallthrough: true,
            fire_limit: None,
            line_limit: None,
            source: None,
        })
        .expect("register trigger");
}

fn manager(spec: &str) -> (Manager, BenchActionQueue) {
    let (mut manager, queue) = Manager::new_for_bench(
        Arc::new(";".to_owned()),
        SharedAutomationRegistry::default(),
    );
    if spec == "10k" {
        for (i, name) in smudgy_bench::load_item_names_10k().iter().enumerate() {
            push(
                &mut manager,
                &format!("item_{i}"),
                vec![regex::escape(name)],
                None,
            );
        }
        for (i, pattern) in smudgy_bench::REGEX_TRIGGERS.iter().enumerate() {
            push(
                &mut manager,
                &format!("regex_{i}"),
                vec![(*pattern).to_owned()],
                None,
            );
        }
    } else {
        for row in fs::read_to_string(spec).expect("read spec").lines() {
            let mut columns = row.split('\t');
            let name = columns.next().expect("name");
            let color = columns.next().expect("color");
            push(
                &mut manager,
                name,
                columns.map(str::to_owned).collect(),
                if color.is_empty() {
                    None
                } else {
                    Some(color.parse().expect("ANSI index"))
                },
            );
        }
    }
    manager
        .process_incoming_line(&styled("warmup"))
        .expect("compile trigger sets");
    queue.clear();
    (manager, queue)
}

#[inline(never)]
fn scan(manager: &mut Manager, queue: &BenchActionQueue, lines: &[Arc<StyledLine>]) -> usize {
    for line in lines {
        manager
            .process_incoming_line(black_box(line))
            .expect("match line");
    }
    let count = queue.len();
    queue.clear();
    count
}

#[allow(
    clippy::too_many_lines,
    reason = "keep each diagnostic mode and its measurement boundaries together"
)]
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 5, "mode corpus spec/module passes");
    let mode = &args[1];
    let bytes = fs::read(&args[2]).expect("read corpus");
    let passes: usize = args[4].parse().expect("passes");
    let lines = parse(&bytes);
    println!(
        "{{\"mode\":\"{mode}\",\"bytes\":{},\"lines\":{},\"spans\":{}}}",
        bytes.len(),
        lines.len(),
        lines.iter().map(|l| l.spans.len()).sum::<usize>()
    );
    match mode.as_str() {
        "ingest" => {
            for pass in 0..passes + 2 {
                let start = Instant::now();
                let parsed = parse(black_box(&bytes));
                assert_eq!(parsed.len(), lines.len());
                drop(parsed);
                println!("{{\"pass\":{pass},\"ns\":{}}}", start.elapsed().as_nanos());
            }
        }
        "manager" => {
            #[cfg(feature = "ingest-allocations")]
            let registration = smudgy_bench::alloc::snapshot();
            let (mut manager, queue) = manager(&args[3]);
            #[cfg(feature = "ingest-allocations")]
            {
                let delta = smudgy_bench::alloc::since(registration);
                println!(
                    "{{\"registration_allocations\":{},\"registration_bytes\":{},\"action_size\":{}}}",
                    delta.count,
                    delta.bytes,
                    std::mem::size_of::<RuntimeAction>()
                );
            }
            let expected = scan(&mut manager, &queue, &lines);
            for pass in 0..passes + 2 {
                #[cfg(feature = "ingest-allocations")]
                let allocations = smudgy_bench::alloc::snapshot();
                let start = Instant::now();
                let actions = scan(&mut manager, &queue, &lines);
                let elapsed = start.elapsed();
                #[cfg(feature = "ingest-allocations")]
                {
                    let delta = smudgy_bench::alloc::since(allocations);
                    println!(
                        "{{\"alloc_pass\":{pass},\"allocations\":{},\"bytes\":{}}}",
                        delta.count, delta.bytes
                    );
                }
                assert_eq!(actions, expected);
                println!(
                    "{{\"pass\":{pass},\"ns\":{},\"actions\":{actions}}}",
                    elapsed.as_nanos()
                );
            }
        }
        "session" => {
            let runtime = bench_runtime();
            let module = fs::read_to_string(&args[3]).expect("read module");
            let mut session = BenchSession::start(
                &runtime,
                "ReplayDiagnostic",
                9711,
                &[("replay.ts", module)],
                &[],
            );
            session.feed(&styled("ZZREPLAYCHECK"));
            let mut transcript = Vec::new();
            assert!(
                runtime.block_on(session.drain_collect_until("ZZREPLAYREADY", &mut transcript)),
                "module startup failed: {transcript:?}"
            );
            for pass in 0..passes + 2 {
                session.drain_stragglers();
                let mut batch = Vec::with_capacity(lines.len() + 3);
                batch.push(styled("--- BEGIN MEASURING ---"));
                batch.extend(lines.iter().cloned());
                batch.push(styled("--- END MEASURING ---"));
                batch.push(styled("ZZREPLAYEND"));
                if pass == 0 {
                    transcript.clear();
                    for line in &batch {
                        session.feed(line);
                    }
                    assert!(
                        runtime
                            .block_on(session.drain_collect_until("ZZREPLAYDONE", &mut transcript)),
                        "replay timed out"
                    );
                    for text in transcript.iter().filter(|text| {
                        text.starts_with('|')
                            || text.starts_with("ZZCOUNTS")
                            || text.contains("Error")
                    }) {
                        eprintln!("{text}");
                    }
                } else {
                    let elapsed = runtime.block_on(session.timed_pass(&batch, "ZZREPLAYDONE"));
                    println!("{{\"pass\":{pass},\"ns\":{}}}", elapsed.as_nanos());
                }
            }
        }
        _ => panic!("unknown mode"),
    }
}
