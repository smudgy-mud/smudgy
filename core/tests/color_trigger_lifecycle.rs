//! Lifecycle and sandbox-capability coverage for script-authored color triggers.
//!
//! The matching semantics have focused coverage elsewhere. These tests hold the
//! integration seams that are easy to miss: a sandboxed package still needs the
//! ordinary `automations: ["triggers"]` consent for a styled descriptor, and a
//! reloaded module's newly registered prompt trigger can consume the first
//! partial line without a completed-line priming event.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::settings::{ScriptSettings, Settings, TerminalBoldMode};
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::connection::vt_processor::{AnsiColor, Color, VtProcessor};
use smudgy_core::session::runtime::{RuntimeAction, RuntimeThreadJoinOutcome, join_runtime_thread};
use smudgy_core::session::styled_line::{Style, StyledLine, TextAttributes, VtSpan};
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams, spawn,
    spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage, SmudgyCapabilities,
};
use vtparse::VTParser;

static NEXT_SESSION_ID: AtomicU32 = AtomicU32::new(17_300);

const PACKAGE_SOURCE: &str = r#"
import { createTrigger, echo, style } from "smudgy:core";

try {
    createTrigger(style.red(/^PKG_COLOR$/), () => echo("PKG_COLOR_FIRED"), {
        name: "package-color-trigger",
    });
    echo("PKG_COLOR_REGISTERED");
} catch (error) {
    echo("PKG_COLOR_DENIED:" + (error?.message ?? String(error)));
}
"#;

const LIFECYCLE_SOURCE: &str = r#"
import { createTrigger, echo, style } from "smudgy:core";

createTrigger(style.red(/^LIFECYCLE>$/), () => echo("LIFECYCLE_PROMPT_FIRED"), {
    name: "lifecycle-color-prompt",
    prompt: true,
});
echo("LIFECYCLE_READY");
"#;

const EMPTY_PROMPT_SOURCE: &str = r#"
import { createTrigger, echo, style } from "smudgy:core";

let promptCount = 0;
createTrigger(style.red, () => {
    promptCount += 1;
    if (promptCount === 1) {
        echo("EMPTY_RED_SGR_ONLY_PROMPT_FIRED");
    } else if (promptCount === 2) {
        echo("EMPTY_RED_INHERITED_PROMPT_FIRED");
    } else {
        echo("EMPTY_RED_EXTRA_PROMPT_FIRED");
    }
}, {
    name: "empty-red-prompt",
    prompt: true,
});
echo("EMPTY_RED_PROMPT_READY");
"#;

fn next_session_id() -> SessionId {
    SessionId::from(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed))
}

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

fn params(session_id: SessionId, server: &str) -> Arc<SessionParams> {
    Arc::new(SessionParams {
        session_id,
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })
}

fn factory_for(package: ResolvedPackage) -> PackageProviderFactory {
    Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        provider.insert(package.clone());
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    })
}

fn package(name: &str) -> ResolvedPackage {
    let manifest = format!(
        r#"{{
            "name": "{name}",
            "version": "1.0.0",
            "permissions": {{
                "smudgy": {{
                    "automations": ["triggers"],
                    "session": ["echo"]
                }}
            }}
        }}"#
    );
    ResolvedPackage {
        key: PackageKey {
            owner: "wbk".to_string(),
            name: name.to_string(),
        },
        resolved_version: "1.0.0".to_string(),
        manifest: PackageManifest::parse(&manifest).expect("valid package manifest"),
        integrity: format!("test-{name}-1.0.0"),
        modules: vec![PackageModuleSource {
            subpath: "index.js".to_string(),
            text: PACKAGE_SOURCE.to_string(),
        }],
    }
}

fn package_consent(create_triggers: bool) -> PackagePermissions {
    PackagePermissions {
        smudgy: SmudgyCapabilities {
            echo: true,
            create_triggers,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn bright_red_line(text: &str) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(
        text,
        vec![VtSpan {
            style: Style {
                fg: Color::Ansi {
                    color: AnsiColor::Red,
                    bold: true,
                },
                ..Style::DEFAULT
            },
            begin_pos: 0,
            end_pos: text.len(),
        }],
    ))
}

fn normal_red_bold_line(text: &str) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(
        text,
        vec![VtSpan {
            style: Style {
                fg: Color::Ansi {
                    color: AnsiColor::Red,
                    bold: false,
                },
                attributes: TextAttributes {
                    bold: true,
                    ..TextAttributes::DEFAULT
                },
                ..Style::DEFAULT
            },
            begin_pos: 0,
            end_pos: text.len(),
        }],
    ))
}

