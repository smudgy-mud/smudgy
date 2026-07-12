//! Regression tests for command execution ordering.
//!
//! The contract under test: command expansion is depth-first and
//! source-agnostic. Whatever a command produces — whether by plaintext alias
//! expansion or by a script calling `send()` — executes immediately after that
//! command, in the order it was produced, before any sibling commands that
//! were already queued behind it.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::{ScriptLang, aliases::AliasDefinition};
use smudgy_core::session::runtime::{IsolateId, Origin, RuntimeAction};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const EVENT_QUIET_PERIOD: Duration = Duration::from_millis(750);

/// One alias to register, with the isolate + origin it belongs to. The isolate is the
/// key dimension under test in the multi-isolate cases: with only `Main` it behaves as a
/// single-isolate session, but two specs sharing `(origin, name)` across *different* isolates
/// must coexist rather than clobber.
struct AliasSpec {
    isolate: IsolateId,
    origin: Origin,
    name: String,
    alias: AliasDefinition,
}

impl AliasSpec {
    /// The common single-isolate case: a user alias in the main isolate.
    fn main(name: &str, alias: AliasDefinition) -> Self {
        Self {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: name.to_string(),
            alias,
        }
    }
}

/// A synthetic sandboxed-package isolate id used purely as a *second key* so the
/// ordering/coexistence invariants can be exercised without a real backing isolate.
/// (Plaintext aliases never touch a v8 registry, so a key with no backing isolate is safe
/// to route.)
fn synthetic_package_isolate() -> IsolateId {
    IsolateId::Package {
        owner: Arc::from("wbk"),
        name: Arc::from("mapper"),
        version: Arc::from("1.4.0"),
    }
}

