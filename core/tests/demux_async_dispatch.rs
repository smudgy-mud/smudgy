//! Regression guard for the per-isolate waker demux (`script/EVENT-LOOP-READINESS-DEMUX.md`).
//!
//! The demux rewrites `ScriptEngine::poll_event_loop` to pump only the isolates in a "ready-set"
//! instead of every isolate each pass. The subtle hazard: an alias/trigger
//! fired by user input runs its JS **synchronously** via `call_javascript_function`, *outside* any
//! `poll_event_loop` pass. If the engine only ever polled isolates the demux queued, a continuation
//! that isolate schedules could be **stranded** — the isolate would never be re-polled to run it.
//!
//! Scheduling a bare `setTimeout` does NOT expose this: a timer op wakes deno's registered waker
//! (the isolate's `DemuxWaker`) as a side effect, which re-queues the isolate on its own. A
//! **promise microtask** (`Promise.then`) does not wake the runtime, so it is the case that
//! actually strands. The implementation seeds the dispatched isolate into the ready-set after
//! synchronous JS execution (`ScriptEngine::mark_isolate_ready`) to cover it.
//!
//! This file covers the two continuation shapes that motivated probing the demux:
//! 1. `alias_scheduled_promise_chain_drains_under_demux` — a *chained* promise microtask sequence
//!    (several microtask ticks) must fully drain in the single seeded pump. This is the real seed
//!    guard: removing the seed in `call_javascript_function` makes it fail.
//! 2. (same test, second phase) a `setTimeout` callback that itself queues a microtask must run
//!    that microtask too — drained via deno's self-rewake / `MAX_DENO_ITERS` drain-to-quiescence.
//!
//! The two phases are sequenced (the timer alias is only fired *after* the microtask chain has
//! already completed) so the timer's self-wake can't mask the microtask guard.
//!
//! The existing `command_ordering` / `script_integration` tests use only synchronous `send()`, so
//! they would NOT catch a stranded continuation.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// Two aliases:
/// - `chain`: a chained promise microtask sequence (each `.then` returns a promise, adding
///   adoption ticks) ending in a sentinel — exercises drain-to-exhaustion of the microtask queue.
/// - `tmicro`: a `setTimeout` whose callback queues a microtask that echoes a sentinel — exercises
///   a continuation queued *by* a timer callback.
const HARNESS_TS: &str = r#"
// `echo` is not ambient in modules (minimal globalThis); import it
// alongside `createAlias` from smudgy:core.
import { createAlias, echo } from "smudgy:core";

createAlias("^chain$", () => {
    Promise.resolve()
        .then(() => Promise.resolve(1))
        .then((n) => Promise.resolve(n + 1))
        .then((n) => Promise.resolve(n + 1))
        .then((n) => { echo("CHAIN_FIRED depth=" + n); });
});

createAlias("^tmicro$", () => {
    setTimeout(() => {
        Promise.resolve().then(() => { echo("TIMER_MICRO_FIRED"); });
    }, 40);
});

echo("MODULE_READY");
"#;

#[tokio::test]
async fn alias_scheduled_promise_chain_drains_under_demux() {
    // Hermetic smudgy home so the test never touches the user's real data dir.
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    // Leak the TempDir: the runtime thread may flush its session log slightly after the test
    // returns, and we don't want cleanup to race that write.
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);

    let server = "Arctic";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("demux_harness.ts"), HARNESS_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7002),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));

    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Phase 1: once the module's aliases are registered (MODULE_READY), fire `chain` — a microtask
    // chain with no timer, so nothing but the seed-after-dispatch can get it re-polled.
    // Phase 2: only AFTER the chain has fully drained (CHAIN_FIRED) do we fire `tmicro`, so the
    // timer's self-wake cannot mask the microtask guard above.
    let mut lines = Vec::new();
    let mut sent_chain = false;
    let mut sent_tmicro = false;
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                    if !sent_chain && line.text == "MODULE_READY" {
                        tx.send(RuntimeAction::Send(Arc::new("chain".to_string())))
                            .unwrap();
                        sent_chain = true;
                    } else if !sent_tmicro && line.text.starts_with("CHAIN_FIRED") {
                        tx.send(RuntimeAction::Send(Arc::new("tmicro".to_string())))
                            .unwrap();
                        sent_tmicro = true;
                    }
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "MODULE_READY"),
        "module top-level must run and register the aliases.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("CHAIN_FIRED")),
        "an alias-scheduled chained promise microtask must fully drain under the demux \
         (seed-after-dispatch, Doc B §5e). If this is missing, the dispatched isolate was never \
         re-polled to run its queued microtasks — the demux stranded the continuation.\n\
         Transcript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "TIMER_MICRO_FIRED"),
        "a microtask queued by a setTimeout callback must also run (drained via deno self-rewake \
         / MAX_DENO_ITERS).\nTranscript:\n{transcript}"
    );
}
