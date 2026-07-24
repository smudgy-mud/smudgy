//! End-to-end coverage of the `input` scripting surface (`docs/input.md`):
//! write ops travel op → `RuntimeAction::InputApply` → `SessionEvent::InputOp`
//! (observed here on the session event stream, exactly where the UI applies them),
//! mirror **reads** — and only reads — flag interest (a write-only script never
//! subscribes the session to per-keystroke traffic), reads resolve against the
//! session-thread mirror fed by `RuntimeAction::InputStateChanged`, handles cannot
//! be re-aimed at another pane, and the whole surface is denied to a sandboxed
//! package without the `input` capability.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::runtime::input::{InputOp, InputSnapshot, InputSource};
use smudgy_core::session::runtime::pane::{MAIN_PANE_KEY, PaneKey};
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams,
    TaggedSessionEvent, spawn, spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage, SmudgyCapabilities,
};
use tokio::sync::mpsc::UnboundedSender;

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// First-setter-wins process-global smudgy home; create `<home>/<server>/{modules,logs}`.
fn prepare_server(server: &str) -> std::path::PathBuf {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let server_dir = home.join(server);
    std::fs::create_dir_all(server_dir.join("modules")).unwrap();
    std::fs::create_dir_all(server_dir.join("logs")).unwrap();
    server_dir
}

fn session_params(session_id: u32, server: &str) -> Arc<SessionParams> {
    Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })
}

fn collect_lines(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

/// One input-related event as seen on the session event stream, in arrival
/// order — the interest notification interleaved with the forwarded ops and
/// merged word-set pushes, so tests can pin their relative order.
#[derive(Debug, PartialEq, Eq)]
enum WireRecord {
    Interest,
    Op(PaneKey, InputOp),
    /// One merged word-set push: suggestions in merge order, blacklist sorted
    /// (the wire set is unordered).
    Words {
        key: PaneKey,
        suggestions: Vec<String>,
        blacklist: Vec<String>,
    },
    /// A pane opened, with whether its def carries an input — the failed-load
    /// test pins that an input-bearing pane both opens and closes.
    PaneOpened {
        key: PaneKey,
        has_input: bool,
    },
    PaneClosed(PaneKey),
    /// The server's telnet ECHO state forwarded to the UI (the auto-mask
    /// signal, `docs/input.md` §3.10).
    ServerEcho(bool),
}

fn record_event(event: SessionEvent, lines: &mut Vec<String>, records: &mut Vec<WireRecord>) {
    match event {
        SessionEvent::UpdateBuffer(updates) => collect_lines(&updates, lines),
        SessionEvent::InputMirrorInterest => records.push(WireRecord::Interest),
        SessionEvent::InputOp { key, op } => records.push(WireRecord::Op(key, op)),
        SessionEvent::InputWordSets {
            key,
            suggestions,
            blacklist,
        } => {
            let mut blacklist: Vec<String> = blacklist.iter().cloned().collect();
            blacklist.sort();
            records.push(WireRecord::Words {
                key,
                suggestions: suggestions.iter().map(|w| w.as_str().to_string()).collect(),
                blacklist,
            });
        }
        SessionEvent::PaneOpened { def, .. } => records.push(WireRecord::PaneOpened {
            key: def.key,
            has_input: def.input.is_some(),
        }),
        SessionEvent::PaneClosed(key) => records.push(WireRecord::PaneClosed(key)),
        SessionEvent::ServerEcho { enabled } => records.push(WireRecord::ServerEcho(enabled)),
        _ => {}
    }
}

/// Wait for `RuntimeReady`, recording lines and input wire events on the way.
async fn wait_runtime_ready(
    events: &mut (impl futures::Stream<Item = TaggedSessionEvent> + Unpin),
    lines: &mut Vec<String>,
    records: &mut Vec<WireRecord>,
) -> UnboundedSender<RuntimeAction> {
    loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => return tx,
            other => record_event(other, lines, records),
        }
    }
}

/// Drain the event stream until it goes quiet, recording lines and input wire
/// events.
async fn drain_quiet(
    events: &mut (impl futures::Stream<Item = TaggedSessionEvent> + Unpin),
    lines: &mut Vec<String>,
    records: &mut Vec<WireRecord>,
) {
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        record_event(event.event, lines, records);
    }
}

/// The main-isolate module for the write/interest test: cold mirror reads
/// (before any UI state message), the ambient/`session.input` identity, the
/// frozen-handle forgery probe, the Session creator-re-aim and
/// hand-constructed-Session probes, then a burst of writes — focus and blur
/// back-to-back (both must arrive; blur is not gated on any stale focus
/// state), a proposal, and masking.
const INPUT_HARNESS_TS: &str = r#"
import { input, session, echo } from "smudgy:core";

// Reads against the cold mirror resolve to the default empty state.
const cold =
    input.value === "" &&
    input.cursor === 0 &&
    input.selection === null &&
    input.focused === false &&
    input.masked === false;
echo(cold ? "COLD_READ_OK" : "COLD_READ_FAIL");

// `session.input` and the ambient `input` address the same main input.
echo(session.input.value === input.value ? "ALIAS_OK" : "ALIAS_FAIL");

// A handle cannot be re-aimed at another pane: identity is closure-captured
// and the handle frozen, so a (strict-mode) forgery attempt throws and the
// handle keeps addressing the main input.
const forged = input as any;
let verdict = "silent";
try { forged._pane = "side"; verdict = "assigned"; }
catch (e) { verdict = e instanceof TypeError ? "threw" : "other"; }
try { forged.replace = (_: string) => {}; verdict += "+assigned"; }
catch (e) { verdict += e instanceof TypeError ? "+threw" : "+other"; }
echo(verdict === "threw+threw" ? "FORGE_OK" : "FORGE_FAIL:" + verdict);

// A Session handle cannot be re-aimed at another creator's word sets either:
// the creator id is a private field, so a `_creatorId` write lands as an
// inert expando and the handle keeps acting under its minting creator (a
// forged id would otherwise error at the op as an unknown creator).
const reaimed = session as any;
reaimed._creatorId = 424242;
session.input.completion.add("reaimcheck");
echo(session.input.completion.has("reaimcheck") ? "REAIM_OK" : "REAIM_FAIL");
session.input.completion.clear();

// A hand-constructed Session carries no creator identity: its word-set
// registries teach on use instead of guessing one.
let bare = "silent";
try {
    new (session.constructor as any)(session.id).input.completion.add("x");
    bare = "no_throw";
} catch (e) { bare = e instanceof TypeError ? "threw" : "other"; }
echo(bare === "threw" ? "BARE_SESSION_OK" : "BARE_SESSION_FAIL:" + bare);

// Writes: each becomes an InputOp session event, in this order.
input.focus();
input.blur();
input.propose("kill rat");
input.masked = true;
"#;

