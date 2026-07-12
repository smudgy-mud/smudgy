//! Handle-based automation CRUD + the `triggers`/`aliases` registries, driven
//! end-to-end through a real session.
//!
//! Covers the four criteria:
//!  - a `delete()`d alias is gone from `aliases.list()` and freed from the matcher
//!    (a later matching input no longer fires it);
//!  - `createTrigger(.., { fireLimit: 1 })` fires once, then self-removes;
//!  - `createTrigger(.., { lineLimit: N })` self-removes after N tested lines;
//!  - `triggers.exists`/`triggers.get(..).pattern` read back the live set.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// A module exercising the handle/registry surface. An `intro` alias (driven by `Send`) runs the
/// synchronous introspection assertions and echoes a single sentinel; the one-shot/line-limited
/// triggers (driven by incoming lines) exercise the self-limits. Most automations are unnamed,
/// so their registry identity is their derived name (the pattern source); the one-shot trigger
/// carries an explicit `options.name` to cover the explicit-identity path too.
const CRUD_TS: &str = r#"
import { createAlias, createTrigger, echo, send, triggers, aliases } from "smudgy:core";

// A plain alias we will delete to prove `delete()` frees the matcher slot. Unnamed: the
// registry addresses it by its pattern source.
const doomed = createAlias("^doomed$", () => { echo("DOOMED_FIRED"); });

// A one-shot trigger with an explicit name: fires on the first "^tick$" line, then
// self-removes (fireLimit:1).
createTrigger("^tick$", () => { echo("ONCE_FIRED"); }, { fireLimit: 1, name: "once" });

// A line-limited trigger that never matches its pattern but is *tested* against every incoming
// line; it must self-remove after 2 tested lines even though it never fires. Unnamed: its
// registry identity is its pattern source.
createTrigger("^willnevermatch$", () => { echo("COUNTED_FIRED"); }, { lineLimit: 2 });

// `intro`: registry reads + delete, reported as one sentinel. `delete()` queues the removal
// (like create), so the drop is observed by a later handler (`check_doomed`), not synchronously.
createAlias("^intro$", () => {
    const existsBefore = aliases.exists("^doomed$");
    const listBefore = aliases.list().includes("^doomed$");
    const handleName = doomed.name === "^doomed$";
    const pat = triggers.get("once") ? triggers.get("once").pattern : "MISSING";
    const triggerExists = triggers.exists("once");
    aliases.get("^doomed$").delete();
    const ok =
        existsBefore === true &&
        listBefore === true &&
        handleName === true &&
        pat === "^tick$" &&
        triggerExists === true;
    echo(ok ? "INTRO_OK" : ("INTRO_FAIL eb=" + existsBefore + " lb=" + listBefore + " hn=" + handleName + " pat=" + pat + " te=" + triggerExists));
});

// `check_doomed`: reports whether the deleted alias is gone from the registry (after the queued
// RemoveAlias has been processed).
createAlias("^check_doomed$", () => {
    echo(aliases.exists("^doomed$") ? "DOOMED_STILL_PRESENT" : "DOOMED_GONE");
});

// `check_once`: reports whether the `once` trigger is gone after it fired (fireLimit self-remove).
createAlias("^check_once$", () => {
    echo(triggers.exists("once") ? "ONCE_STILL_PRESENT" : "ONCE_GONE");
});

// `check_counted`: reports whether the line-limited trigger self-removed after its lineLimit
// (looked up by its derived name).
createAlias("^check_counted$", () => {
    echo(triggers.exists("^willnevermatch$") ? "COUNTED_STILL_PRESENT" : "COUNTED_GONE");
});

echo("CRUD_READY");
"#;

/// Drive `CRUD_TS` and assert all four CRUD/registry acceptance criteria.
#[tokio::test]
async fn handle_crud_and_registries() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "HandleCrud";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("crud.ts"), CRUD_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7101),
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

    // Wait until the module's top-level has run (its registrations are queued ahead of anything
    // we send only once `CRUD_READY` is observed — the create actions are processed before the
    // sentinel echo that follows them in the module body).
    let mut preamble: Vec<String> = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for CRUD_READY")
            .expect("event stream ended before CRUD_READY");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    preamble.push(line.text.clone());
                }
            }
        }
        if preamble.iter().any(|l| l == "CRUD_READY") {
            break;
        }
    }

    // Feed a server line so triggers are evaluated against it.
    let feed_line = |text: &str| {
        tx.send(RuntimeAction::HandleIncomingLine(Arc::new(StyledLine::new(
            text,
            Vec::new(),
        ))))
        .unwrap();
    };
    let send_input = |text: &str| {
        tx.send(RuntimeAction::Send(Arc::new(text.to_string()))).unwrap();
    };

    // Drive the whole scenario in FIFO order in one shot: every action lands at the back of the
    // session's queue and is processed in turn (the module's top-level registrations are already
    // ahead of these, since `RuntimeReady` is emitted after the engine builds + auto-loads). Each
    // self-limited trigger removal also queues a `RemoveTrigger` that the runtime applies in order,
    // so by the time `check_*` runs the removals have been processed.
    send_input("intro"); // registry get/list/exists/pattern + delete `doomed`
    send_input("doomed"); // deleted alias: must pass through unmatched (no DOOMED_FIRED)
    feed_line("tick"); // fires `once` (fireLimit:1) -> self-removes
    feed_line("tick"); // `once` already gone: no second ONCE_FIRED
    feed_line("some line"); // counted: tested line 1
    feed_line("another line"); // counted: tested line 2 -> lineLimit self-remove
    send_input("check_once"); // reports ONCE_GONE
    send_input("check_counted"); // reports COUNTED_GONE
    send_input("check_doomed"); // reports DOOMED_GONE

    // Drain until the session goes quiet.
    let mut lines: Vec<String> = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    let has = |s: &str| lines.iter().any(|l| l == s);
    let count = |s: &str| lines.iter().filter(|l| *l == s).count();

    // Registry reads + delete.
    assert!(has("INTRO_OK"), "registry get/list/exists/pattern + delete must work.\n{transcript}");
    // The doomed alias is freed from the matcher: its input passes through unmatched (no
    // DOOMED_FIRED after the delete; it was never sent before the delete).
    assert!(
        !has("DOOMED_FIRED"),
        "a deleted alias must not fire on a later matching input.\n{transcript}"
    );
    // ...and it is gone from the registry too.
    assert!(has("DOOMED_GONE"), "a deleted alias must drop from the registry.\n{transcript}");
    // fireLimit:1 — fires exactly once, then self-removes.
    assert_eq!(count("ONCE_FIRED"), 1, "fireLimit:1 trigger must fire exactly once.\n{transcript}");
    assert!(has("ONCE_GONE"), "fireLimit:1 trigger must self-remove after firing.\n{transcript}");
    // lineLimit:2 — self-removes after 2 tested lines.
    assert!(has("COUNTED_GONE"), "lineLimit trigger must self-remove after its line budget.\n{transcript}");
    assert!(
        !has("COUNTED_FIRED"),
        "the lineLimit trigger never matches, so it must never fire.\n{transcript}"
    );
}
