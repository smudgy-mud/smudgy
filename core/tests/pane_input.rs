//! End-to-end coverage of per-pane inputs (`docs/input.md` §3.7):
//! a `PaneSpec.input` creates a pane whose def carries the input, submissions
//! driven over the wire (`RuntimeAction::PaneInputSubmit`, exactly what the
//! UI's pane submit routing sends) reach the registered `onSubmit` handler
//! and nothing else — no alias pipeline, no `sys:input` — the `pane.input`
//! handle exercises every Phase 1-4 surface addressed at the pane (writes,
//! word sets, history) without touching the main input, the capability gate
//! depends on the target (own pane → `panes`; main → `input`), a pane
//! without an input teaches, and a reload re-registers the handler through
//! the re-claiming split.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::runtime::input::InputOp;
use smudgy_core::session::runtime::pane::{MAIN_PANE_KEY, PaneKey};
use smudgy_core::session::{
    PackageProviderFactory, SessionEvent, SessionId, SessionParams, TaggedSessionEvent, spawn,
    spawn_with_package_provider,
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

/// One event as seen on the session event stream, in arrival order: echoed
/// lines, opened panes (the def facts pane-input tests pin), and the
/// forwarded input ops / word-set pushes keyed by pane.
#[derive(Debug, PartialEq, Eq)]
enum WireRecord {
    Op(PaneKey, InputOp),
    Words { key: PaneKey, suggestions: Vec<String> },
    PaneOpened { key: PaneKey, has_input: bool, placeholder: Option<String> },
}

#[derive(Default)]
struct Recording {
    lines: Vec<String>,
    records: Vec<WireRecord>,
}

impl Recording {
    fn record(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let smudgy_core::session::BufferUpdate::Append(line) = update {
                        self.lines.push(line.text.clone());
                    }
                }
            }
            SessionEvent::InputOp { key, op } => self.records.push(WireRecord::Op(key, op)),
            SessionEvent::InputWordSets { key, suggestions, .. } => {
                self.records.push(WireRecord::Words {
                    key,
                    suggestions: suggestions.iter().map(|w| w.as_str().to_string()).collect(),
                });
            }
            SessionEvent::PaneOpened { def, .. } => self.records.push(WireRecord::PaneOpened {
                key: def.key,
                has_input: def.input.is_some(),
                placeholder: def
                    .input
                    .as_ref()
                    .and_then(|input| input.placeholder.as_deref().map(str::to_string)),
            }),
            _ => {}
        }
    }

    fn has_line(&self, needle: &str) -> bool {
        self.lines.iter().any(|l| l.contains(needle))
    }

    fn count_lines(&self, needle: &str) -> usize {
        self.lines.iter().filter(|l| l.contains(needle)).count()
    }

    /// The most recently opened pane that hosts an input.
    fn opened_input_pane(&self) -> Option<PaneKey> {
        self.records.iter().rev().find_map(|record| match record {
            WireRecord::PaneOpened { key, has_input: true, .. } => Some(*key),
            _ => None,
        })
    }
}

/// Wait for `RuntimeReady`, recording everything on the way.
async fn wait_runtime_ready(
    events: &mut (impl futures::Stream<Item = TaggedSessionEvent> + Unpin),
    recording: &mut Recording,
) -> UnboundedSender<RuntimeAction> {
    loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => return tx,
            other => recording.record(other),
        }
    }
}

/// Drain the event stream until it goes quiet, recording on the way.
async fn drain_quiet(
    events: &mut (impl futures::Stream<Item = TaggedSessionEvent> + Unpin),
    recording: &mut Recording,
) {
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        recording.record(event.event);
    }
}

/// The handler-owns-the-text module: a pane input whose `onSubmit` echoes the
/// delivered text, an alias that would fire if the text entered the pipeline,
/// and a `sys:input` subscriber that would fire if a pane submission were a
/// typed submission. Also pins the handle shape: `pane.input` present on the
/// created pane, `undefined` on main.
const ON_SUBMIT_HARNESS_TS: &str = r#"
import { session, echo, createAlias } from "smudgy:core";
import { submit } from "smudgy:events/sys";

submit.on(() => echo("SYS_INPUT_FIRED"));
createAlias("^hello pane$", () => echo("ALIAS_FIRED"));