#[tokio::test]
async fn input_writes_arrive_in_order_and_reads_flag_interest_once() {
    let server = "InputApi";
    let server_dir = prepare_server(server);
    std::fs::write(server_dir.join("modules").join("input.ts"), INPUT_HARNESS_TS).unwrap();

    let mut events = Box::pin(spawn(session_params(7301, server)));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    let transcript = lines.join("\n");
    for marker in ["COLD_READ_OK", "ALIAS_OK", "FORGE_OK", "REAIM_OK", "BARE_SESSION_OK"] {
        assert!(
            lines.iter().any(|l| l == marker),
            "expected {marker}.\nTranscript:\n{transcript}"
        );
    }

    // Interest is flagged exactly once — by the reads — and precedes the
    // first forwarded write on the ordered channel (the module reads first).
    let interest_count = records
        .iter()
        .filter(|r| matches!(r, WireRecord::Interest))
        .count();
    assert_eq!(interest_count, 1, "reads must flag interest exactly once");
    let interest_at = records
        .iter()
        .position(|r| matches!(r, WireRecord::Interest))
        .expect("interest recorded");
    let first_op_at = records
        .iter()
        .position(|r| matches!(r, WireRecord::Op(..)))
        .expect("ops recorded");
    assert!(
        interest_at < first_op_at,
        "the reads' interest notification precedes the first write op; got {records:#?}"
    );

    // The write ops arrive on the main input, in issue order — including the
    // focus();blur() pair (blur is never dropped on stale focus state).
    let ops: Vec<&InputOp> = records
        .iter()
        .filter_map(|r| match r {
            WireRecord::Op(key, op) => {
                assert_eq!(*key, MAIN_PANE_KEY, "ops address the main pane's input");
                Some(op)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ops,
        vec![
            &InputOp::Focus,
            &InputOp::Blur,
            &InputOp::Propose(Arc::new("kill rat".to_string())),
            &InputOp::SetMasked(true),
        ],
        "focus/blur/propose/mask must arrive in script order"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// The mirror-read module: an alias reads the (fed) mirror back on demand.
const MIRROR_HARNESS_TS: &str = r#"
import { input, createAlias, echo } from "smudgy:core";

createAlias("^readmirror$", () => {
    const sel = input.selection;
    echo(
        "MIRROR value=" + JSON.stringify(input.value) +
        " cursor=" + input.cursor +
        " sel=" + (sel === null ? "null" : sel.start + "-" + sel.end) +
        " focused=" + input.focused +
        " masked=" + input.masked,
    );
});
"#;

/// Feed one snapshot the way the UI would, run the `readmirror` alias, and
/// return the transcript.
async fn feed_and_read(
    events: &mut (impl futures::Stream<Item = TaggedSessionEvent> + Unpin),
    tx: &UnboundedSender<RuntimeAction>,
    snapshot: InputSnapshot,
) -> Vec<String> {
    tx.send(RuntimeAction::InputStateChanged {
        key: MAIN_PANE_KEY,
        snapshot,
        source: InputSource::User,
    })
    .unwrap();
    tx.send(RuntimeAction::Send(Arc::new("readmirror".to_string())))
        .unwrap();
    let mut lines = Vec::new();
    let mut records = Vec::new();
    drain_quiet(events, &mut lines, &mut records).await;
    lines
}

#[tokio::test]
async fn mirror_reads_track_delivered_state_and_masked_content_is_suppressed() {
    let server = "InputApiMirror";
    let server_dir = prepare_server(server);
    std::fs::write(server_dir.join("modules").join("input.ts"), MIRROR_HARNESS_TS).unwrap();

    let mut events = Box::pin(spawn(session_params(7304, server)));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    // Reads reflect the delivered mirror state (positions are UTF-16 code
    // units end to end; they cross this boundary untouched).
    let lines = feed_and_read(
        &mut events,
        &tx,
        InputSnapshot {
            value: Arc::new("kill rat".to_string()),
            cursor: 8,
            selection: Some((0, 8)),
            focused: true,
            masked: false,
        },
    )
    .await;
    let transcript = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|l| l == "MIRROR value=\"kill rat\" cursor=8 sel=0-8 focused=true masked=false"),
        "reads must reflect the delivered mirror state.\nTranscript:\n{transcript}"
    );

    // A masked state message must never land content in the mirror, even if
    // it (wrongly) carries some.
    let lines = feed_and_read(
        &mut events,
        &tx,
        InputSnapshot {
            value: Arc::new("hunter2".to_string()),
            cursor: 7,
            selection: Some((0, 7)),
            focused: true,
            masked: true,
        },
    )
    .await;
    let transcript = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|l| l == "MIRROR value=\"\" cursor=0 sel=null focused=true masked=true"),
        "a masked snapshot must read back content-suppressed.\nTranscript:\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

// ---------------------------------------------------------------------------
// Capability gating (the sandboxed-package harness of
// package_isolates_op_capabilities.rs, applied to the input surface).
// ---------------------------------------------------------------------------

fn make_package(owner: &str, name: &str, version: &str, src: &str) -> ResolvedPackage {
    let manifest_json = format!(r#"{{ "name": "{name}", "version": "{version}" }}"#);
    ResolvedPackage {
        key: PackageKey {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        resolved_version: version.to_string(),
        manifest: PackageManifest::parse(&manifest_json).expect("valid manifest"),
        integrity: format!("test-{name}-{version}"),
        modules: vec![PackageModuleSource {
            subpath: "index.js".to_string(),
            text: src.to_string(),
        }],
    }
}

fn factory_for(packages: Vec<ResolvedPackage>) -> PackageProviderFactory {
    Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        for pkg in &packages {
            provider.insert(pkg.clone());
        }
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    })
}

fn consent_with(extra: impl FnOnce(&mut SmudgyCapabilities)) -> PackagePermissions {
    let mut smudgy = SmudgyCapabilities {
        echo: true,
        ..Default::default()
    };
    extra(&mut smudgy);
    PackagePermissions {
        smudgy,
        ..Default::default()
    }
}

async fn run_capability_case(
    session_id: u32,
    server: &str,
    spec: &str,
    consent: PackagePermissions,
    pkg: ResolvedPackage,
) -> (Vec<String>, Vec<WireRecord>) {
    prepare_server(server);
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(server, spec, &consent).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn_with_package_provider(params, factory_for(vec![pkg])));
    let mut lines: Vec<String> = Vec::new();
    let mut records: Vec<WireRecord> = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();
    (lines, records)
}

fn has_line(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

/// Without the `input` capability, every input op — reads included — throws
/// `NotCapable` naming `input`, and nothing (op or interest) reaches the UI.
#[tokio::test]
async fn input_ops_are_denied_without_the_capability() {
    let src = r#"
        import session, { input, echo } from "smudgy:core";
        const probe = (name, fn) => {
            try { fn(); echo(name + ":NO_THROW"); }
            catch (e) { echo(name + ":DENIED:" + (e?.message ?? String(e))); }
        };
        probe("read",    () => { const _ = input.value; });
        probe("replace", () => input.replace("x"));
        probe("propose", () => session.input.propose("x"));
        probe("focus",   () => input.focus());
        probe("submit",  () => input.submit());
        probe("mask",    () => { input.masked = true; });
        probe("words",   () => input.completion.add("north"));
        probe("wordsq",  () => input.completion.list());
        probe("wordsb",  () => input.completion.blacklist.clear());
        probe("hist",    () => input.history.push("x"));
        probe("histq",   () => input.history.list());
        probe("histc",   () => input.history.clear());
        echo("DONE");
    "#;
    let (lines, records) = run_capability_case(
        7302,
        "pi_caps_input_denied",
        "smudgy://wbk/inputless",
        consent_with(|_| {}), // echo only
        make_package("wbk", "inputless", "1.0.0", src),
    )
    .await;

    for probe in [
        "read", "replace", "propose", "focus", "submit", "mask", "words", "wordsq", "wordsb",
        "hist", "histq", "histc",
    ] {
        assert!(
            !has_line(&lines, &format!("{probe}:NO_THROW")),
            "without the input capability, `{probe}` must throw; transcript:\n{lines:#?}"
        );
        assert!(
            has_line(&lines, &format!("{probe}:DENIED:")),
            "the `{probe}` denial must surface; transcript:\n{lines:#?}"
        );
    }
    assert!(
        has_line(&lines, "'input'"),
        "the denial must name the missing 'input' capability; transcript:\n{lines:#?}"
    );
    assert!(
        records.is_empty(),
        "no input op or interest may reach the UI from a denied package; got {records:#?}"
    );
    assert!(has_line(&lines, "DONE"), "the probe must run to completion");
}

/// With the `input` capability granted, the same surface works — and the
/// write, issued first, does not flag mirror interest: only the state read
/// that follows it does. (That state read would mask any interest from the
/// history read here; `history_reads_alone_put_nothing_on_the_wire` is the
/// pin that history reads flag none.)
#[tokio::test]
async fn input_ops_work_with_the_capability_and_writes_do_not_subscribe() {
    let src = r#"
        import { input, echo } from "smudgy:core";
        try {
            input.propose("say hi"); // a write alone must not flag interest
            const hist = input.history.list();
            const cold = input.value === "" && input.masked === false && hist.length === 0;
            echo(cold ? "INPUT_OK" : "INPUT_BAD_READ");
        } catch (e) {
            echo("INPUT_ERR:" + (e?.message ?? String(e)));
        }
    "#;
    let (lines, records) = run_capability_case(
        7303,
        "pi_caps_input_granted",
        "smudgy://wbk/inputful",
        consent_with(|s| s.input = true),
        make_package("wbk", "inputful", "1.0.0", src),
    )
    .await;

    assert!(
        has_line(&lines, "INPUT_OK"),
        "the granted input surface must work; transcript:\n{lines:#?}"
    );
    assert_eq!(
        records,
        vec![
            WireRecord::Op(
                MAIN_PANE_KEY,
                InputOp::Propose(Arc::new("say hi".to_string()))
            ),
            WireRecord::Interest,
        ],
        "the write reaches the UI without flagging interest; the read that \
         follows it flags interest exactly once"
    );
}

// ---------------------------------------------------------------------------
// sys:input — submit interception (`docs/input.md` §3.5). The tests
// drive `RuntimeAction::SubmitInput` directly, standing in for the UI's
// submit routing (its only real constructor), and observe the pipeline
// through aliases and the `sys:send` stream.
// ---------------------------------------------------------------------------

/// Feed one typed submission, exactly as the UI's submit routing would.
fn submit_typed(tx: &UnboundedSender<RuntimeAction>, text: &str) {
    tx.send(RuntimeAction::SubmitInput(Arc::new(text.to_string())))
        .unwrap();
}

/// The module-session ritual shared by the `sys:input` tests (the module-file
/// sibling of `run_capability_case` above): write `module_ts` as the server's
/// auto-loaded module, spawn, wait for the runtime, let the boot output
/// settle, run `drive` (typically feeding typed submissions), drain until
/// quiet again, and shut down. Returns the echoed lines and the input wire
/// records, both accumulated across the whole session.
async fn run_module_session(
    session_id: u32,
    server: &str,
    module_ts: &str,
    drive: impl FnOnce(&UnboundedSender<RuntimeAction>),
) -> (Vec<String>, Vec<WireRecord>) {
    let server_dir = prepare_server(server);
    std::fs::write(server_dir.join("modules").join("sys_input.ts"), module_ts).unwrap();

    let mut events = Box::pin(spawn(session_params(session_id, server)));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;
    drive(&tx);
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();
    (lines, records)
}

/// Rewrite and composition: the payload carries the line as typed, a
/// `submission.replace()` feeds the raw/split/alias pipeline the new text
/// (separator splitting included), and a later handler reads an earlier
/// handler's replacement through the live ambient while its payload stays
/// as-typed.
const SYS_INPUT_REWRITE_TS: &str = r#"
import { echo, submission, createAlias } from "smudgy:core";
import { submit, send } from "smudgy:events/sys";

send.on(({ command }) => echo("SENT:" + command));
createAlias("^hail (\\w+)$", (m) => echo("ALIAS:" + m[1]));

submit.on(({ text }) => {
    echo("GOT:" + text);
    if (text === "!both") {
        submission.replace("north;south");
    } else if (text.startsWith("!")) {
        submission.replace("hail " + text.slice(1));
    }
});
submit.on(({ text }) => {
    echo("SECOND:payload=" + text + " live=" + submission.text);
});
"#;

#[tokio::test]
async fn sys_input_rewrite_feeds_the_pipeline_and_handlers_compose() {
    let (lines, _records) =
        run_module_session(7305, "SysInputRewrite", SYS_INPUT_REWRITE_TS, |tx| {
            submit_typed(tx, "!bob");
            submit_typed(tx, "!both");
            submit_typed(tx, "hail ann");
        })
        .await;

    let transcript = lines.join("\n");
    // The payload is the line as typed; the replaced text is what aliases see.
    assert!(
        lines.iter().any(|l| l == "GOT:!bob"),
        "the payload must carry the line as typed.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ALIAS:bob"),
        "the alias must match the replaced text.\nTranscript:\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("SENT:!")),
        "the as-typed `!` line must never reach the pipeline once replaced.\nTranscript:\n{transcript}"
    );
    // The replacement enters BEFORE separator splitting: one replaced line
    // splits into two commands.
    for marker in ["SENT:north", "SENT:south"] {
        assert!(
            lines.iter().any(|l| l == marker),
            "a replacement must pass through separator splitting ({marker}).\nTranscript:\n{transcript}"
        );
    }
    // A later handler sees the earlier handler's replacement through the
    // ambient object, while its payload stays as-typed.
    assert!(
        lines
            .iter()
            .any(|l| l == "SECOND:payload=!both live=north;south"),
        "later handlers must see earlier replacements via `submission`.\nTranscript:\n{transcript}"
    );
    // With a subscriber present, an untouched line passes through unchanged.
    assert!(
        lines.iter().any(|l| l == "ALIAS:ann"),
        "an untouched submission must flow through normally.\nTranscript:\n{transcript}"
    );
}

/// Cancel: nothing reaches raw/`=`/split/alias processing — a cancelled line
/// produces no `sys:send` at all (both separator halves die pre-split) — and
/// cancel beats a replace from a later handler.
const SYS_INPUT_CANCEL_TS: &str = r#"
import { echo, submission } from "smudgy:core";
import { submit, send } from "smudgy:events/sys";

send.on(({ command }) => echo("SENT:" + command));
submit.on(({ text }) => {
    if (text.includes("forbidden")) submission.cancel();
});
submit.on(({ text }) => {
    if (text.includes("forbidden")) submission.replace("laundered");
});
"#;

#[tokio::test]
async fn sys_input_cancel_swallows_the_line_and_beats_replace() {
    let (lines, _records) = run_module_session(7306, "SysInputCancel", SYS_INPUT_CANCEL_TS, |tx| {
        submit_typed(tx, "say forbidden;say fine");
        submit_typed(tx, "say hello");
    })
    .await;

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "SENT:say hello"),
        "an uncancelled submission still flows.\nTranscript:\n{transcript}"
    );
    for leaked in ["SENT:say forbidden", "SENT:say fine", "SENT:laundered"] {
        assert!(
            !lines.iter().any(|l| l == leaked),
            "a cancel swallows the whole pre-split line and beats any replace ({leaked}).\nTranscript:\n{transcript}"
        );
    }
}

