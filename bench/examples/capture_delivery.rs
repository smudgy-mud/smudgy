//! Compare capture delivery through function arguments, classic scripts, and templates.
//! Usage: `capture_delivery <function|script|template> <literal|groups|wide> <passes> <lines>`
//! Synthetic inputs only. Outgoing commands are consumed by a disconnected local session.

use std::{fmt::Write as _, sync::Arc};

#[cfg(feature = "ingest-allocations")]
#[global_allocator]
static ALLOC: smudgy_bench::alloc::CountingAllocator = smudgy_bench::alloc::CountingAllocator;

use smudgy_bench::session::{BenchSession, bench_runtime, styled};
use smudgy_core::{
    models::{ScriptLang, triggers::TriggerDefinition},
    session::runtime::{IsolateId, Origin, RuntimeAction},
};

#[allow(
    clippy::too_many_lines,
    reason = "linear diagnostic setup keeps untimed verification separate from timed passes"
)]
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 5, "see module usage");
    let consumer = args[1].as_str();
    let shape = args[2].as_str();
    let passes: usize = args[3].parse().unwrap();
    let count: usize = args[4].parse().unwrap();
    let (pattern, input, expression, template, output, contribution) = match shape {
        "literal" => (
            "needle".to_string(),
            "find needle twice: needle".to_string(),
            "m[0].length",
            "OUT:$0",
            "OUT:needle",
            6,
        ),
        "groups" => (
            r"^CAP (?<word>é+) (\d+)(?: (x))?$".to_string(),
            "CAP éé 42".to_string(),
            "m.word.length + m[2].length + m[3].length",
            "OUT:$word:$2:$3",
            "OUT:éé:42:",
            4,
        ),
        "wide" => {
            let mut pattern = String::from("^CAP ");
            for index in 0..40 {
                write!(pattern, "(?<g{index}>x)").unwrap();
            }
            pattern.push('$');
            (
                pattern,
                format!("CAP {}", "x".repeat(40)),
                "m.g39.length + m[40].length",
                "OUT:${g39}:${40}",
                "OUT:x:x",
                2,
            )
        }
        _ => panic!("unknown shape"),
    };
    assert!(matches!(consumer, "function" | "script" | "template"));
    let expected = if consumer == "template" {
        0
    } else {
        count * contribution
    };
    // These fixed patterns contain only printable text and regex backslashes.
    let pattern_json = format!("{pattern:?}");
    let body = format!("globalThis.captureSum += {expression};");
    let registration = if consumer == "function" {
        format!("createTrigger({pattern_json}, (m) => {{ {body} }});")
    } else {
        String::new()
    };
    let module = format!(
        r#"
import {{ createTrigger, echo }} from "smudgy:core";
globalThis.captureSum = 0;
{registration}
createTrigger("^ZZCAPTURECHECK$", () => echo("ZZCAPTUREREADY"));
createTrigger("^ZZCAPTUREEND$", () => {{
    const got = globalThis.captureSum;
    globalThis.captureSum = 0;
    echo(got === {expected} ? "ZZCAPTUREDONE" : `ZZCAPTUREFAIL:${{got}}`);
}});
"#
    );
    let runtime = bench_runtime();
    let mut session = BenchSession::start(
        &runtime,
        "CaptureDelivery",
        9720,
        &[("capture.ts", module)],
        &[],
    );
    session.feed(&styled("ZZCAPTURECHECK"));
    let mut transcript = Vec::new();
    assert!(
        runtime.block_on(session.drain_collect_until("ZZCAPTUREREADY", &mut transcript)),
        "{transcript:?}"
    );
    if consumer != "function" {
        session.dispatch(RuntimeAction::AddTrigger {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: Arc::new("capture delivery".to_string()),
            trigger: TriggerDefinition {
                patterns: Some(vec![pattern]),
                language: if consumer == "script" {
                    ScriptLang::JS
                } else {
                    ScriptLang::Plaintext
                },
                script: Some(if consumer == "script" {
                    format!("{{ const m = matches; {body} }}; void 0;")
                } else {
                    template.to_string()
                }),
                ..TriggerDefinition::default()
            },
            fire_limit: None,
            line_limit: None,
        });
    }
    let input = styled(&input);
    let mut lines = vec![input; count];
    lines.push(styled("ZZCAPTUREEND"));
    println!("{{\"consumer\":\"{consumer}\",\"shape\":\"{shape}\",\"lines\":{count}}}");
    for pass in 0..passes + 2 {
        session.drain_stragglers();
        if pass == 0 {
            transcript.clear();
            for line in &lines {
                session.feed(line);
            }
            assert!(
                runtime.block_on(session.drain_collect_until("ZZCAPTUREDONE", &mut transcript)),
                "{transcript:?}"
            );
            if consumer == "template" {
                assert_eq!(
                    transcript
                        .iter()
                        .filter(|line| line.as_str() == output)
                        .count(),
                    count
                );
                assert_eq!(
                    transcript
                        .iter()
                        .filter(|line| line.starts_with("OUT:"))
                        .count(),
                    count
                );
            }
        } else {
            #[cfg(feature = "ingest-allocations")]
            let allocations = smudgy_bench::alloc::snapshot();
            let elapsed = runtime.block_on(session.timed_pass(&lines, "ZZCAPTUREDONE"));
            #[cfg(feature = "ingest-allocations")]
            {
                let delta = smudgy_bench::alloc::since(allocations);
                println!(
                    "{{\"alloc_pass\":{pass},\"allocations\":{},\"bytes\":{}}}",
                    delta.count, delta.bytes
                );
            }
            println!("{{\"pass\":{pass},\"ns\":{}}}", elapsed.as_nanos());
        }
    }
}
