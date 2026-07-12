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

const EVENT_QUIET_PERIOD: Duration = Duration::from_millis(900);

/// A module exposing two controller aliases. "makeauto" saves an alias + a trigger via the
/// registry, edits the alias through its handle (`update`), reads it back (`get().def()`), and
/// introspects (`list`/`exists`); "delauto" removes the alias.
const MODULE_TS: &str = r#"
import { createAlias, echo, userAutomations } from "smudgy:core";
createAlias("^makeauto$", () => {
    const a = userAutomations.aliases.save("greet", { pattern: "^hi$", script: "wave", language: "js" });
    userAutomations.triggers.save("onsay", { patterns: ["^(\\w+) says"], rawPatterns: ["TICK"], script: "listen" });
    const langOk = a.def().language === "js";
    const upd = userAutomations.aliases.get("greet").update({ enabled: false });
    const nowDisabled = userAutomations.aliases.get("greet").def().enabled === false;
    const listed = userAutomations.aliases.list().join(",");
    const trig = userAutomations.triggers.exists("onsay");
    echo("MADE lang=" + langOk + " upd=" + upd + " disabled=" + nowDisabled + " list=" + listed + " trig=" + trig);
});
createAlias("^delauto$", () => {
    echo("DEL removed=" + userAutomations.aliases.delete("greet"));
});
"#;

async fn drain_until_quiet(
    events: &mut std::pin::Pin<Box<impl futures::Stream<Item = smudgy_core::session::TaggedSessionEvent>>>,
) -> Vec<String> {
    let mut lines = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(EVENT_QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let smudgy_core::session::BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
    }
    lines
}

#[tokio::test]
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

    let mut events = Box::pin(spawn(params));

    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Fire the create/edit controller (the writes persist synchronously; the calling session is
    // not reloaded, so the controller aliases stay registered for the delete step below).
    tx.send(RuntimeAction::Send(Arc::new("makeauto".to_string()))).unwrap();
    let made = drain_until_quiet(&mut events).await;
    assert!(
        made.iter().any(|l| l == "MADE lang=true upd=true disabled=true list=greet trig=true"),
        "create/edit controller did not report success: {made:?}"
    );

    // The persisted files reflect the save AND the handle.update().
    let aliases = load_aliases(&server_name).unwrap();
    let greet = aliases.get("greet").expect("greet alias persisted to aliases.json");
    assert_eq!(greet.pattern, "^hi$");
    assert_eq!(greet.script.as_deref(), Some("wave"));
    assert_eq!(greet.language, ScriptLang::JS, "language round-trips js -> JS on disk");
    assert!(!greet.enabled, "handle.update({{enabled:false}}) persisted to disk");

    let triggers = load_triggers(&server_name).unwrap();
    let onsay = triggers.get("onsay").expect("onsay trigger persisted to triggers.json");
    assert_eq!(onsay.patterns.as_deref(), Some(&[r"^(\w+) says".to_string()][..]));
    assert_eq!(onsay.raw_patterns.as_deref(), Some(&["TICK".to_string()][..]));

    // Fire the delete controller and settle.
    tx.send(RuntimeAction::Send(Arc::new("delauto".to_string()))).unwrap();
    let deleted = drain_until_quiet(&mut events).await;
    assert!(
        deleted.iter().any(|l| l == "DEL removed=true"),
        "delete controller did not report success: {deleted:?}"
    );

    tx.send(RuntimeAction::Shutdown).ok();

    let aliases = load_aliases(&server_name).unwrap();
    assert!(!aliases.contains_key("greet"), "greet should be deleted from aliases.json");
    // The untouched trigger is still there.
    assert!(load_triggers(&server_name).unwrap().contains_key("onsay"));
}