/// The discriminator: `session.send()` (the `Send` action) never fires
/// `sys:input`; only the typed-submission action does. A masked submission
/// (the redaction path the UI routes it to) never fires it either.
const SYS_INPUT_DISCRIMINATOR_TS: &str = r#"
import { echo, send, createAlias } from "smudgy:core";
import { submit } from "smudgy:events/sys";

submit.on(({ text }) => echo("GOT:" + text));
createAlias("^viasend$", () => { send("hello world"); });
"#;

#[tokio::test]
async fn session_send_and_masked_submissions_never_fire_sys_input() {
    let (lines, _records) = run_module_session(
        7307,
        "SysInputDiscriminator",
        SYS_INPUT_DISCRIMINATOR_TS,
        |tx| {
            // Typed: fires. The alias's `send("hello world")` re-enters the pipeline
            // as `Send` and must NOT fire again.
            submit_typed(tx, "viasend");
            // A script/link send arriving as `Send` must not fire either.
            tx.send(RuntimeAction::Send(Arc::new("scripted".to_string())))
                .unwrap();
            // A masked submission rides the redaction path (what the UI sends for a
            // masked Enter) and must never reach the handlers.
            tx.send(RuntimeAction::SendWithRedactions {
                text: Arc::new("hunter2".to_string()),
                redactions: Arc::new(vec!["hunter2".to_string()]),
            })
            .unwrap();
        },
    )
    .await;

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "GOT:viasend"),
        "the typed submission must fire sys:input.\nTranscript:\n{transcript}"
    );
    for absent in ["GOT:hello world", "GOT:scripted", "GOT:hunter2"] {
        assert!(
            !lines.iter().any(|l| l == absent),
            "only typed submissions may fire sys:input ({absent}).\nTranscript:\n{transcript}"
        );
    }
    assert!(
        !transcript.contains("hunter2"),
        "the masked submission's secret must stay redacted everywhere.\nTranscript:\n{transcript}"
    );
}

/// Passthrough: with no `sys:input` subscriber, a typed submission behaves
/// exactly like `Send` (the emit is gated on a live subscriber).
const SYS_INPUT_PASSTHROUGH_TS: &str = r#"
import { echo, createAlias } from "smudgy:core";

createAlias("^hail (\\w+)$", (m) => echo("ALIAS:" + m[1]));
"#;

#[tokio::test]
async fn sys_input_without_subscribers_is_a_plain_send() {
    let (lines, _records) = run_module_session(
        7308,
        "SysInputPassthrough",
        SYS_INPUT_PASSTHROUGH_TS,
        |tx| submit_typed(tx, "hail bob"),
    )
    .await;

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "ALIAS:bob"),
        "with nobody subscribed a typed submission flows like a Send.\nTranscript:\n{transcript}"
    );
}

/// The ambient `submission` object outside a `sys:input` handler throws, for
/// every member.
const SUBMISSION_OUTSIDE_HANDLER_TS: &str = r#"
import { echo, submission } from "smudgy:core";

const probe = (name: string, fn: () => unknown) => {
    try { fn(); echo(name + ":NO_THROW"); }
    catch (e) { echo(name + ":THREW:" + ((e as any)?.message ?? String(e))); }
};
probe("text", () => submission.text);
probe("replace", () => submission.replace("x"));
probe("cancel", () => submission.cancel());
"#;

#[tokio::test]
async fn submission_outside_a_handler_throws() {
    let (lines, _records) = run_module_session(
        7309,
        "SysInputOutside",
        SUBMISSION_OUTSIDE_HANDLER_TS,
        |_| {},
    )
    .await;

    let transcript = lines.join("\n");
    for probe in ["text", "replace", "cancel"] {
        assert!(
            !lines.iter().any(|l| l == &format!("{probe}:NO_THROW")),
            "`submission.{probe}` outside a handler must throw.\nTranscript:\n{transcript}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("{probe}:THREW:")) && l.contains("sys:input")),
            "the `submission.{probe}` error must name the sys:input handler contract.\nTranscript:\n{transcript}"
        );
    }
}