/// Spins up a headless session, registers each alias under its specified isolate, sends
/// `input`, and returns the outgoing lines (in emission order) restricted to `tokens`.
async fn run_scenario_multi(
    session_id: u32,
    aliases: Vec<AliasSpec>,
    input: &str,
    tokens: &[&str],
) -> Vec<String> {
    // Runtime::run unconditionally opens a session log under
    // <smudgy home>/<server>/logs, so the directory has to exist.
    let server_name = format!("test_ordering_{session_id}");
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().unwrap();
    std::fs::create_dir_all(home.join(&server_name).join("logs")).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
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

    // The runtime self-seeds the command separator from the machine's
    // settings.json; pin it so the test is environment-independent.
    tx.send(RuntimeAction::ApplySettings {
        command_separator: Arc::new(";".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: true,
        script_settings: Box::new(smudgy_core::models::settings::ScriptSettings::default()),
    })
    .unwrap();

    for spec in aliases {
        tx.send(RuntimeAction::AddAlias {
            isolate: spec.isolate,
            origin: spec.origin,
            name: Arc::new(spec.name),
            alias: spec.alias,
            fire_limit: None,
        })
        .unwrap();
    }

    tx.send(RuntimeAction::Send(Arc::new(input.to_string())))
        .unwrap();

    // There is no "done" signal; collect until the session goes quiet.
    let mut seen = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(EVENT_QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update
                    && tokens.contains(&line.text.as_str()) {
                        seen.push(line.text.clone());
                    }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    seen
}

/// Spins up a headless session, registers `alias` (as a user alias in the main isolate),
/// sends `input`, and returns the outgoing lines (in emission order) restricted to `tokens`.
async fn run_scenario(
    session_id: u32,
    alias: AliasDefinition,
    input: &str,
    tokens: &[&str],
) -> Vec<String> {
    run_scenario_multi(
        session_id,
        vec![AliasSpec::main("test_alias", alias)],
        input,
        tokens,
    )
    .await
}

/// A plaintext alias whose body is `script` (split on the command separator at send time).
fn plaintext_alias(pattern: &str, script: &str) -> AliasDefinition {
    AliasDefinition {
        pattern: pattern.to_string(),
        script: Some(script.to_string()),
        package: None,
        enabled: true,
        language: ScriptLang::Plaintext,
    }
}

#[tokio::test]
async fn plaintext_alias_expansion_preserves_command_order() {
    let alias = AliasDefinition {
        pattern: "^pt_alias$".to_string(),
        script: Some("pt_first;pt_second".to_string()),
        package: None,
        enabled: true,
        language: ScriptLang::Plaintext,
    };

    let seen = run_scenario(
        9001,
        alias,
        "pt_alias;pt_third",
        &["pt_first", "pt_second", "pt_third"],
    )
    .await;

    assert_eq!(seen, vec!["pt_first", "pt_second", "pt_third"]);
}

#[tokio::test]
async fn script_alias_sends_preserve_command_order() {
    let alias = AliasDefinition {
        pattern: "^js_alias$".to_string(),
        script: Some(r#"send("js_first"); send("js_second");"#.to_string()),
        package: None,
        enabled: true,
        language: ScriptLang::JS,
    };

    let seen = run_scenario(
        9002,
        alias,
        "js_alias;js_third",
        &["js_first", "js_second", "js_third"],
    )
    .await;

    assert_eq!(seen, vec!["js_first", "js_second", "js_third"]);
}

/// Multi-isolate: two aliases sharing the *same* `(origin, name)` but living in
/// different isolates must **coexist** — both fire — instead of the second clobbering the
/// first via upsert. This is the direct proof that the trigger Manager keys by its
/// isolate dimension (`PACKAGE-ISOLATES.md`): with an `(origin, name)`-only key the
/// second registration would overwrite the first, and only `from_pkg` would appear.
#[tokio::test]
async fn same_origin_name_coexists_across_isolates() {
    let seen = run_scenario_multi(
        9003,
        vec![
            AliasSpec {
                isolate: IsolateId::Main,
                origin: Origin::User,
                name: "dup".to_string(),
                alias: plaintext_alias("^dup$", "from_main"),
            },
            AliasSpec {
                isolate: synthetic_package_isolate(),
                origin: Origin::User,
                name: "dup".to_string(),
                alias: plaintext_alias("^dup$", "from_pkg"),
            },
        ],
        "dup",
        &["from_main", "from_pkg"],
    )
    .await;

    // Both fire, in registration order — the isolate dimension kept them distinct.
    assert_eq!(seen, vec!["from_main", "from_pkg"]);
}

/// Multi-isolate: depth-first command expansion is preserved when expansion
/// crosses the isolate boundary, AND this is a real regression net for the isolate key.
/// The two aliases deliberately share `(origin, name)` ("step") but live in different
/// isolates: a main-isolate alias whose body matches a *package-isolate* alias. With the
/// `(IsolateId, Origin, name)` key they coexist, so "outer" expands "step"→"deep;tail",
/// "deep" expands to two leaves, and depth-first ordering holds across the boundary. With
/// an `(origin, name)`-only key the second registration would clobber the first (both are
/// `(User, "step")`), the main alias would vanish, "outer" would never match, and the
/// assertion would fail — so the isolate dimension in trigger.rs is what this test locks in.
#[tokio::test]
async fn depth_first_order_preserved_across_isolates() {
    let seen = run_scenario_multi(
        9004,
        vec![
            // Main alias: expands to a package-isolate command, then a sibling.
            AliasSpec::main("step", plaintext_alias("^outer$", "deep;tail")),
            // Package-isolate alias — SAME (origin, name) as above, different isolate.
            AliasSpec {
                isolate: synthetic_package_isolate(),
                origin: Origin::User,
                name: "step".to_string(),
                alias: plaintext_alias("^deep$", "deep_a;deep_b"),
            },
        ],
        "outer",
        &["deep_a", "deep_b", "tail", "deep", "outer"],
    )
    .await;

    // "deep" (in the package isolate) fully expands before the "tail" sibling — depth-first
    // across the boundary. "deep"/"outer" themselves are consumed by alias expansion.
    assert_eq!(seen, vec!["deep_a", "deep_b", "tail"]);
}