fn apply_bold_is_bright(enabled: bool) -> RuntimeAction {
    let settings = Settings {
        terminal_bold_mode: if enabled {
            TerminalBoldMode::BoldAndBright
        } else {
            TerminalBoldMode::Bold
        },
        ..Settings::default()
    };
    RuntimeAction::ApplySettings {
        command_separator: Arc::new(settings.command_separator.clone()),
        raw_line_prefix: Arc::new(settings.raw_line_prefix.clone()),
        log_enabled: false,
        bold_is_bright: enabled,
        reconnect_on_send_error: false,
        script_settings: Box::new(ScriptSettings::from(&settings)),
    }
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

async fn run_package_case(server: &str, name: &str, create_triggers: bool) -> Vec<String> {
    let session_id = next_session_id();
    prepare_server(server);
    let spec = format!("smudgy://wbk/{name}");
    let package = package(name);
    shared_packages::install_package(server, &spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(server, &spec, &package_consent(create_triggers)).unwrap();

    let mut events = Box::pin(spawn_with_package_provider(
        params(session_id, server),
        factory_for(package),
    ));
    let mut lines = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for package RuntimeReady")
            .expect("package event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };

    tx.send(RuntimeAction::HandleIncomingLine(bright_red_line(
        "PKG_COLOR",
    )))
    .unwrap();
    // This external echo sits behind the incoming line in the runtime queue.
    // Trigger descendants run depth-first before the next external action, so
    // observing it proves both a possible fire and its callback have drained.
    tx.send(RuntimeAction::Echo(Arc::new(
        "PKG_COLOR_DRAINED".to_string(),
    )))
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    while !lines.iter().any(|line| line == "PKG_COLOR_DRAINED") {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the package-line drain barrier")
            .expect("package event stream ended before the line drain barrier");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }

    tx.send(RuntimeAction::Shutdown).unwrap();
    drop(tx);
    drop(events);
    let joined = tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
        .await
        .expect("runtime join task does not panic");
    assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });
    lines
}

#[tokio::test]
async fn sandboxed_packages_need_trigger_consent_for_styled_descriptors() {
    let denied = run_package_case("ColorTriggerPackageDenied", "color_trigger_denied", false).await;
    let denied_transcript = denied.join("\n");
    assert!(
        denied
            .iter()
            .any(|line| line.starts_with("PKG_COLOR_DENIED:") && line.contains("triggers")),
        "the denial must name the missing triggers capability\n{denied_transcript}"
    );
    assert!(
        !denied.iter().any(|line| line == "PKG_COLOR_REGISTERED")
            && !denied.iter().any(|line| line == "PKG_COLOR_FIRED"),
        "a denied package must neither register nor fire\n{denied_transcript}"
    );

    let allowed =
        run_package_case("ColorTriggerPackageAllowed", "color_trigger_allowed", true).await;
    let allowed_transcript = allowed.join("\n");
    assert!(
        allowed.iter().any(|line| line == "PKG_COLOR_REGISTERED"),
        "the consented package must register its styled trigger\n{allowed_transcript}"
    );
    assert_eq!(
        allowed
            .iter()
            .filter(|line| line.as_str() == "PKG_COLOR_FIRED")
            .count(),
        1,
        "the consented package's styled trigger must fire once\n{allowed_transcript}"
    );
    assert!(
        !allowed
            .iter()
            .any(|line| line.starts_with("PKG_COLOR_DENIED:")),
        "the consented descriptor must not be rejected\n{allowed_transcript}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn apply_settings_and_reload_preserve_first_styled_prompt_matching() {
    let session_id = next_session_id();
    let server = "ColorTriggerSettingsReload";
    let server_dir = prepare_server(server);
    std::fs::write(
        server_dir
            .join("modules")
            .join("color_trigger_lifecycle.ts"),
        LIFECYCLE_SOURCE,
    )
    .unwrap();

    let mut events = Box::pin(spawn(params(session_id, server)));
    let mut lines = Vec::new();
    let mut tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for initial RuntimeReady")
            .expect("event stream ended before initial RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };

    // Normal red plus the terminal bold attribute is not the bright red ANSI
    // slot while bold-as-bright is disabled.
    tx.send(apply_bold_is_bright(false)).unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(
        normal_red_bold_line("LIFECYCLE>"),
    ))
    .unwrap();
    // Like the package-case barrier, this proves the partial line and all of
    // its trigger descendants finished before we take the negative snapshot.
    tx.send(RuntimeAction::Echo(Arc::new(
        "BOLD_DISABLED_DRAINED".to_string(),
    )))
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    while !lines.iter().any(|line| line == "BOLD_DISABLED_DRAINED") {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the bold-disabled drain barrier")
            .expect("event stream ended before the bold-disabled drain barrier");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    let fires_with_bright_disabled = lines
        .iter()
        .filter(|line| line.as_str() == "LIFECYCLE_PROMPT_FIRED")
        .count();
    tx.send(RuntimeAction::RetractIncomingPartialLine).unwrap();

    // ApplySettings changes the very next qualification without rebuilding
    // the registered predicate.
    tx.send(apply_bold_is_bright(true)).unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(
        normal_red_bold_line("LIFECYCLE>"),
    ))
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    while lines
        .iter()
        .filter(|line| line.as_str() == "LIFECYCLE_PROMPT_FIRED")
        .count()
        < 1
    {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the post-settings prompt")
            .expect("event stream ended before the post-settings prompt");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    tx.send(RuntimeAction::RetractIncomingPartialLine).unwrap();

    // Reload creates a fresh engine and reruns the module. RuntimeReady is the
    // only synchronization: no complete incoming line primes the new prompt
    // PatternSet before this first partial line.
    tx.send(RuntimeAction::Reload).unwrap();
    tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for post-reload RuntimeReady")
            .expect("event stream ended before post-reload RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    tx.send(RuntimeAction::HandleIncomingPartialLine(
        normal_red_bold_line("LIFECYCLE>"),
    ))
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    while lines
        .iter()
        .filter(|line| line.as_str() == "LIFECYCLE_PROMPT_FIRED")
        .count()
        < 2
    {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the first post-reload prompt")
            .expect("event stream ended before the first post-reload prompt");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }

    tx.send(RuntimeAction::Shutdown).unwrap();
    drop(tx);
    drop(events);
    let joined = tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
        .await
        .expect("runtime join task does not panic");
    assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });

    let transcript = lines.join("\n");
    assert_eq!(
        fires_with_bright_disabled, 0,
        "normal red + bold must not qualify while bold-as-bright is off\n{transcript}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "LIFECYCLE_READY")
            .count(),
        2,
        "the module must register once initially and once after reload\n{transcript}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "LIFECYCLE_PROMPT_FIRED")
            .count(),
        2,
        "the settings-qualified and first post-reload prompts must each fire once\n{transcript}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn sgr_only_and_bare_prompt_boundaries_reach_a_color_only_prompt_trigger() {
    let session_id = next_session_id();
    let server = "ColorTriggerEmptyPromptBoundary";
    let server_dir = prepare_server(server);
    std::fs::write(
        server_dir.join("modules").join("empty_prompt.ts"),
        EMPTY_PROMPT_SOURCE,
    )
    .unwrap();

    let mut events = Box::pin(spawn(params(session_id, server)));
    let mut lines = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for empty-prompt RuntimeReady")
            .expect("event stream ended before empty-prompt RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    while !lines.iter().any(|line| line == "EMPTY_RED_PROMPT_READY") {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the empty-prompt trigger registration")
            .expect("event stream ended before the empty-prompt trigger registered");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }

    // Drive the real VT actor into the runtime queue. The first explicit
    // boundary carries only an SGR transition; the second carries no bytes at
    // all and must inherit the cursor's still-red style.
    let mut parser = VTParser::new();
    let mut processor = VtProcessor::new(tx.clone());
    // `style.red` is the bright ANSI palette identity (slot 9), so use SGR
    // 91 rather than normal/dim red (slot 1).
    for &byte in b"\x1b[91m" {
        parser.parse_byte(byte, &mut processor);
    }
    processor.commit_prompt();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    while !lines
        .iter()
        .any(|line| line == "EMPTY_RED_SGR_ONLY_PROMPT_FIRED")
    {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the SGR-only empty styled prompt")
            .expect("event stream ended before the SGR-only empty styled prompt fired");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    assert!(
        !lines
            .iter()
            .any(|line| line == "EMPTY_RED_INHERITED_PROMPT_FIRED"),
        "the inherited-style marker must not appear before its bare boundary\n{}",
        lines.join("\n")
    );

    processor.commit_prompt();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    while !lines
        .iter()
        .any(|line| line == "EMPTY_RED_INHERITED_PROMPT_FIRED")
    {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the inherited-style bare prompt")
            .expect("event stream ended before the inherited-style bare prompt fired");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }

    tx.send(RuntimeAction::Shutdown).unwrap();
    drop(processor);
    drop(tx);
    drop(events);
    let joined = tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
        .await
        .expect("runtime join task does not panic");
    assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });

    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "EMPTY_RED_SGR_ONLY_PROMPT_FIRED")
            .count(),
        1,
        "the SGR-only prompt must fire exactly once\n{}",
        lines.join("\n")
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "EMPTY_RED_INHERITED_PROMPT_FIRED")
            .count(),
        1,
        "the inherited-style bare prompt must fire exactly once\n{}",
        lines.join("\n")
    );
    assert!(
        !lines
            .iter()
            .any(|line| line == "EMPTY_RED_EXTRA_PROMPT_FIRED"),
        "no extra prompt boundary may fire the trigger\n{}",
        lines.join("\n")
    );
}