/// Subscribing to `sys:input` needs the `input` capability on top of the
/// interop read grant: a package with only `interop: ["read"]` is denied
/// (while other `sys:` events still work), and the denial names `input`.
#[tokio::test]
async fn sys_input_subscription_is_denied_without_the_input_capability() {
    let src = r#"
        import { echo } from "smudgy:core";
        import { submit, receive } from "smudgy:events/sys";
        try { submit.on(() => {}); echo("SUB:OK"); }
        catch (e) { echo("SUB:DENIED:" + (e?.message ?? String(e))); }
        try { receive.on(() => {}); echo("RECEIVE:OK"); }
        catch (e) { echo("RECEIVE:DENIED:" + (e?.message ?? String(e))); }
    "#;
    let (lines, _records) = run_capability_case(
        7310,
        "pi_caps_sys_input_denied",
        "smudgy://wbk/inputspy",
        consent_with(|s| s.interop_read = true),
        make_package("wbk", "inputspy", "1.0.0", src),
    )
    .await;

    assert!(
        !has_line(&lines, "SUB:OK"),
        "without the input capability, subscribing to sys:input must throw; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "SUB:DENIED:") && has_line(&lines, "'input'"),
        "the denial must surface and name the missing 'input' capability; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "RECEIVE:OK"),
        "interop read alone still covers the other sys events; transcript:\n{lines:#?}"
    );
}

/// With `input` granted (plus interop read), a sandboxed package subscribes
/// and its handler acts on the submission like any other subscriber.
#[tokio::test]
async fn sys_input_subscription_works_with_the_input_capability() {
    let src = r#"
        import { echo, submission } from "smudgy:core";
        import { submit } from "smudgy:events/sys";
        try {
            submit.on(({ text }) => {
                echo("GOT:" + text);
                if (text === "!x") submission.replace("expanded");
            });
            echo("SUB:OK");
        } catch (e) {
            echo("SUB:DENIED:" + (e?.message ?? String(e)));
        }
    "#;
    let server = "pi_caps_sys_input_granted";
    let spec = "smudgy://wbk/inputhelper";
    prepare_server(server);
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(
        server,
        spec,
        &consent_with(|s| {
            s.interop_read = true;
            s.input = true;
        }),
    )
    .unwrap();

    let mut events = Box::pin(spawn_with_package_provider(
        session_params(7311, server),
        factory_for(vec![make_package("wbk", "inputhelper", "1.0.0", src)]),
    ));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    submit_typed(&tx, "!x");
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();

    assert!(
        has_line(&lines, "SUB:OK"),
        "the granted package must subscribe; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "GOT:!x"),
        "the granted package's handler must fire on a typed submission; transcript:\n{lines:#?}"
    );
}

/// The scripted half of the submit path: `input.submit()` becomes an
/// `InputOp::Submit` for the UI, whose submit routing turns it into the
/// typed-submission action (replayed here by the test) — so a scripted
/// submit fires `sys:input` exactly like the user's Enter.
const SCRIPTED_SUBMIT_TS: &str = r#"
import { echo, input } from "smudgy:core";
import { submit } from "smudgy:events/sys";

submit.on(({ text }) => echo("GOT:" + text));
input.replace("from script");
input.submit();
"#;

#[tokio::test]
async fn scripted_submit_rides_the_typed_submission_path() {
    let (lines, records) = run_module_session(
        7312,
        "SysInputScriptedSubmit",
        SCRIPTED_SUBMIT_TS,
        // Stand in for the UI's submit routing: the widget submits its contents
        // through the same path as Enter, which constructs the typed submission.
        |tx| submit_typed(tx, "from script"),
    )
    .await;

    // The script's writes reached the UI in order: stuff the box, submit it.
    // (The drive adds no wire records, so the whole-session record set is the
    // boot-time one.)
    let ops: Vec<&InputOp> = records
        .iter()
        .filter_map(|r| match r {
            WireRecord::Op(_, op) => Some(op),
            _ => None,
        })
        .collect();
    assert_eq!(
        ops,
        vec![
            &InputOp::Replace(Arc::new("from script".to_string())),
            &InputOp::Submit,
        ],
        "input.submit() must arrive as InputOp::Submit behind the replace"
    );

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "GOT:from script"),
        "a scripted submit must fire sys:input via the shared submit path.\nTranscript:\n{transcript}"
    );
}

/// The staleness guard: an async handler that awaits past its own submission's
/// completion holds a dead generation, so acting through the ambient
/// `submission` throws — it must never cancel (or rewrite) a LATER submission.
/// The gate promise is resolved by the second submission's handler, so the
/// stale continuation resumes precisely while the second submission is live
/// (the run loop pumps resolved promises between the handler splice and its
/// completion action) — the exact window in which the old code cancelled the
/// wrong submission.
const SYS_INPUT_STALE_CONTINUATION_TS: &str = r#"
import { echo, submission } from "smudgy:core";
import { submit, send } from "smudgy:events/sys";

send.on(({ command }) => echo("SENT:" + command));

let release: (() => void) | undefined;
const gate = new Promise<void>((resolve) => { release = resolve; });

submit.on(async ({ text }) => {
    if (text === "first") {
        await gate;
        try {
            submission.cancel();
            echo("STALE:NO_THROW");
        } catch (e) {
            echo("STALE:THREW:" + ((e as any)?.message ?? String(e)));
        }
    } else if (text === "second") {
        release!();
    }
});
"#;

#[tokio::test]
async fn stale_submission_continuation_throws_and_cannot_swallow_a_later_submission() {
    let (lines, _records) = run_module_session(
        7313,
        "SysInputStale",
        SYS_INPUT_STALE_CONTINUATION_TS,
        |tx| {
            submit_typed(tx, "first");
            submit_typed(tx, "second");
        },
    )
    .await;

    let transcript = lines.join("\n");
    // The stale continuation's cancel must throw the not-active error…
    assert!(
        !lines.iter().any(|l| l == "STALE:NO_THROW"),
        "a stale continuation must not act on a live submission.\nTranscript:\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("STALE:THREW:") && l.contains("sys:input")),
        "the stale cancel must throw the sys:input contract error.\nTranscript:\n{transcript}"
    );
    // …and the submission that was live when it resumed flows untouched.
    assert!(
        lines.iter().any(|l| l == "SENT:second"),
        "the later submission must NOT be swallowed by the stale cancel.\nTranscript:\n{transcript}"
    );
    // The stale handler's own submission was never cancelled either (its
    // handler only awaited), so it flowed normally at its completion.
    assert!(
        lines.iter().any(|l| l == "SENT:first"),
        "the first submission completes uncancelled.\nTranscript:\n{transcript}"
    );
}

// ---------------------------------------------------------------------------
// Completion word sets (`docs/input.md` §3.8): the creator-scoped
// registries on `input.completion` (+ `.blacklist`), and the merged pushes
// the UI receives.
// ---------------------------------------------------------------------------

/// The registry surface, exercised end to end from a user module:
/// add/list/has/delete/clear round-trip (case-insensitive identity,
/// registered casing preserved), and the whole mutation burst coalescing into
/// ONE merged push carrying the final state.
const WORD_SET_ROUND_TRIP_TS: &str = r#"
import { input, echo } from "smudgy:core";

const c = input.completion;
c.add("north", "Fjord");
c.add("fjord"); // case-insensitive re-add: updates the casing in place
echo("LIST1:" + JSON.stringify(c.list()));
echo("HAS:" + c.has("FJORD"));
echo("DEL:" + c.delete("NORTH"));
echo("DEL2:" + c.delete("north"));
echo("LIST2:" + JSON.stringify(c.list()));
c.blacklist.add("ooc");
echo("BLIST:" + JSON.stringify(c.blacklist.list()));
c.clear();
echo("LIST3:" + JSON.stringify(c.list()));
echo("BLIST2:" + JSON.stringify(c.blacklist.list()));
c.add("west");
"#;

#[tokio::test]
async fn word_set_registry_round_trips_and_coalesces_one_merged_push() {
    let (lines, records) =
        run_module_session(7314, "InputWordsRoundTrip", WORD_SET_ROUND_TRIP_TS, |_| {}).await;

    let transcript = lines.join("\n");
    for marker in [
        r#"LIST1:["north","fjord"]"#,
        "HAS:true",
        "DEL:true",
        "DEL2:false",
        r#"LIST2:["fjord"]"#,
        r#"BLIST:["ooc"]"#,
        "LIST3:[]",
        r#"BLIST2:["ooc"]"#,
    ] {
        assert!(
            lines.iter().any(|l| l == marker),
            "expected {marker}.\nTranscript:\n{transcript}"
        );
    }

    // The whole burst — adds, deletes, clear — coalesces into one push
    // carrying the final merged state.
    let words: Vec<&WireRecord> = records
        .iter()
        .filter(|r| matches!(r, WireRecord::Words { .. }))
        .collect();
    assert_eq!(
        words,
        vec![&WireRecord::Words {
            key: MAIN_PANE_KEY,
            suggestions: vec!["west".to_string()],
            blacklist: vec!["ooc".to_string()],
        }],
        "one coalesced merged push with the burst's final state"
    );
}

