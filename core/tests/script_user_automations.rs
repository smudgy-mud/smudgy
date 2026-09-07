//! End-to-end: a script creates, reads, edits, and deletes the REGULAR, persisted user-side
//! automations via `userAutomations.<kind>.*` (`smudgy:core`).
//!
//! Unlike the ephemeral `createAlias`/`createTrigger` runtime automations, these write the
//! server's `aliases.json` / `triggers.json` — the same files the automations window edits — so
//! the test observes the on-disk result directly with `load_aliases`/`load_triggers`, and the
//! handle's `update()` is checked by reading the changed field back.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::ScriptLang;
use smudgy_core::models::aliases::load_aliases;
use smudgy_core::models::triggers::load_triggers;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{SessionEvent, SessionId, SessionParams, spawn};

const COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);

/// A module exposing two controller aliases. "makeauto" saves an alias + a trigger via the
/// registry, edits the alias through its handle (`update`), reads it back (`get().def()`), and
/// introspects (`list`/`exists`); "delauto" removes the alias.
const MODULE_TS: &str = r#"
import { createAlias, echo, userAutomations } from "smudgy:core";
createAlias("^makeauto$", () => {
    const a = userAutomations.aliases.save("greet", { pattern: "^hi$", script: "echo('WAVE')", language: "js" });
    userAutomations.triggers.save("onsay", { patterns: ["^(\\w+) says"], rawPatterns: ["TICK"], script: "listen" });
    const langOk = a.def().language === "js";
    const upd = userAutomations.aliases.get("greet").update({ enabled: false });
    userAutomations.aliases.save("live", { pattern: "^ping$", script: "echo('PONG')", language: "js" });
    // A get -> save round trip must not strip the editor's matcher sidecar or
    // the self-match opt-in: both ride the def.
    userAutomations.aliases.save("cmdlike", {
        pattern: "^gt(?:\\s|$)",
        script: "guildtell $words",
        allowSelfMatch: true,
        matcher: { kind: "command", name: "gt", args: [{ name: "words", kind: "rest" }] },
    });
    userAutomations.aliases.save("cmdlike", userAutomations.aliases.get("cmdlike").def());
    const back = userAutomations.aliases.get("cmdlike").def();
    const rt = back.allowSelfMatch === true
        && (back.matcher as any)?.kind === "command"
        && (back.matcher as any)?.name === "gt";
    const nowDisabled = userAutomations.aliases.get("greet").def().enabled === false;
    const listed = userAutomations.aliases.list().join(",");
    const trig = userAutomations.triggers.exists("onsay");
    echo("MADE lang=" + langOk + " upd=" + upd + " disabled=" + nowDisabled + " rt=" + rt + " list=" + listed + " trig=" + trig);
});
createAlias("^delauto$", () => {
    const removed = userAutomations.aliases.delete("greet");
    userAutomations.aliases.delete("live");
    userAutomations.aliases.delete("cmdlike");
    echo("DEL removed=" + removed);
});
"#;

/// Commands and their synchronous descendants finish before the next queued echo. Wait for that
/// marker even across quiet gaps, collecting rebuild events and output for the assertions below.
async fn collect_through_marker(
    events: &mut std::pin::Pin<
        Box<impl futures::Stream<Item = smudgy_core::session::TaggedSessionEvent>>,
    >,
    marker: &str,
) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut runtime_ready = 0;
    let deadline = tokio::time::Instant::now() + COMPLETION_TIMEOUT;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.next()).await {
        match event.event {
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let smudgy_core::session::BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            SessionEvent::RuntimeReady(_) => runtime_ready += 1,
            _ => {}
        }
        if lines.iter().any(|line| line == marker) {
            return (lines, runtime_ready);
        }
    }
    panic!(
        "session ended or timed out before {marker}: output={lines:?}, rebuilds={runtime_ready}"
    );
}

