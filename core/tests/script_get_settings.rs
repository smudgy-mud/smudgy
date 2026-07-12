//! End-to-end: a script reads the app settings via `getSettings()` (`smudgy:core`).
//!
//! Covers the full path — the `op_smudgy_get_settings` op, the synthesized `smudgy:core`
//! named export, and the snapshot delivered by `RuntimeAction::ApplySettings` (including the
//! UI-resolved palette). The harness has no socket, so the script's reading is observed
//! through the echo it appends to the terminal buffer, like `raw_prefix.rs`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::settings::{ScriptPalette, ScriptSettings};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const EVENT_QUIET_PERIOD: Duration = Duration::from_millis(900);

/// A module that registers a JS-function alias which echoes the settings it reads. The alias
/// fires AFTER `ApplySettings`, so `getSettings()` reflects the pushed snapshot (separator,
/// theme, font size, and the resolved palette).
const MODULE_TS: &str = r#"
import { createAlias, echo, getSettings } from "smudgy:core";
createAlias("^checksettings$", () => {
    const s = getSettings();
    const fg = s.palette ? s.palette.foreground : "none";
    echo("SETTINGS sep=" + s.commandSeparator + " theme=" + s.theme + " size=" + s.terminalFontSize + " fg=" + fg);
});
"#;

#[tokio::test]
async fn script_reads_settings_via_get_settings() {
    let server_name = "test_get_settings".to_string();
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().unwrap();
    std::fs::create_dir_all(home.join(&server_name).join("logs")).unwrap();
    let modules_dir = home.join(&server_name).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::write(modules_dir.join("check.ts"), MODULE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9301u32),
        server_name: Arc::new(server_name),
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

    // Push a distinctive settings snapshot (the UI does this on RuntimeReady), including a
    // resolved palette, so the script reads known values rather than the machine's defaults.
    let script_settings = ScriptSettings {
        command_separator: "::".to_string(),
        theme: "TestTheme".to_string(),
        terminal_font_size: 22.0,
        palette: Some(ScriptPalette {
            foreground: "#abcdef".to_string(),
            ..ScriptPalette::default()
        }),
        ..ScriptSettings::default()
    };
    tx.send(RuntimeAction::ApplySettings {
        command_separator: Arc::new("::".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: true,
        script_settings: Box::new(script_settings),
    })
    .unwrap();

    // Fire the alias; "::" as the separator means this single token is never split.
    tx.send(RuntimeAction::Send(Arc::new("checksettings".to_string())))
        .unwrap();

    let mut settings_line = None;
    while let Ok(Some(event)) = tokio::time::timeout(EVENT_QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update
                    && line.text.starts_with("SETTINGS ")
                {
                    settings_line = Some(line.text.clone());
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let line = settings_line.expect("script never echoed the settings it read");
    assert_eq!(
        line,
        "SETTINGS sep=:: theme=TestTheme size=22 fg=#abcdef",
        "getSettings() did not reflect the pushed snapshot"
    );
}