/// A registered word is one token: empty strings and strings with whitespace
/// throw at registration, on the suggestion set and the blacklist alike.
const WORD_SET_VALIDATION_TS: &str = r#"
import { input, echo } from "smudgy:core";

const probe = (name: string, fn: () => unknown) => {
    try { fn(); echo(name + ":NO_THROW"); }
    catch (e) { echo(name + ":THREW:" + ((e as any)?.message ?? String(e))); }
};
probe("space", () => input.completion.add("two words"));
probe("empty", () => input.completion.add(""));
probe("tab", () => input.completion.blacklist.add("a\tb"));
echo("LIST:" + JSON.stringify(input.completion.list()));
"#;

#[tokio::test]
async fn word_set_registration_rejects_non_token_words() {
    let (lines, records) =
        run_module_session(7315, "InputWordsValidate", WORD_SET_VALIDATION_TS, |_| {}).await;

    let transcript = lines.join("\n");
    for probe in ["space", "empty", "tab"] {
        assert!(
            !lines.iter().any(|l| l == &format!("{probe}:NO_THROW")),
            "add() must reject a non-token word ({probe}).\nTranscript:\n{transcript}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with(&format!("{probe}:THREW:"))),
            "the {probe} rejection must surface.\nTranscript:\n{transcript}"
        );
    }
    assert!(
        lines.iter().any(|l| l == "LIST:[]"),
        "nothing registers from a rejected call.\nTranscript:\n{transcript}"
    );
    assert!(
        !records.iter().any(|r| matches!(r, WireRecord::Words { .. })),
        "no push goes out when nothing changed; got {records:#?}"
    );
}

/// Two creators — the user's module in the main isolate and a sandboxed
/// package granted `input` — contribute to the same input: each `list()`s
/// only its own words, the package's `clear()` leaves the module's words
/// standing, and the UI's merged push carries both creators' final words.
const WORD_SET_ISOLATION_MODULE_TS: &str = r#"
import { input, echo, createAlias } from "smudgy:core";

input.completion.add("alpha");
createAlias("^wordcheck$", () => {
    echo("USER_LIST:" + JSON.stringify(input.completion.list()));
});
"#;

#[tokio::test]
async fn word_sets_are_creator_scoped_and_merge_across_isolates() {
    let pkg_src = r#"
        import { input, echo } from "smudgy:core";
        try {
            input.completion.add("beta", "alpha"); // "alpha" collides with the user's word
            input.completion.clear();              // ...and clears ONLY this package's words
            input.completion.add("beta");
            echo("PKG_LIST:" + JSON.stringify(input.completion.list()));
        } catch (e) {
            echo("PKG_ERR:" + (e?.message ?? String(e)));
        }
    "#;
    let server = "InputWordsIsolation";
    let spec = "smudgy://wbk/wordhelper";
    let server_dir = prepare_server(server);
    std::fs::write(
        server_dir.join("modules").join("words.ts"),
        WORD_SET_ISOLATION_MODULE_TS,
    )
    .unwrap();
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(server, spec, &consent_with(|s| s.input = true)).unwrap();

    let mut events = Box::pin(spawn_with_package_provider(
        session_params(7316, server),
        factory_for(vec![make_package("wbk", "wordhelper", "1.0.0", pkg_src)]),
    ));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    // The user's list, read after everything loaded: the package's clear()
    // never touched it.
    tx.send(RuntimeAction::Send(Arc::new("wordcheck".to_string())))
        .unwrap();
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();

    assert!(
        has_line(&lines, r#"USER_LIST:["alpha"]"#),
        "the module's words survive the package's clear(); transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, r#"PKG_LIST:["beta"]"#),
        "the package lists only its own words; transcript:\n{lines:#?}"
    );

    // The last merged push carries both creators' final words (isolate load
    // order is not pinned, so assert membership, not order).
    let last_words = records
        .iter()
        .rev()
        .find_map(|r| match r {
            WireRecord::Words { suggestions, .. } => Some(suggestions.clone()),
            _ => None,
        })
        .expect("a merged push was recorded");
    let mut sorted = last_words;
    sorted.sort();
    assert_eq!(
        sorted,
        vec!["alpha".to_string(), "beta".to_string()],
        "the merged view carries both creators' words exactly once"
    );
}

/// The reload lifecycle: word sets die with their creator's isolate
/// generation and reappear only as the reloaded scripts re-register. The
/// module registers "bootword" on its first run only (a data-dir marker file
/// tells runs apart) and "always" on every run — after a reload, the merged
/// push carries "always" alone, proving the reset + post-rebuild resync
/// (rather than words lingering from the dead generation).
const WORD_SET_RELOAD_TS: &str = r#"
import { input, getDataDir } from "smudgy:core";

const flag = getDataDir() + "/word-set-reload-ran";
let first = false;
try { Deno.statSync(flag); } catch { first = true; }
if (first) {
    Deno.writeTextFileSync(flag, "x");
    input.completion.add("bootword");
}
input.completion.add("always");
"#;

#[tokio::test]
async fn word_sets_die_with_the_engine_generation_and_reregister_on_reload() {
    let server = "InputWordsReload";
    let server_dir = prepare_server(server);
    std::fs::write(
        server_dir.join("modules").join("words_reload.ts"),
        WORD_SET_RELOAD_TS,
    )
    .unwrap();

    let mut events = Box::pin(spawn(session_params(7317, server)));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    // Reload, then wait out the (blocking) engine rebuild by waiting for the
    // post-reload RuntimeReady before draining the resync push.
    tx.send(RuntimeAction::Reload).unwrap();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();

    let pushes: Vec<&Vec<String>> = records
        .iter()
        .filter_map(|r| match r {
            WireRecord::Words { suggestions, .. } => Some(suggestions),
            _ => None,
        })
        .collect();
    assert!(
        pushes
            .iter()
            .any(|s| s.contains(&"bootword".to_string()) && s.contains(&"always".to_string())),
        "the first generation pushed both words; got {pushes:#?}"
    );
    assert_eq!(
        pushes.last().copied(),
        Some(&vec!["always".to_string()]),
        "after the reload only the re-registered word survives; got {pushes:#?}"
    );
}

/// The registration caps (`docs/input.md` §3.8): a word longer than
/// 64 characters is rejected, a batch that would push the caller past 512
/// words in one set throws — and registers nothing (`add()` is atomic) —
/// while re-adds of already-registered words stay free at the cap.
const WORD_SET_CAPS_TS: &str = r#"
import { input, echo } from "smudgy:core";

const c = input.completion;
const probe = (name: string, fn: () => unknown) => {
    try { fn(); echo(name + ":NO_THROW"); }
    catch (e) { echo(name + ":THREW:" + ((e as any)?.message ?? String(e))); }
};

probe("longword", () => c.add("x".repeat(65)));
c.add("y".repeat(64)); // exactly at the length cap: fine
echo("LEN64:" + c.has("y".repeat(64)));

// One batch past the count cap: atomic, so nothing from it registers.
probe("bigbatch", () => c.add(...Array.from({ length: 512 }, (_, i) => "w" + i)));
echo("ATOMIC:" + (c.list().length === 1 ? "OK" : "FAIL:" + c.list().length));

// Fill to the cap exactly, then one more new word is refused...
c.add(...Array.from({ length: 511 }, (_, i) => "w" + i));
echo("FULL:" + c.list().length);
probe("overflow", () => c.add("straw"));
// ...but a re-add (casing update) of an existing word is not a new word.
probe("readd", () => c.add("W0"));
echo("READD:" + c.has("w0"));
"#;

#[tokio::test]
async fn word_set_caps_reject_long_words_and_overfull_sets_atomically() {
    let (lines, _records) =
        run_module_session(7318, "InputWordsCaps", WORD_SET_CAPS_TS, |_| {}).await;

    let transcript = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("longword:THREW:") && l.contains("64")),
        "a 65-char word must throw naming the cap.\nTranscript:\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("bigbatch:THREW:") && l.contains("512")),
        "an overfull batch must throw naming the cap.\nTranscript:\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("overflow:THREW:") && l.contains("clear()")),
        "the teaching error names the remedy.\nTranscript:\n{transcript}"
    );
    for marker in ["LEN64:true", "ATOMIC:OK", "FULL:512", "readd:NO_THROW", "READD:true"] {
        assert!(
            lines.iter().any(|l| l == marker),
            "expected {marker}.\nTranscript:\n{transcript}"
        );
    }
}

/// The reload/pending-push interaction: `session.reload()` queued ahead of a
/// word-set mutation in the SAME expansion means the mutation's push action
/// dies with the old engine (the reload drops queued spawned actions) while
/// its pending flag was already set. The reload teardown must treat that flag
/// as needing a resync — otherwise the flag sits forever with no action
/// behind it, and no later mutation can ever queue a push again (the wedge).
const WORD_SET_RELOAD_WEDGE_TS: &str = r#"
import { input, session, createAlias } from "smudgy:core";