#[tokio::test]
async fn completion_wait_survives_a_gap_between_events() {
    use smudgy_core::session::styled_line::StyledLine;
    use smudgy_core::session::{BufferUpdate, TaggedSessionEvent};

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let ready = TaggedSessionEvent {
        session_id: SessionId::from(9403u32),
        event: SessionEvent::RuntimeReady(tx),
    };
    let delayed = futures::stream::once(async {
        // A busy runner can legitimately pause longer than the old 900 ms quiet period.
        tokio::time::sleep(Duration::from_secs(2)).await;
        TaggedSessionEvent {
            session_id: SessionId::from(9403u32),
            event: SessionEvent::UpdateBuffer(Arc::new(vec![BufferUpdate::Append(Arc::new(
                StyledLine::from_echo_str("COMMAND_DONE"),
            ))])),
        }
    });
    let mut events = Box::pin(futures::stream::iter([ready]).chain(delayed));
    let (lines, runtime_ready) = collect_through_marker(&mut events, "COMMAND_DONE").await;
    assert_eq!(lines, ["COMMAND_DONE"]);
    assert_eq!(runtime_ready, 1);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn script_crud_persisted_user_automations() {
    let server_name = "test_user_automations".to_string();
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().unwrap();
    std::fs::create_dir_all(home.join(&server_name).join("logs")).unwrap();
    let modules_dir = home.join(&server_name).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::write(modules_dir.join("ctrl.ts"), MODULE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9401u32),
        server_name: Arc::new(server_name.clone()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let other_params = Arc::new(SessionParams {
        session_id: SessionId::from(9402u32),
        server_name: Arc::new(server_name.clone()),
        profile_name: Arc::new("other".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut other_events = Box::pin(spawn(other_params));

    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };
    let other_tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), other_events.next())
            .await
            .expect("timed out waiting for other RuntimeReady")
            .expect("other event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Fire the create/edit controller (the writes persist synchronously; the calling session is
    // not reloaded, so the controller aliases stay registered for the delete step below).
    tx.send(RuntimeAction::Send(Arc::new("makeauto".to_string())))
        .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new("MAKE_DONE".to_string())))
        .unwrap();
    let (made, own_reloads) = collect_through_marker(&mut events, "MAKE_DONE").await;
    assert!(
        made.iter().any(|l| l
            == "MADE lang=true upd=true disabled=true rt=true list=cmdlike,greet,live trig=true"),
        "create/edit controller did not report success: {made:?}"
    );
    assert_eq!(
        own_reloads, 0,
        "user automation changes rebuilt the calling runtime"
    );

    // Saves queue the other session's sync actions before MAKE_DONE. The next command therefore
    // observes that snapshot, without a sleep or an explicit sync that could hide broken fan-out.
    other_tx
        .send(RuntimeAction::Send(Arc::new("ping".to_string())))
        .unwrap();
    other_tx
        .send(RuntimeAction::Echo(Arc::new("PING_DONE".to_string())))
        .unwrap();
    let (other_ping, other_reloads) = collect_through_marker(&mut other_events, "PING_DONE").await;
    assert!(
        other_ping.iter().any(|line| line == "PONG"),
        "new alias was not synchronized to the other session: {other_ping:?}"
    );
    assert_eq!(
        other_reloads, 0,
        "user automation changes rebuilt another runtime"
    );

    // The disabled alias is absent from both live matchers.
    other_tx
        .send(RuntimeAction::Send(Arc::new("hi".to_string())))
        .unwrap();
    other_tx
        .send(RuntimeAction::Echo(Arc::new("DISABLED_DONE".to_string())))
        .unwrap();
    let (disabled, other_reloads) =
        collect_through_marker(&mut other_events, "DISABLED_DONE").await;
    assert!(!disabled.iter().any(|line| line == "WAVE"));
    assert_eq!(other_reloads, 0);

    // The persisted files reflect the save AND the handle.update().
    let aliases = load_aliases(&server_name).unwrap();
    let greet = aliases
        .get("greet")
        .expect("greet alias persisted to aliases.json");
    assert_eq!(greet.pattern, "^hi$");
    assert_eq!(greet.script.as_deref(), Some("echo('WAVE')"));
    assert_eq!(
        greet.language,
        ScriptLang::JS,
        "language round-trips js -> JS on disk"
    );
    assert!(
        !greet.enabled,
        "handle.update({{enabled:false}}) persisted to disk"
    );

    // The round-tripped alias kept its sidecar and self-match opt-in on disk.
    let cmdlike = aliases
        .get("cmdlike")
        .expect("cmdlike alias persisted to aliases.json");
    assert!(
        cmdlike.allow_self_match,
        "allowSelfMatch round-trips to disk"
    );
    match &cmdlike.matcher {
        Some(smudgy_core::models::matchers::AliasMatcherSource::Command { name, args, .. }) => {
            assert_eq!(name.as_deref(), Some("gt"));
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].name, "words");
        }
        other => panic!("the matcher sidecar was stripped or reshaped: {other:?}"),
    }

    let triggers = load_triggers(&server_name).unwrap();
    let onsay = triggers
        .get("onsay")
        .expect("onsay trigger persisted to triggers.json");
    assert_eq!(
        onsay.patterns.as_deref(),
        Some(&[r"^(\w+) says".to_string()][..])
    );
    assert_eq!(
        onsay.raw_patterns.as_deref(),
        Some(&["TICK".to_string()][..])
    );

    // Fire the delete controller and wait for its complete action frame.
    tx.send(RuntimeAction::Send(Arc::new("delauto".to_string())))
        .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new("DELETE_DONE".to_string())))
        .unwrap();
    let (deleted, own_reloads) = collect_through_marker(&mut events, "DELETE_DONE").await;
    assert!(
        deleted.iter().any(|l| l == "DEL removed=true"),
        "delete controller did not report success: {deleted:?}"
    );
    assert_eq!(own_reloads, 0);

    other_tx
        .send(RuntimeAction::Send(Arc::new("ping".to_string())))
        .unwrap();
    other_tx
        .send(RuntimeAction::Echo(Arc::new(
            "DELETED_PING_DONE".to_string(),
        )))
        .unwrap();
    let (after_delete, other_reloads) =
        collect_through_marker(&mut other_events, "DELETED_PING_DONE").await;
    assert!(
        !after_delete.iter().any(|line| line == "PONG"),
        "deleted alias remained live in the other session: {after_delete:?}"
    );
    assert_eq!(other_reloads, 0);

    tx.send(RuntimeAction::Shutdown).ok();
    other_tx.send(RuntimeAction::Shutdown).ok();

    let aliases = load_aliases(&server_name).unwrap();
    assert!(
        !aliases.contains_key("greet"),
        "greet should be deleted from aliases.json"
    );
    assert!(
        !aliases.contains_key("live"),
        "live should be deleted from aliases.json"
    );
    assert!(
        !aliases.contains_key("cmdlike"),
        "cmdlike should be deleted from aliases.json"
    );
    // The untouched trigger is still there.
    assert!(load_triggers(&server_name).unwrap().contains_key("onsay"));
}