const pane = session.mainPane.split("right", {
    name: "chat",
    width: 200,
    input: {
        onSubmit: (text: string) => echo("ONSUBMIT:" + text),
        placeholder: "chat...",
    },
});
echo("PANE_INPUT_" + (pane.input !== undefined ? "PRESENT" : "MISSING"));
echo("MAIN_PANE_INPUT_" + (session.mainPane.input === undefined ? "UNDEFINED" : "PRESENT"));
"#;

/// A pane submission driven over the wire reaches `onSubmit` and nothing
/// else; a typed submission (the sanity half) still reaches everything. A
/// reload then re-registers the handler through the re-claiming split — the
/// pane (and its key) survive, and the same key delivers again.
#[tokio::test]
async fn pane_submissions_deliver_to_on_submit_only_and_survive_reload() {
    let server = "PaneInputSubmit";
    let server_dir = prepare_server(server);
    std::fs::write(server_dir.join("modules").join("pane.ts"), ON_SUBMIT_HARNESS_TS).unwrap();

    let mut events = Box::pin(spawn(session_params(7401, server)));
    let mut recording = Recording::default();
    let tx = wait_runtime_ready(&mut events, &mut recording).await;
    drain_quiet(&mut events, &mut recording).await;

    assert!(
        recording.has_line("PANE_INPUT_PRESENT"),
        "the created pane exposes its input handle.\n{:#?}",
        recording.lines
    );
    assert!(
        recording.has_line("MAIN_PANE_INPUT_UNDEFINED"),
        "the main pane's input is the session input, not a pane input.\n{:#?}",
        recording.lines
    );
    let key = recording.opened_input_pane().expect("PaneOpened with an input");
    assert_ne!(key, MAIN_PANE_KEY);
    assert!(
        recording.records.iter().any(|r| matches!(
            r,
            WireRecord::PaneOpened { has_input: true, placeholder: Some(p), .. }
                if p == "chat..."
        )),
        "the def carries the placeholder; got {:#?}",
        recording.records
    );

    // The pane submission: onSubmit receives the text; the alias that matches
    // it does NOT fire, and sys:input does NOT fire.
    tx.send(RuntimeAction::PaneInputSubmit {
        key,
        text: Arc::new("hello pane".to_string()),
    })
    .unwrap();
    drain_quiet(&mut events, &mut recording).await;
    assert!(
        recording.has_line("ONSUBMIT:hello pane"),
        "the handler receives the typed text.\n{:#?}",
        recording.lines
    );
    assert!(
        !recording.has_line("ALIAS_FIRED"),
        "a pane submission never enters the alias pipeline.\n{:#?}",
        recording.lines
    );
    assert!(
        !recording.has_line("SYS_INPUT_FIRED"),
        "sys:input does not fire for pane submissions.\n{:#?}",
        recording.lines
    );

    // Sanity: a typed MAIN submission of the same text reaches both.
    tx.send(RuntimeAction::SubmitInput(Arc::new("hello pane".to_string())))
        .unwrap();
    drain_quiet(&mut events, &mut recording).await;
    assert!(recording.has_line("SYS_INPUT_FIRED"), "the subscriber is live");
    assert!(recording.has_line("ALIAS_FIRED"), "the alias is live");
    assert_eq!(
        recording.count_lines("ONSUBMIT:"),
        1,
        "a main submission never reaches a pane handler.\n{:#?}",
        recording.lines
    );

    // Reload: the module re-runs, its split re-claims the pane (same key —
    // no new PaneOpened), and the re-registered handler delivers again.
    tx.send(RuntimeAction::Reload).unwrap();
    let tx = wait_runtime_ready(&mut events, &mut recording).await;
    drain_quiet(&mut events, &mut recording).await;
    assert_eq!(
        recording
            .records
            .iter()
            .filter(|r| matches!(r, WireRecord::PaneOpened { .. }))
            .count(),
        1,
        "the re-claimed pane is not re-opened; got {:#?}",
        recording.records
    );
    tx.send(RuntimeAction::PaneInputSubmit {
        key,
        text: Arc::new("after reload".to_string()),
    })
    .unwrap();
    drain_quiet(&mut events, &mut recording).await;
    assert!(
        recording.has_line("ONSUBMIT:after reload"),
        "the reloaded script's re-split re-registered its handler.\n{:#?}",
        recording.lines
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// The Phase 1-4 surfaces addressed at a pane: writes ride `InputOp` keyed by
/// the pane, word-set contributions merge per input (the main input's stay
/// empty), and history mutation/reads scope per input (the pane's mirror feed
/// leaves the main list untouched).
const HANDLE_HARNESS_TS: &str = r#"
import { session, input, echo, createAlias } from "smudgy:core";

const pane = session.mainPane.split("right", {
    name: "notes",
    input: { onSubmit: () => {} },
});
const pin = pane.input!;
pin.propose("draft");
input.propose("main draft");
pin.completion.add("alpha");
echo("PANE_WORDS:" + JSON.stringify(pin.completion.list()));
echo("MAIN_WORDS:" + JSON.stringify(input.completion.list()));
pin.history.push("note one");
createAlias("^checkhist$", () => {
    echo("PANE_HIST:" + JSON.stringify(pin.history.list()));
    echo("MAIN_HIST:" + JSON.stringify(input.history.list()));
});
"#;

#[tokio::test]
async fn pane_input_handle_addresses_the_pane_and_leaves_main_untouched() {
    let server = "PaneInputHandle";
    let server_dir = prepare_server(server);
    std::fs::write(server_dir.join("modules").join("handle.ts"), HANDLE_HARNESS_TS).unwrap();

    let mut events = Box::pin(spawn(session_params(7402, server)));
    let mut recording = Recording::default();
    let tx = wait_runtime_ready(&mut events, &mut recording).await;
    drain_quiet(&mut events, &mut recording).await;

    let key = recording.opened_input_pane().expect("PaneOpened with an input");

    // The pane write and the main write each address their own input.
    assert!(
        recording
            .records
            .iter()
            .any(|r| *r == WireRecord::Op(key, InputOp::Propose(Arc::new("draft".to_string())))),
        "the pane propose targets the pane's key; got {:#?}",
        recording.records
    );
    assert!(
        recording.records.iter().any(|r| *r
            == WireRecord::Op(
                MAIN_PANE_KEY,
                InputOp::Propose(Arc::new("main draft".to_string()))
            )),
        "the main propose still targets main; got {:#?}",
        recording.records
    );

    // Word sets: the pane's contribution merges under the pane's key; the
    // main input's registry stays empty (per-input scoping).
    assert!(recording.has_line(r#"PANE_WORDS:["alpha"]"#), "{:#?}", recording.lines);
    assert!(recording.has_line("MAIN_WORDS:[]"), "{:#?}", recording.lines);
    assert!(
        recording
            .records
            .iter()
            .any(|r| matches!(r, WireRecord::Words { key: k, suggestions } if *k == key && suggestions == &vec!["alpha".to_string()])),
        "the merged push is keyed by the pane; got {:#?}",
        recording.records
    );
    assert!(
        !recording
            .records
            .iter()
            .any(|r| matches!(r, WireRecord::Words { key: k, .. } if *k == MAIN_PANE_KEY)),
        "no word-set push targets main; got {:#?}",
        recording.records
    );

    // History: the push rides the pane's key; a mirror update for the pane
    // (what the UI would send once the push lands) is readable through the
    // pane handle while the main list stays empty.
    assert!(
        recording.records.iter().any(|r| *r
            == WireRecord::Op(key, InputOp::HistoryPush(Arc::new("note one".to_string())))),
        "the history push targets the pane's key; got {:#?}",
        recording.records
    );
    tx.send(RuntimeAction::InputHistoryChanged {
        key,
        entries: Arc::new(vec![Arc::new("note one".to_string())]),
    })
    .unwrap();
    tx.send(RuntimeAction::Send(Arc::new("checkhist".to_string())))
        .unwrap();
    drain_quiet(&mut events, &mut recording).await;
    assert!(recording.has_line(r#"PANE_HIST:["note one"]"#), "{:#?}", recording.lines);
    assert!(recording.has_line("MAIN_HIST:[]"), "{:#?}", recording.lines);

    tx.send(RuntimeAction::Shutdown).ok();
}

/// A pane created without an input, plus the teaching errors: `pane.input` is
/// undefined, a stale handle to a recreated no-input pane throws "has no
/// input", a split asking for an input on an existing no-input pane throws
/// the mismatch, and `main` can never take a pane input.
const NO_INPUT_HARNESS_TS: &str = r#"
import { session, echo } from "smudgy:core";

const withInput = session.mainPane.split("right", {
    name: "temp",
    input: { onSubmit: () => {} },
});
const handle = withInput.input!;
withInput.close();
session.mainPane.split("right", { name: "temp" });
const recreated = session.panes.get("temp")!;
echo("RECREATED_INPUT_" + (recreated.input === undefined ? "UNDEFINED" : "PRESENT"));
try { handle.propose("x"); echo("STALE_NO_THROW"); }
catch (e) { echo("STALE_THREW:" + (e?.message ?? String(e))); }
try {
    session.mainPane.split("right", { name: "temp", input: { onSubmit: () => {} } });
    echo("MISMATCH_NO_THROW");
} catch (e) { echo("MISMATCH_THREW:" + (e?.message ?? String(e))); }
try {
    session.mainPane.split("right", { name: "main", input: { onSubmit: () => {} } });
    echo("MAIN_NO_THROW");
} catch (e) { echo("MAIN_THREW:" + (e?.message ?? String(e))); }
"#;

#[tokio::test]
async fn panes_without_inputs_teach_instead_of_half_working() {
    let server = "PaneInputAbsent";
    let server_dir = prepare_server(server);
    std::fs::write(server_dir.join("modules").join("noinput.ts"), NO_INPUT_HARNESS_TS).unwrap();

    let mut events = Box::pin(spawn(session_params(7403, server)));
    let mut recording = Recording::default();
    let tx = wait_runtime_ready(&mut events, &mut recording).await;
    drain_quiet(&mut events, &mut recording).await;

    assert!(recording.has_line("RECREATED_INPUT_UNDEFINED"), "{:#?}", recording.lines);
    assert!(
        recording.has_line("STALE_THREW:") && recording.has_line("has no input"),
        "a stale handle to a no-input pane teaches.\n{:#?}",
        recording.lines
    );
    assert!(
        recording.has_line("MISMATCH_THREW:")
            && recording.has_line("already exists without an input"),
        "asking for an input on a no-input pane teaches.\n{:#?}",
        recording.lines
    );
    assert!(
        recording.has_line("MAIN_THREW:") && recording.has_line("session's command input"),
        "main never takes a pane input.\n{:#?}",
        recording.lines
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// The split + word-add + close cycle: a closed pane's input state dies with
/// it, so a reload must not resync word sets for the dead key. Without the
/// close-path purge the retired entry survives in `InputWordSets`, and every
/// reload's `reset_engine_state` re-flags it — pushing an `InputWordSets`
/// event for a key the UI no longer knows.
const CLOSE_CYCLE_TS: &str = r#"
import { session, echo } from "smudgy:core";

const pane = session.mainPane.split("right", {
    name: "fleeting",
    input: { onSubmit: () => {} },
});
pane.input!.completion.add("ghost");
pane.close();
echo("CYCLE_DONE");
"#;

#[tokio::test]
async fn a_reload_never_resyncs_word_sets_for_a_closed_panes_key() {
    let server = "PaneInputCloseReload";
    let server_dir = prepare_server(server);
    std::fs::write(server_dir.join("modules").join("cycle.ts"), CLOSE_CYCLE_TS).unwrap();

    let mut events = Box::pin(spawn(session_params(7405, server)));
    let mut recording = Recording::default();
    let tx = wait_runtime_ready(&mut events, &mut recording).await;
    drain_quiet(&mut events, &mut recording).await;

    assert!(recording.has_line("CYCLE_DONE"), "{:#?}", recording.lines);
    let dead = recording
        .opened_input_pane()
        .expect("the pane opened before closing");

    let mark = recording.records.len();
    tx.send(RuntimeAction::Reload).unwrap();
    let tx = wait_runtime_ready(&mut events, &mut recording).await;
    drain_quiet(&mut events, &mut recording).await;
    assert_eq!(
        recording.count_lines("CYCLE_DONE"),
        2,
        "the module re-ran on reload.\n{:#?}",
        recording.lines
    );

    let stale: Vec<&WireRecord> = recording.records[mark..]
        .iter()
        .filter(|r| matches!(r, WireRecord::Words { key, .. } if *key == dead))
        .collect();
    assert!(
        stale.is_empty(),
        "no word-set push may name the closed pane's key after the reload; got {stale:#?}"
    );
    // The re-run's own cycle minted a fresh key (keys are never reused).
    let recreated = recording
        .opened_input_pane()
        .expect("the re-run's pane opened");
    assert_ne!(recreated, dead);

    tx.send(RuntimeAction::Shutdown).ok();
}

// ---------------------------------------------------------------------------
// Capability matrix (the sandboxed-package harness of input_api.rs).
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

/// A package granted `panes` but NOT `input` owns its pane's input end to
/// end — creation, onSubmit delivery, writes, reads, word sets, history —
/// while the MAIN input stays denied under the `input` capability.
const PANES_ONLY_PACKAGE_JS: &str = r#"
import { session, echo } from "smudgy:core";
const probe = (name, fn) => {
    try { fn(); echo(name + ":OK"); }
    catch (e) { echo(name + ":DENIED:" + (e?.message ?? String(e))); }
};
const pane = session.mainPane.split("right", {
    name: "own",
    input: { onSubmit: (text) => echo("PKG_ONSUBMIT:" + text) },
});
const pin = pane.input;
probe("write", () => pin.propose("x"));
probe("read", () => { void pin.value; });
probe("words", () => pin.completion.add("north"));
probe("hist", () => pin.history.push("cmd"));
probe("mainread", () => { void session.input.value; });
probe("mainwrite", () => session.input.propose("x"));
echo("DONE");
"#;

#[tokio::test]
async fn panes_capability_covers_own_pane_input_but_not_main() {
    let server = "PaneInputCaps";
    prepare_server(server);
    let spec = "smudgy://wbk/paneful";
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    let consent = PackagePermissions {
        smudgy: SmudgyCapabilities {
            echo: true,
            panes: true,
            ..Default::default()
        },
        ..Default::default()
    };
    shared_packages::record_consent(server, spec, &consent).unwrap();

    let mut events = Box::pin(spawn_with_package_provider(
        session_params(7404, server),
        factory_for(vec![make_package("wbk", "paneful", "1.0.0", PANES_ONLY_PACKAGE_JS)]),
    ));
    let mut recording = Recording::default();
    let tx = wait_runtime_ready(&mut events, &mut recording).await;
    drain_quiet(&mut events, &mut recording).await;

    assert!(recording.has_line("DONE"), "the probe ran to completion.\n{:#?}", recording.lines);
    for probe in ["write", "read", "words", "hist"] {
        assert!(
            recording.has_line(&format!("{probe}:OK")),
            "`panes` covers the package's own pane input (`{probe}`).\n{:#?}",
            recording.lines
        );
    }
    for probe in ["mainread", "mainwrite"] {
        assert!(
            recording.has_line(&format!("{probe}:DENIED:")),
            "the main input still requires `input` (`{probe}`).\n{:#?}",
            recording.lines
        );
    }
    assert!(
        recording.has_line("'input'"),
        "the main-input denial names the missing capability.\n{:#?}",
        recording.lines
    );

    // The pane write went out keyed by the package's pane, and its word
    // contribution merged there.
    let key = recording.opened_input_pane().expect("the package's pane opened");
    assert!(
        recording
            .records
            .iter()
            .any(|r| *r == WireRecord::Op(key, InputOp::Propose(Arc::new("x".to_string())))),
        "the pane write reached the UI; got {:#?}",
        recording.records
    );

    // And the submission delivers into the sandboxed isolate's handler.
    tx.send(RuntimeAction::PaneInputSubmit {
        key,
        text: Arc::new("from the wire".to_string()),
    })
    .unwrap();
    drain_quiet(&mut events, &mut recording).await;
    assert!(
        recording.has_line("PKG_ONSUBMIT:from the wire"),
        "onSubmit runs in the creating (sandboxed) isolate.\n{:#?}",
        recording.lines
    );

    tx.send(RuntimeAction::Shutdown).ok();
}