createAlias("^seed$", () => { input.completion.add("seeded"); });
// Reload FIRST, so the clear()'s push action is queued behind the Reload and
// dropped by it — the wedge-triggering order.
createAlias("^wedge$", () => { session.reload(); input.completion.clear(); });
createAlias("^late$", () => { input.completion.add("post"); });
"#;

#[tokio::test]
async fn word_set_pushes_survive_a_reload_that_dropped_a_pending_push() {
    let server = "InputWordsReloadWedge";
    let server_dir = prepare_server(server);
    std::fs::write(
        server_dir.join("modules").join("words_wedge.ts"),
        WORD_SET_RELOAD_WEDGE_TS,
    )
    .unwrap();

    let mut events = Box::pin(spawn(session_params(7319, server)));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    tx.send(RuntimeAction::Send(Arc::new("seed".to_string())))
        .unwrap();
    drain_quiet(&mut events, &mut lines, &mut records).await;

    // The wedge expansion: the clear()'s push flag is set, its action dropped.
    tx.send(RuntimeAction::Send(Arc::new("wedge".to_string())))
        .unwrap();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    // A post-reload mutation must still reach the UI.
    tx.send(RuntimeAction::Send(Arc::new("late".to_string())))
        .unwrap();
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();

    let pushes: Vec<&Vec<String>> = records
        .iter()
        .filter_map(|r| match r {
            WireRecord::Words { suggestions, .. } => Some(suggestions),
            _ => None,
        })
        .collect();
    assert!(
        pushes.iter().any(|s| s.contains(&"seeded".to_string())),
        "the pre-reload registration pushed; got {pushes:#?}"
    );
    assert!(
        pushes.iter().any(|s| s.is_empty()),
        "the reload resync pushed the cleared (empty) view; got {pushes:#?}"
    );
    assert_eq!(
        pushes.last().copied(),
        Some(&vec!["post".to_string()]),
        "a mutation after the reload still pushes (no wedged pending flag); got {pushes:#?}"
    );
}

/// A sandboxed package that splits an input-bearing pane, registers
/// completion words at top level, and THEN fails its load: the isolate is
/// discarded, so its already-landed contributions have no owner left to
/// clear them. The failed-load cleanup must purge its word-set seats — the
/// merged view the UI ends up with carries only the surviving creators'
/// words — and must CLOSE the input pane: its `onSubmit` can never exist
/// again (only this package's own re-split could re-register one), so
/// leaving it open would be a live-looking input whose submissions vanish.
#[tokio::test]
async fn failed_package_load_purges_its_word_set_contributions() {
    // Registrations land synchronously, then a top-level await of a failing
    // dynamic import sinks the load itself (a bare top-level throw surfaces
    // as an async uncaught error instead, with the isolate kept).
    let pkg_src = r#"
        import { input, session } from "smudgy:core";
        session.mainPane.split("right", {
            name: "doomed",
            input: { onSubmit: () => {} },
        });
        input.completion.add("zombie");
        input.completion.blacklist.add("shade");
        await import("smudgy://wbk/no_such_dependency");
    "#;
    let module_src = r#"
import { input } from "smudgy:core";
input.completion.add("keep");
"#;
    let server = "InputWordsFailedLoad";
    let spec = "smudgy://wbk/wordbomb";
    let server_dir = prepare_server(server);
    std::fs::write(server_dir.join("modules").join("words.ts"), module_src).unwrap();
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(
        server,
        spec,
        &consent_with(|s| {
            s.input = true;
            s.panes = true;
        }),
    )
    .unwrap();

    let mut events = Box::pin(spawn_with_package_provider(
        session_params(7320, server),
        factory_for(vec![make_package("wbk", "wordbomb", "1.0.0", pkg_src)]),
    ));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();

    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("[package] wordbomb failed to load")),
        "the load failure surfaced; transcript:\n{lines:#?}"
    );

    let last = records
        .iter()
        .rev()
        .find_map(|r| match r {
            WireRecord::Words {
                suggestions,
                blacklist,
                ..
            } => Some((suggestions.clone(), blacklist.clone())),
            _ => None,
        })
        .expect("a merged push was recorded");
    assert_eq!(
        last.0,
        vec!["keep".to_string()],
        "the dead isolate's suggestions were purged; got {records:#?}"
    );
    assert!(
        last.1.is_empty(),
        "the dead isolate's blacklist words were purged; got {records:#?}"
    );

    // The package's input pane opened before the load failed, and the
    // failed-load cleanup closed it through the normal close path.
    let doomed_key = records
        .iter()
        .find_map(|r| match r {
            WireRecord::PaneOpened {
                key,
                has_input: true,
            } => Some(*key),
            _ => None,
        })
        .expect("the input pane opened before the load failed");
    assert!(
        records.contains(&WireRecord::PaneClosed(doomed_key)),
        "the failed load closes its input-bearing pane; got {records:#?}"
    );
}

// ---------------------------------------------------------------------------
// History (`docs/input.md` §3.9): `input.history` reads against the
// unconditionally fed mirror, push/clear crossing to the UI as InputOps, and
// the single-line validation. The tests feed `RuntimeAction::InputHistoryChanged`
// by hand, standing in for the UI's on-change history sync exactly as the
// mirror tests stand in for its state feed.
// ---------------------------------------------------------------------------

/// Feed one history update the way the UI would (newest first).
fn feed_history(tx: &UnboundedSender<RuntimeAction>, entries: &[&str]) {
    tx.send(RuntimeAction::InputHistoryChanged {
        key: MAIN_PANE_KEY,
        entries: Arc::new(entries.iter().map(|e| Arc::new((*e).to_string())).collect()),
    })
    .unwrap();
}

/// The history read module: a cold read at load (before any UI update), an
/// alias reading the fed mirror back on demand, and a `sys:input` handler
/// reading it mid-submission — the UI sends the history update ahead of the
/// submission action, so the handler already sees the line that fired it.
const HISTORY_READ_TS: &str = r#"
import { input, echo, createAlias } from "smudgy:core";
import { submit } from "smudgy:events/sys";

echo("COLD:" + JSON.stringify(input.history.list()));
createAlias("^readhist$", () => echo("LIST:" + JSON.stringify(input.history.list())));
submit.on(({ text }) => {
    if (text.startsWith("hail")) {
        echo("HANDLER_LIST:" + JSON.stringify(input.history.list()));
    }
});
"#;

#[tokio::test]
async fn history_reads_track_the_fed_mirror_newest_first() {
    let (lines, _records) = run_module_session(7321, "InputHistoryRead", HISTORY_READ_TS, |tx| {
        // Two submissions, as the UI would report them: the history update
        // crosses ahead of each typed submission, newest first.
        feed_history(tx, &["look"]);
        submit_typed(tx, "look");
        feed_history(tx, &["hail bob", "look"]);
        submit_typed(tx, "hail bob");
        tx.send(RuntimeAction::Send(Arc::new("readhist".to_string())))
            .unwrap();
    })
    .await;

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "COLD:[]"),
        "the cold mirror reads as empty history.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"LIST:["hail bob","look"]"#),
        "list() reflects the fed history, newest first.\nTranscript:\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == r#"HANDLER_LIST:["hail bob","look"]"#),
        "a sys:input handler sees the submission that fired it in history.\nTranscript:\n{transcript}"
    );
}

/// A script that reads history and nothing else puts nothing on the wire:
/// no ops and, unlike a state read, no mirror interest — history is fed
/// unconditionally, so there is no subscription to flag. History-only so no
/// state read can mask the assertion.
const HISTORY_ONLY_TS: &str = r#"
import { input, echo } from "smudgy:core";

const hist = input.history.list();
echo(hist.length === 0 ? "HIST_ONLY_OK" : "HIST_ONLY_BAD:" + JSON.stringify(hist));
"#;

#[tokio::test]
async fn history_reads_alone_put_nothing_on_the_wire() {
    let (lines, records) =
        run_module_session(7324, "InputHistoryOnly", HISTORY_ONLY_TS, |_| {}).await;

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "HIST_ONLY_OK"),
        "the cold history read resolves to empty.\nTranscript:\n{transcript}"
    );
    assert!(
        records.is_empty(),
        "a history-only script must flag no interest and send no ops; got {records:#?}"
    );
}

/// Push and clear cross to the UI as history `InputOp`s — a multi-word entry
/// is fine (history holds whole commands, unlike completion words) — and once
/// the UI's confirming update comes back, `list()` reflects them.
const HISTORY_MUTATE_TS: &str = r#"
import { input, echo, createAlias } from "smudgy:core";

input.history.push("drink potion");
input.history.push("say hello there");
createAlias("^readhist$", () => echo("LIST:" + JSON.stringify(input.history.list())));
createAlias("^wipehist$", () => input.history.clear());
"#;

