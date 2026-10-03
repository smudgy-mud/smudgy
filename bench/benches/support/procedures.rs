//! Shared live-session fixture for procedure controls and RPC timings.

use std::{sync::Arc, time::Duration};

use smudgy_bench::session::{BenchPackage, BenchSession, bench_runtime};
use smudgy_core::session::styled_line::StyledLine;
use smudgy_script::{PackagePermissions, SmudgyCapabilities};

pub fn start() -> (tokio::runtime::Runtime, BenchSession, bool) {
    let rt = bench_runtime();
    let package = BenchPackage {
        owner: "bench",
        name: "procedures",
        source: r#"
import { createProcedure, echo } from "smudgy:core";
let posts = 0, release;
export const request = createProcedure((args) => {
    if (args.post && ++posts === 128) { posts = 0; echo("POST_DONE"); }
    if (args.wait) return new Promise(resolve => { release = resolve; });
    return args;
});
export const finish = createProcedure(() => { release?.(); release = undefined; });
"#
        .to_string(),
        consent: PackagePermissions {
            smudgy: SmudgyCapabilities {
                echo: true,
                interop_write: true,
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let consumer = r#"
import { createTrigger, echo } from "smudgy:core";
import { request, finish } from "smudgy:procedures/bench/procedures";
let fires = 0;
createTrigger(/^ZZLINE$/, () => {
    if (++fires === 500) { fires = 0; echo("LINES_DONE"); }
}, { name: "line" });
createTrigger(/^ZZPOST$/, () => {
    for (let i = 0; i < 128; i++) request.post({ post: true, n: i });
}, { name: "post" });
createTrigger(/^ZZCALL (\d+) (\d+)$/, async (m) => {
    const count = Number(m[1]), size = Number(m[2]);
    const payload = { blob: "x".repeat(size) };
    const replies = await Promise.all(Array.from({ length: count }, () => request.call(payload)));
    if (replies.some(r => r.blob !== payload.blob)) throw new Error("bad reply");
    echo("CALLS_DONE");
}, { name: "call" });
createTrigger(/^ZZWAIT$/, () => {
    request.call({ wait: true }, { timeoutMs: 60000 }).then(() => echo("WAIT_DONE"));
    // This reply proves that the earlier waiting implementation was entered.
    request.call({}).then(() => echo("WAIT_STARTED"));
}, { name: "wait" });
createTrigger(/^ZZFINISH$/, () => finish.post({}), { name: "finish" });
echo("PROCEDURES_READY:" + (typeof request.call === "function"));
"#;
    let mut session = BenchSession::start(
        &rt,
        "ZZProcedures",
        9420,
        &[("consumer.js", consumer.to_string())],
        &[package],
    );
    let mut transcript = Vec::new();
    rt.block_on(async {
        assert!(
            session
                .drain_collect_until("PROCEDURES_READY:", &mut transcript)
                .await,
            "consumer failed to load: {transcript:?}"
        );
    });
    let calls = transcript
        .iter()
        .any(|s| s.contains("PROCEDURES_READY:true"));
    (rt, session, calls)
}

pub fn passes(
    rt: &tokio::runtime::Runtime,
    session: &mut BenchSession,
    lines: &[Arc<StyledLine>],
    marker: &str,
    iters: u64,
) -> Duration {
    rt.block_on(async {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            session.drain_stragglers();
            total += session.timed_pass(lines, marker).await;
        }
        total
    })
}