#[tokio::test]
async fn history_push_and_clear_cross_as_input_ops_and_read_back() {
    let (lines, records) =
        run_module_session(7322, "InputHistoryMutate", HISTORY_MUTATE_TS, |tx| {
            // The UI's confirming update for the two boot-time pushes.
            feed_history(tx, &["say hello there", "drink potion"]);
            tx.send(RuntimeAction::Send(Arc::new("readhist".to_string())))
                .unwrap();
            // A scripted clear, then the UI's (empty) confirmation.
            tx.send(RuntimeAction::Send(Arc::new("wipehist".to_string())))
                .unwrap();
            feed_history(tx, &[]);
            tx.send(RuntimeAction::Send(Arc::new("readhist".to_string())))
                .unwrap();
        })
        .await;

    // The mutations arrived as ops on the main input, in issue order.
    let ops: Vec<&InputOp> = records
        .iter()
        .filter_map(|r| match r {
            WireRecord::Op(key, op) => {
                assert_eq!(*key, MAIN_PANE_KEY, "ops address the main pane's input");
                Some(op)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ops,
        vec![
            &InputOp::HistoryPush(Arc::new("drink potion".to_string())),
            &InputOp::HistoryPush(Arc::new("say hello there".to_string())),
            &InputOp::HistoryClear,
        ],
        "push/push/clear must arrive in script order"
    );

    let transcript = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|l| l == r#"LIST:["say hello there","drink potion"]"#),
        "pushed entries read back newest first once the UI confirms.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "LIST:[]"),
        "clear() empties the history the reads see.\nTranscript:\n{transcript}"
    );
}

/// A pushed entry is one non-blank line: empty and whitespace-only strings
/// and strings carrying `\n`/`\r` throw at the op boundary, and nothing
/// crosses to the UI. Whitespace-only matters on its own: the UI's history
/// would drop it silently, and a silent no-op is exactly what the op
/// boundary must not permit.
const HISTORY_VALIDATION_TS: &str = r#"
import { input, echo } from "smudgy:core";

const probe = (name: string, fn: () => unknown) => {
    try { fn(); echo(name + ":NO_THROW"); }
    catch (e) { echo(name + ":THREW:" + ((e as any)?.message ?? String(e))); }
};
probe("empty", () => input.history.push(""));
probe("blank", () => input.history.push("   "));
probe("tabbed", () => input.history.push(" \t "));
probe("newline", () => input.history.push("north\nsouth"));
probe("carriage", () => input.history.push("north\rsouth"));
"#;

#[tokio::test]
async fn history_push_rejects_blank_and_multiline_entries() {
    let (lines, records) =
        run_module_session(7323, "InputHistoryValidate", HISTORY_VALIDATION_TS, |_| {}).await;

    let transcript = lines.join("\n");
    for probe in ["empty", "blank", "tabbed", "newline", "carriage"] {
        assert!(
            !lines.iter().any(|l| l == &format!("{probe}:NO_THROW")),
            "push() must reject a {probe} entry.\nTranscript:\n{transcript}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with(&format!("{probe}:THREW:"))),
            "the {probe} rejection must surface.\nTranscript:\n{transcript}"
        );
    }
    assert!(
        !records
            .iter()
            .any(|r| matches!(r, WireRecord::Op(_, InputOp::HistoryPush(_)))),
        "nothing crosses to the UI from a rejected push; got {records:#?}"
    );
}

// ---------------------------------------------------------------------------
// input:change / input:focus (`docs/input.md` §3.5): the observe-only
// notifications derived from the mirror feed. The tests feed
// `RuntimeAction::InputStateChanged` by hand, standing in for the UI's
// coalesced state sync exactly as the mirror tests do.
// ---------------------------------------------------------------------------

/// Feed one main-input state update the way the UI would.
fn feed_state(
    tx: &UnboundedSender<RuntimeAction>,
    key: PaneKey,
    value: &str,
    cursor: usize,
    focused: bool,
    masked: bool,
    source: InputSource,
) {
    tx.send(RuntimeAction::InputStateChanged {
        key,
        snapshot: InputSnapshot {
            value: Arc::new(value.to_string()),
            cursor,
            selection: None,
            focused,
            masked,
        },
        source,
    })
    .unwrap();
}

/// The observer module: change and focus handlers echoing every payload field
/// (missing optionals surface as `undefined`, pinning that they were omitted,
/// not null).
const INPUT_EVENTS_TS: &str = r#"
import { echo } from "smudgy:core";
import { change, focus } from "smudgy:events/input";

change.on((p) =>
    echo(
        "CHG value=" + JSON.stringify(p.value) +
        " masked=" + p.masked +
        " pane=" + p.pane +
        " source=" + p.source,
    ),
);
focus.on((p) =>
    echo("FOC focused=" + p.focused + " masked=" + p.masked + " pane=" + p.pane),
);
"#;

/// The observe events end to end: subscribing flags mirror interest (the
/// Interest wire record, with no write ops), value changes deliver `change`
/// with the attributed source, focus edges deliver `focus`, a
/// cursor-only update delivers neither, a masked update's `change`
/// carries `masked: true` and no value — even when the (misbehaving) state
/// message still carried content — and the mask releasing delivers an
/// ordinary change carrying the restored text, with no masked flag.
#[tokio::test]
async fn input_change_and_focus_events_deliver_edges_with_source() {
    let (lines, records) = run_module_session(7325, "InputEvents", INPUT_EVENTS_TS, |tx| {
        // The warm-up baseline: the default state the UI pushes when
        // interest lands on an untouched input. Seeds the mirror, no events.
        feed_state(tx, MAIN_PANE_KEY, "", 0, false, false, InputSource::Other);
        // Typed text arriving focused: one change (user) + one focus edge.
        feed_state(tx, MAIN_PANE_KEY, "kill ", 5, true, false, InputSource::User);
        // A cursor-only move: no content news, no focus edge — no events.
        feed_state(tx, MAIN_PANE_KEY, "kill ", 2, true, false, InputSource::User);
        // A script finished the command: change attributed to the script.
        feed_state(tx, MAIN_PANE_KEY, "kill rat", 8, true, false, InputSource::Script);
        // Masking engaged (the update wrongly carries content; the mirror
        // suppresses it): one change with masked and no value.
        feed_state(tx, MAIN_PANE_KEY, "hunter2", 7, true, true, InputSource::User);
        // Blur while masked: a focus edge carrying the masked flag.
        feed_state(tx, MAIN_PANE_KEY, "", 0, false, true, InputSource::Other);
        // The mask releases, restoring the pre-mask stash: an ordinary
        // change with the restored value and no masked key.
        feed_state(tx, MAIN_PANE_KEY, "kill rat", 8, false, false, InputSource::Other);
    })
    .await;

    let transcript = lines.join("\n");
    for marker in [
        r#"CHG value="kill " masked=undefined pane=undefined source=user"#,
        r#"CHG value="kill rat" masked=undefined pane=undefined source=script"#,
        "CHG value=undefined masked=true pane=undefined source=user",
        r#"CHG value="kill rat" masked=undefined pane=undefined source=other"#,
        "FOC focused=true masked=undefined pane=undefined",
        "FOC focused=false masked=true pane=undefined",
    ] {
        assert!(
            lines.iter().any(|l| l == marker),
            "expected {marker}.\nTranscript:\n{transcript}"
        );
    }
    // Exactly these events: the baseline and the cursor-only update
    // delivered neither kind.
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("CHG ")).count(),
        4,
        "baseline and cursor-only updates must not fire change.\nTranscript:\n{transcript}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("FOC ")).count(),
        2,
        "focus fires on focus edges only.\nTranscript:\n{transcript}"
    );

    // Subscribing flagged mirror interest — once, with no write ops (the
    // module only observes).
    assert_eq!(
        records,
        vec![WireRecord::Interest],
        "subscription must flag interest exactly once and send nothing else"
    );
}

/// A pane-hosted input's changes carry the pane's name; the main input's
/// carry none (pinned as `undefined` by the test above).
const PANE_INPUT_EVENTS_TS: &str = r#"
import { echo, session } from "smudgy:core";
import { change } from "smudgy:events/input";

session.mainPane.split("right", { name: "chat", input: { onSubmit: () => {} } });
change.on((p) => echo("CHG pane=" + p.pane + " value=" + JSON.stringify(p.value)));
"#;

#[tokio::test]
async fn pane_input_change_events_carry_the_pane_name() {
    let server = "InputEventsPane";
    let server_dir = prepare_server(server);
    std::fs::write(
        server_dir.join("modules").join("input_events.ts"),
        PANE_INPUT_EVENTS_TS,
    )
    .unwrap();

    let mut events = Box::pin(spawn(session_params(7326, server)));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    let pane_key = records
        .iter()
        .find_map(|r| match r {
            WireRecord::PaneOpened {
                key,
                has_input: true,
            } => Some(*key),
            _ => None,
        })
        .expect("the input pane opened");
    // The creation-time baseline the UI sends for a pane input opened under
    // standing interest, then the real change.
    feed_state(&tx, pane_key, "", 0, false, false, InputSource::Other);
    feed_state(&tx, pane_key, "gt hello", 8, true, false, InputSource::User);
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|l| l == r#"CHG pane=chat value="gt hello""#),
        "a pane input's change must name its pane.\nTranscript:\n{transcript}"
    );
}

/// The warm-up push is a baseline, not an edge: state that already existed
/// when the subscription flagged interest (the UI pushes the current state
/// unconditionally at that moment) seeds the mirror without replaying as
/// change/focus events. Only a real change after the baseline fires.
#[tokio::test]
async fn warm_up_state_is_a_baseline_and_fires_no_events() {
    let (lines, _records) = run_module_session(7330, "InputWarmup", INPUT_EVENTS_TS, |tx| {
        // The warm-up push: the input already held focused text when
        // interest was flagged.
        feed_state(tx, MAIN_PANE_KEY, "half typed", 10, true, false, InputSource::User);
        // A real change after the baseline.
        feed_state(tx, MAIN_PANE_KEY, "half typed more", 15, true, false, InputSource::User);
    })
    .await;

    let transcript = lines.join("\n");
    assert!(
        !lines
            .iter()
            .any(|l| l.contains(r#"value="half typed" "#)),
        "the pre-existing state must not replay as a change.\nTranscript:\n{transcript}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("FOC ")).count(),
        0,
        "the pre-existing focus must not replay as an edge.\nTranscript:\n{transcript}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("CHG ")).count(),
        1,
        "exactly the post-baseline change fires.\nTranscript:\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == r#"CHG value="half typed more" masked=undefined pane=undefined source=user"#),
        "the post-baseline change delivers normally.\nTranscript:\n{transcript}"
    );
}

/// The observer module for the stale-pane race: split off an input pane and
/// close it again immediately, leaving subscriptions listening.
const STALE_PANE_EVENTS_TS: &str = r#"
import { echo, session } from "smudgy:core";
import { change, focus } from "smudgy:events/input";

change.on((p) => echo("CHG pane=" + p.pane + " value=" + JSON.stringify(p.value)));
focus.on((p) => echo("FOC pane=" + p.pane));
const pane = session.mainPane.split("right", { name: "chat", input: { onSubmit: () => {} } });
pane.close();
"#;

/// An `InputStateChanged` for a pane whose registry entry is gone (the UI had
/// it in flight when the pane closed) is dropped whole: no events at all —
/// in particular no pane-less payload masquerading as the MAIN input's — and
/// no mirror apply resurrecting the purged key. Fed twice with distinct
/// values so a swallowed-as-baseline first report can't fake the pass.
#[tokio::test]
async fn stale_pane_state_after_close_is_dropped_not_replayed_as_main() {
    let server = "InputEventsStalePane";
    let server_dir = prepare_server(server);
    std::fs::write(
        server_dir.join("modules").join("input_events.ts"),
        STALE_PANE_EVENTS_TS,
    )
    .unwrap();

    let mut events = Box::pin(spawn(session_params(7331, server)));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    let pane_key = records
        .iter()
        .find_map(|r| match r {
            WireRecord::PaneOpened {
                key,
                has_input: true,
            } => Some(*key),
            _ => None,
        })
        .expect("the input pane opened");
    assert!(
        records.contains(&WireRecord::PaneClosed(pane_key)),
        "the pane closed before the stale feed; got {records:#?}"
    );

    // The main input's baseline, then the stale pane updates, then a live
    // main change proving the event machinery still runs.
    feed_state(&tx, MAIN_PANE_KEY, "", 0, false, false, InputSource::Other);
    feed_state(&tx, pane_key, "ghost", 5, true, false, InputSource::User);
    feed_state(&tx, pane_key, "ghost more", 10, true, false, InputSource::User);
    feed_state(&tx, MAIN_PANE_KEY, "north", 5, false, false, InputSource::User);
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        !lines.iter().any(|l| l.contains("ghost")),
        "stale pane state must never surface as an event.\nTranscript:\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("FOC ")),
        "the stale pane's focus must not surface either.\nTranscript:\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == r#"CHG pane=undefined value="north""#),
        "a live main change still delivers after the drops.\nTranscript:\n{transcript}"
    );
}

/// Subscribing to the observe events needs the `input` capability on top of
/// `interop: ["read"]` — without it both subscriptions throw naming `input`,
/// and nothing (no interest, no op) reaches the UI.
#[tokio::test]
async fn input_event_subscription_is_denied_without_the_input_capability() {
    let src = r#"
        import { echo } from "smudgy:core";
        import { change, focus } from "smudgy:events/input";
        try { change.on(() => {}); echo("CHANGE:OK"); }
        catch (e) { echo("CHANGE:DENIED:" + (e?.message ?? String(e))); }
        try { focus.on(() => {}); echo("FOCUS:OK"); }
        catch (e) { echo("FOCUS:DENIED:" + (e?.message ?? String(e))); }
    "#;
    let (lines, records) = run_capability_case(
        7327,
        "pi_caps_input_events_denied",
        "smudgy://wbk/inputwatcher",
        consent_with(|s| s.interop_read = true),
        make_package("wbk", "inputwatcher", "1.0.0", src),
    )
    .await;

    for probe in ["CHANGE", "FOCUS"] {
        assert!(
            !has_line(&lines, &format!("{probe}:OK")),
            "without the input capability, {probe} subscription must throw; transcript:\n{lines:#?}"
        );
        assert!(
            has_line(&lines, &format!("{probe}:DENIED:")),
            "the {probe} denial must surface; transcript:\n{lines:#?}"
        );
    }
    assert!(
        has_line(&lines, "'input'"),
        "the denial must name the missing 'input' capability; transcript:\n{lines:#?}"
    );
    assert!(
        records.is_empty(),
        "a denied subscription must flag no interest; got {records:#?}"
    );
}

/// With `input` granted (plus interop read), a sandboxed package subscribes,
/// its subscription flags mirror interest, and delivered changes reach its
/// handler.
#[tokio::test]
async fn input_event_subscription_works_with_the_input_capability() {
    let src = r#"
        import { echo } from "smudgy:core";
        import { change } from "smudgy:events/input";
        try {
            change.on((p) => echo("GOT:" + p.value + ":" + p.source));
            echo("SUB:OK");
        } catch (e) {
            echo("SUB:DENIED:" + (e?.message ?? String(e)));
        }
    "#;
    let server = "pi_caps_input_events_granted";
    let spec = "smudgy://wbk/inputgauge";
    prepare_server(server);
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(
        server,
        spec,
        &consent_with(|s| {
            s.interop_read = true;
            s.input = true;
        }),
    )
    .unwrap();

    let mut events = Box::pin(spawn_with_package_provider(
        session_params(7328, server),
        factory_for(vec![make_package("wbk", "inputgauge", "1.0.0", src)]),
    ));
    let mut lines = Vec::new();
    let mut records = Vec::new();
    let tx = wait_runtime_ready(&mut events, &mut lines, &mut records).await;
    drain_quiet(&mut events, &mut lines, &mut records).await;

    // The warm-up baseline first (an input's first report never fires
    // events), then the change the handler must receive.
    feed_state(&tx, MAIN_PANE_KEY, "", 0, false, false, InputSource::Other);
    feed_state(&tx, MAIN_PANE_KEY, "north", 5, true, false, InputSource::User);
    drain_quiet(&mut events, &mut lines, &mut records).await;
    tx.send(RuntimeAction::Shutdown).ok();

    assert!(
        has_line(&lines, "SUB:OK"),
        "the granted package must subscribe; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "GOT:north:user"),
        "the granted package's handler must receive delivered changes; transcript:\n{lines:#?}"
    );
    assert!(
        records.contains(&WireRecord::Interest),
        "the package's subscription must flag mirror interest; got {records:#?}"
    );
}

// ---------------------------------------------------------------------------
// Telnet ECHO auto-mask (`docs/input.md` §3.10): the connection's
// negotiation action forwards to the UI as `SessionEvent::ServerEcho` (the
// negotiation answers themselves are unit-tested at the telnet layer and the
// connection ingest bridge; the mask compose rule is unit-tested UI-side).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn server_echo_actions_forward_to_the_ui_in_order() {
    let (_lines, records) = run_module_session(7329, "ServerEchoWire", "", |tx| {
        tx.send(RuntimeAction::ServerEchoChanged { enabled: true })
            .unwrap();
        tx.send(RuntimeAction::ServerEchoChanged { enabled: false })
            .unwrap();
    })
    .await;

    let echoes: Vec<&WireRecord> = records
        .iter()
        .filter(|r| matches!(r, WireRecord::ServerEcho(_)))
        .collect();
    assert_eq!(
        echoes,
        vec![&WireRecord::ServerEcho(true), &WireRecord::ServerEcho(false)],
        "the negotiation actions must forward as ServerEcho events in order"
    );
}
