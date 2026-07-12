//! Interop consumption of a **locally-authored** producer, driven through the real
//! cloud-backed `SmudgyPackageProvider` (not the in-memory test provider the rest of the
//! store suite uses — that provider has no local-override concept, which is exactly why the
//! regression below slipped past `session_store_isolates.rs`).
//!
//! A local dev-override resolves entirely from disk (`<home>/<server>/packages/<name>/`), so
//! these sessions need no network: the provider is built from a dead-address client that the
//! local path never calls. That lets a genuine `SmudgyPackageProvider` — the only place the
//! bug lives — run headless.
//!
//! Regression: consuming an installed local producer over `smudgy:events/…` must not record a
//! code-load footprint, so the code-import stumble diagnostic must stay quiet. The stub fetch
//! took the local-override branch of `resolve_impl`, which inserted into the served-set cache
//! unconditionally — ignoring the `track == false` contract every other branch honors — and
//! the guard then misfired on a consumer that never imported the producer's code.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_cloud::{Credential, CredentialSource, PackageApiClient};
use smudgy_core::models::local_packages::packages_dir;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);
const MANIFEST_FILE: &str = "smudgy.package.json";

// ---------------------------------------------------------------------------
// Harness — real provider, local packages on disk
// ---------------------------------------------------------------------------

/// First-setter-wins process-global smudgy home; create `<home>/<server>/{modules,logs}`.
/// Each integration file is its own test binary, so the `OnceLock` home is clean here.
fn prepare_server(server: &str) {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let server_dir = home.join(server);
    std::fs::create_dir_all(server_dir.join("modules")).unwrap();
    std::fs::create_dir_all(server_dir.join("logs")).unwrap();
}

/// Author a local package under `<home>/<server>/packages/<name>/` (an `index.ts` plus the
/// manifest), the npm-link-style dev override the provider resolves before the cloud.
fn write_local_package(server: &str, name: &str, manifest_json: &str, index_src: &str) {
    let dir = packages_dir(server).expect("packages dir").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), manifest_json).unwrap();
    std::fs::write(dir.join("index.ts"), index_src).unwrap();
}

/// Write a local `modules/` file for `server` (runs in the MAIN isolate, allow-all).
fn write_main_module(server: &str, name: &str, source: &str) {
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::write(home.join(server).join("modules").join(name), source).unwrap();
}

/// A `SmudgyPackageProvider` behind a dead-address client: local overrides resolve from disk
/// and never touch it, so the session runs fully offline against the real resolver.
fn offline_package_client() -> PackageApiClient {
    PackageApiClient::new(
        "http://127.0.0.1:0",
        CredentialSource::new(Some(Credential::ApiKey("test".into()))),
    )
}

/// Spawn the session against the REAL cloud-backed provider (no in-memory override), collect
/// every appended line (notices included) until quiet.
async fn run_session_real_provider(session_id: u32, server: &str) -> Vec<String> {
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: Some(offline_package_client()),
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines: Vec<String> = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();
    lines
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

fn has_line(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

/// Install a local package untrusted (→ its own sandbox isolate, sandboxed to its manifest).
fn install_untrusted(server: &str, specifier: &str) {
    shared_packages::install_package(server, specifier, UpdateMode::Auto, true).unwrap();
}

/// Install a local package and mark it trusted (→ runs in the main isolate, allow-all).
fn install_trusted(server: &str, specifier: &str) {
    shared_packages::install_package(server, specifier, UpdateMode::Auto, true).unwrap();
    shared_packages::set_trusted(server, specifier, true).unwrap();
}

/// The producer used across most of these tests: a local package publishing an event `prompt`,
/// a state `vitals`, and a procedure `refresh`, granting itself the interop + echo
/// capabilities a local (manifest-sandboxed) package needs to use them.
const PROMPT_MANIFEST: &str = r#"{ "name": "arctic-prompt", "version": "1.0.0",
     "permissions": { "smudgy": { "session": ["echo"], "interop": ["read", "write"] } } }"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A locally-authored producer (`local/arctic-prompt`, installed untrusted → its own sandbox
/// home) is consumed by a locally-authored package (`local/arctic-hud`) that `requires` it and
/// imports `smudgy:events/local/arctic-prompt/prompt`. The consume is a stub fetch, not a code
/// import, so:
/// - the code-import stumble notice must NOT fire (the regression — the local-override stub
///   fetch used to record a served-set footprint, tripping the guard on a consumer that never
///   imported the producer's code); and
/// - the event still delivers cross-isolate, proving the consumption path itself is intact.
#[tokio::test]
async fn local_producer_consumed_over_events_does_not_trip_the_stumble_notice() {
    let server = "lo_events";
    prepare_server(server);

    // Producer: local, installed untrusted → sandboxed to its OWN manifest permissions (a local
    // override's grant is its on-disk manifest, not a consent record), so it grants itself the
    // interop + echo capabilities it uses.
    write_local_package(
        server,
        "arctic-prompt",
        r#"{ "name": "arctic-prompt", "version": "1.0.0",
             "permissions": { "smudgy": { "session": ["echo"], "interop": ["read", "write"] } } }"#,
        r#"
        import { createEvent, echo } from "smudgy:core";
        const prompt = createEvent("prompt");
        echo("PROMPT_RAN");
        setTimeout(() => { prompt.emit({ hp: 43 }); }, 200);
        "#,
    );
    shared_packages::install_package(server, "smudgy://local/arctic-prompt", UpdateMode::Auto, true)
        .unwrap();

    // Consumer: local package, TRUSTED → runs in main (allow-all), so it needs no manifest
    // grants; `requires` authorizes the interop consume through the dependency gate.
    write_local_package(
        server,
        "arctic-hud",
        r#"{ "name": "arctic-hud", "version": "1.0.0",
             "requires": ["smudgy://local/arctic-prompt"] }"#,
        r#"
        import { echo } from "smudgy:core";
        import prompt from "smudgy:events/local/arctic-prompt/prompt";
        prompt.on((p: any) => echo("HUD_EVENT:" + p.hp));
        echo("HUD_RAN");
        "#,
    );
    shared_packages::install_package(server, "smudgy://local/arctic-hud", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::set_trusted(server, "smudgy://local/arctic-hud", true).unwrap();

    let lines = run_session_real_provider(9801, server).await;

    assert!(has_line(&lines, "HUD_RAN"), "the consumer package must load; transcript:\n{lines:#?}");
    assert!(has_line(&lines, "PROMPT_RAN"), "the producer sandbox must load; transcript:\n{lines:#?}");
    // The regression: a `smudgy:events/…` consume of an installed LOCAL producer must not be
    // mistaken for a code import of it.
    assert!(
        !has_line(&lines, "you code-imported smudgy://local/arctic-prompt"),
        "consuming a local producer over the events scheme must NOT trip the code-import stumble \
         notice; transcript:\n{lines:#?}"
    );
    // And the consume actually works cross-isolate.
    assert!(
        has_line(&lines, "HUD_EVENT:43"),
        "the producer's event must deliver through the consumer's scheme handle; transcript:\n{lines:#?}"
    );
}

/// The true-positive counterpart: the fix is surgical, not a blanket silencing. A local
/// consumer that `dependencies` a local installed producer and actually `import`s its code
/// evaluates a duplicate copy in main — its interop home is the producer's own sandbox — so the
/// stumble notice MUST still fire. (Same package graph as the regression, but a code import in
/// place of an events consume.)
#[tokio::test]
async fn local_producer_actually_code_imported_still_stumbles() {
    let server = "lo_codeimport";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        r#"{ "name": "arctic-prompt", "version": "1.0.0",
             "permissions": { "smudgy": { "session": ["echo"] } } }"#,
        r#"
        import { echo } from "smudgy:core";
        echo("PROMPT_RAN");
        "#,
    );
    shared_packages::install_package(server, "smudgy://local/arctic-prompt", UpdateMode::Auto, true)
        .unwrap();

    // Consumer code-IMPORTS the producer (a `dependencies` edge), evaluating a copy of it in
    // main — the very thing the stumble diagnostic exists to catch.
    write_local_package(
        server,
        "arctic-hud",
        r#"{ "name": "arctic-hud", "version": "1.0.0",
             "dependencies": ["smudgy://local/arctic-prompt"] }"#,
        r#"
        import { echo } from "smudgy:core";
        import "smudgy://local/arctic-prompt";
        echo("HUD_RAN");
        "#,
    );
    shared_packages::install_package(server, "smudgy://local/arctic-hud", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::set_trusted(server, "smudgy://local/arctic-hud", true).unwrap();

    let lines = run_session_real_provider(9802, server).await;

    assert!(has_line(&lines, "HUD_RAN"), "the consumer package must load; transcript:\n{lines:#?}");
    assert!(
        has_line(&lines, "you code-imported smudgy://local/arctic-prompt"),
        "a genuine code import of an installed local producer MUST still trip the stumble notice; \
         transcript:\n{lines:#?}"
    );
}

/// The scrub (interop.md §3): a code-imported (non-home) copy of an installed producer has
/// its interop handle exports REMOVED, so importing one fails at LINK — loudly, dressed with
/// the scheme-import notice — instead of yielding a live producer handle whose writes the
/// home gate would refuse anyway.
#[tokio::test]
async fn code_imported_handle_exports_are_scrubbed_to_a_link_failure() {
    let server = "lo_scrub_link";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { createState, echo } from "smudgy:core";
        export const vitals = createState("vitals");
        vitals.set({ hp: 7 });
        echo("PROMPT_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-prompt");

    // The consumer code-imports the HANDLE — post-scrub, that export no longer exists in
    // the non-home copy, so the consumer's whole module set fails at link.
    write_local_package(
        server,
        "arctic-hud",
        r#"{ "name": "arctic-hud", "version": "1.0.0",
             "dependencies": ["smudgy://local/arctic-prompt"] }"#,
        r#"
        import { echo } from "smudgy:core";
        import { vitals } from "smudgy://local/arctic-prompt";
        void vitals;
        echo("HUD_RAN");
        "#,
    );
    install_trusted(server, "smudgy://local/arctic-hud");

    let lines = run_session_real_provider(9804, server).await;

    assert!(
        !has_line(&lines, "HUD_RAN"),
        "the handle import must fail at link, not evaluate; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "does not provide an export named"),
        "the failure is V8's link error, not a mystery; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "interop \u{2014} import them from")
            || has_line(&lines, "handle exports were removed")
            || has_line(&lines, "smudgy:state/local/arctic-prompt"),
        "the scrub notice names the scheme-import fix; transcript:\n{lines:#?}"
    );
}

/// The scrub is surgical: non-handle exports of the same module survive, the module still
/// evaluates (pre-existing code-import side-effect behavior), and the scrub notice replaces
/// the generic stumble text.
#[tokio::test]
async fn code_import_of_non_handle_exports_survives_the_scrub() {
    let server = "lo_scrub_rest";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { createState, echo } from "smudgy:core";
        export const vitals = createState("vitals");
        export const helper = 7;
        echo("PROMPT_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-prompt");

    write_local_package(
        server,
        "arctic-hud",
        r#"{ "name": "arctic-hud", "version": "1.0.0",
             "dependencies": ["smudgy://local/arctic-prompt"] }"#,
        r#"
        import { echo } from "smudgy:core";
        import { helper } from "smudgy://local/arctic-prompt";
        echo("HUD_RAN:" + helper);
        "#,
    );
    install_trusted(server, "smudgy://local/arctic-hud");

    let lines = run_session_real_provider(9805, server).await;

    assert!(
        has_line(&lines, "HUD_RAN:7"),
        "non-handle exports survive the scrub; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "handle exports were removed"),
        "the scrub notice teaches the scheme imports; transcript:\n{lines:#?}"
    );
}

/// On main, a TRUSTED package's home load cannot be scrubbed (one module map, one instance):
/// a user module's code import hands out live producer handles. The accepted interop.md §1
/// residual — warned, so the attribution is never a surprise.
#[tokio::test]
async fn user_code_import_of_trusted_handle_package_warns() {
    let server = "lo_user_import";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { createState, echo } from "smudgy:core";
        export const vitals = createState("vitals");
        vitals.set({ hp: 7 });
        echo("PROMPT_RAN");
        "#,
    );
    install_trusted(server, "smudgy://local/arctic-prompt");

    write_main_module(
        server,
        "userscript.ts",
        r#"
        import { echo } from "smudgy:core";
        import { vitals } from "smudgy://local/arctic-prompt";
        echo("USER_GOT:" + typeof vitals.set);
        "#,
    );

    let lines = run_session_real_provider(9806, server).await;

    assert!(
        has_line(&lines, "USER_GOT:function"),
        "the home load serves the LIVE producer handle to user code; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "publish AS the package"),
        "the attribution warning fires once; transcript:\n{lines:#?}"
    );
}

/// The `track == false` stub contract holds across ALL three kind schemes, not only events: a
/// local producer consumed over `smudgy:state/…` and `smudgy:procedures/…` records no code-load
/// footprint either, so no stumble — and the state snapshot still reads cross-isolate.
#[tokio::test]
async fn local_producer_consumed_over_state_and_procedures_does_not_stumble() {
    let server = "lo_state_msg";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        r#"{ "name": "arctic-prompt", "version": "1.0.0",
             "permissions": { "smudgy": { "session": ["echo"], "interop": ["read", "write"] } } }"#,
        r#"
        import { createState, createProcedure, echo } from "smudgy:core";
        const vitals = createState("vitals");
        export const refresh = createProcedure((args: any) => { void args; });
        vitals.set({ hp: 7 });
        echo("PROMPT_RAN");
        "#,
    );
    shared_packages::install_package(server, "smudgy://local/arctic-prompt", UpdateMode::Auto, true)
        .unwrap();

    write_local_package(
        server,
        "arctic-hud",
        r#"{ "name": "arctic-hud", "version": "1.0.0",
             "requires": ["smudgy://local/arctic-prompt"] }"#,
        r#"
        import { echo } from "smudgy:core";
        import { vitals } from "smudgy:state/local/arctic-prompt";
        import { refresh } from "smudgy:procedures/local/arctic-prompt";
        // Reference the procedure handle so its import can't be elided, without posting.
        void refresh;
        setTimeout(() => { echo("HUD_STATE:" + (vitals as any).value?.hp); }, 300);
        echo("HUD_RAN");
        "#,
    );
    shared_packages::install_package(server, "smudgy://local/arctic-hud", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::set_trusted(server, "smudgy://local/arctic-hud", true).unwrap();

    let lines = run_session_real_provider(9803, server).await;

    assert!(has_line(&lines, "HUD_RAN"), "the consumer package must load; transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, "you code-imported smudgy://local/arctic-prompt"),
        "consuming a local producer over the state/procedures schemes must NOT trip the stumble \
         notice; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "HUD_STATE:7"),
        "the consumer's state snapshot must read the local producer's published subtree \
         cross-isolate; transcript:\n{lines:#?}"
    );
}

/// #1 — the dependency gate, end to end: a package that consumes a producer over
/// `smudgy:events/…` while declaring it in NEITHER `requires` NOR `dependencies` fails to load,
/// naming the fix. Consuming a package IS depending on it (interop.md §9); the loader
/// rejects the undeclared reference before any stub is synthesized.
#[tokio::test]
async fn consuming_an_undeclared_producer_is_refused_at_load() {
    let server = "lo_undeclared";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { createEvent, echo } from "smudgy:core";
        createEvent("prompt");
        echo("PROMPT_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-prompt");

    // Consumer declares NOTHING about arctic-prompt, yet imports its events — untrusted so the
    // failure is scoped to its own sandbox rather than taking down the whole main entry.
    write_local_package(
        server,
        "arctic-hud",
        r#"{ "name": "arctic-hud", "version": "1.0.0",
             "permissions": { "smudgy": { "session": ["echo"] } } }"#,
        r#"
        import { echo } from "smudgy:core";
        import prompt from "smudgy:events/local/arctic-prompt/prompt";
        prompt.on(() => {});
        echo("HUD_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-hud");

    let lines = run_session_real_provider(9804, server).await;

    assert!(
        !has_line(&lines, "HUD_RAN"),
        "the consumer must NOT finish loading — its undeclared consume fails first; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "consumes undeclared smudgy:// package")
            && has_line(&lines, "arctic-prompt"),
        "the load failure must name the undeclared producer and point at `requires`; transcript:\n{lines:#?}"
    );
}

/// #2 — fork served-set independence through the events path. TWO consumers of one local
/// producer — one trusted (main), one untrusted (its own sandbox) — each stub-fetch the
/// producer in their OWN forked provider. Neither may trip the stumble notice: each isolate's
/// served set is its own, and a stub fetch records nothing in either. (Under the bug BOTH
/// consumers stumbled.)
#[tokio::test]
async fn two_consumers_across_isolates_each_stay_stumble_free() {
    let server = "lo_two_consumers";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { createEvent, echo } from "smudgy:core";
        const prompt = createEvent("prompt");
        echo("PROMPT_RAN");
        setTimeout(() => { prompt.emit({ hp: 5 }); }, 200);
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-prompt");

    // Consumer A: trusted → main (allow-all).
    write_local_package(
        server,
        "hud-main",
        r#"{ "name": "hud-main", "version": "1.0.0",
             "requires": ["smudgy://local/arctic-prompt"] }"#,
        r#"
        import { echo } from "smudgy:core";
        import prompt from "smudgy:events/local/arctic-prompt/prompt";
        prompt.on((p: any) => echo("A_EVENT:" + p.hp));
        echo("A_RAN");
        "#,
    );
    install_trusted(server, "smudgy://local/hud-main");

    // Consumer B: untrusted → its own sandbox, sandboxed to its manifest (interop:read + echo).
    write_local_package(
        server,
        "hud-sandbox",
        r#"{ "name": "hud-sandbox", "version": "1.0.0",
             "requires": ["smudgy://local/arctic-prompt"],
             "permissions": { "smudgy": { "session": ["echo"], "interop": ["read"] } } }"#,
        r#"
        import { echo } from "smudgy:core";
        import prompt from "smudgy:events/local/arctic-prompt/prompt";
        prompt.on((p: any) => echo("B_EVENT:" + p.hp));
        echo("B_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/hud-sandbox");

    let lines = run_session_real_provider(9805, server).await;

    assert!(has_line(&lines, "A_RAN") && has_line(&lines, "B_RAN"), "both consumers must load; transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, "you code-imported smudgy://local/arctic-prompt"),
        "neither consumer isolate may trip the stumble — a stub fetch records nothing in either \
         fork's served set; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "A_EVENT:5") && has_line(&lines, "B_EVENT:5"),
        "the producer's event must fan out to BOTH consumer isolates; transcript:\n{lines:#?}"
    );
}

/// #4 — a `requires` root that is never consumed is an install/home edge only, not a load edge:
/// it runs in its own home and never lands in the requirer's served set, so no stumble.
#[tokio::test]
async fn a_required_but_unconsumed_producer_does_not_stumble() {
    let server = "lo_requires_only";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { echo } from "smudgy:core";
        echo("PROMPT_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-prompt");

    // Requires the producer but imports nothing from it.
    write_local_package(
        server,
        "arctic-hud",
        r#"{ "name": "arctic-hud", "version": "1.0.0",
             "requires": ["smudgy://local/arctic-prompt"] }"#,
        r#"
        import { echo } from "smudgy:core";
        echo("HUD_RAN");
        "#,
    );
    install_trusted(server, "smudgy://local/arctic-hud");

    let lines = run_session_real_provider(9806, server).await;

    assert!(has_line(&lines, "HUD_RAN"), "the requirer must load; transcript:\n{lines:#?}");
    assert!(has_line(&lines, "PROMPT_RAN"), "the required producer must run in its own home; transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, "you code-imported smudgy://local/arctic-prompt"),
        "a `requires` root the requirer never imports must not appear in its served set; transcript:\n{lines:#?}"
    );
}

/// #6 — kind-mismatch and unknown-handle errors surface for a LOCAL producer: importing a
/// declared handle from the WRONG scheme names the right one, and an undeclared handle is
/// reported as such. Both are stub-synthesis errors (the producer's source is parsed, never
/// evaluated), so they fail the consumer's load.
#[tokio::test]
async fn kind_mismatch_and_unknown_handle_are_reported_for_a_local_producer() {
    let server = "lo_kind_mismatch";
    prepare_server(server);

    // `vitals` is a STATE handle; there is no event named `vitals`, nor any handle `ghost`.
    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { createState, echo } from "smudgy:core";
        const vitals = createState("vitals");
        void vitals;
        echo("PROMPT_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-prompt");

    // Part A: import the state handle `vitals` through the EVENTS scheme → kind-mismatch hint.
    write_local_package(
        server,
        "hud-wrongkind",
        r#"{ "name": "hud-wrongkind", "version": "1.0.0",
             "requires": ["smudgy://local/arctic-prompt"],
             "permissions": { "smudgy": { "session": ["echo"] } } }"#,
        r#"
        import { echo } from "smudgy:core";
        import v from "smudgy:events/local/arctic-prompt/vitals";
        void v;
        echo("WRONGKIND_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/hud-wrongkind");

    // Part B: import a handle the producer never declares → unknown-handle error.
    write_local_package(
        server,
        "hud-ghost",
        r#"{ "name": "hud-ghost", "version": "1.0.0",
             "requires": ["smudgy://local/arctic-prompt"],
             "permissions": { "smudgy": { "session": ["echo"] } } }"#,
        r#"
        import { echo } from "smudgy:core";
        import g from "smudgy:state/local/arctic-prompt/ghost";
        void g;
        echo("GHOST_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/hud-ghost");

    let lines = run_session_real_provider(9807, server).await;

    assert!(
        !has_line(&lines, "WRONGKIND_RAN")
            && has_line(&lines, "declared as a state handle"),
        "importing a state handle over the events scheme must fail with a kind hint; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "GHOST_RAN")
            && has_line(&lines, "declares no state handle named ghost"),
        "importing an undeclared handle must fail naming it; transcript:\n{lines:#?}"
    );
}

/// #7 — the documented dynamic-import boundary (`emit_stumble_notices`): a `smudgy://` producer
/// imported *after* the module graph settles (here, from a `setTimeout`) is NOT caught by the
/// load-time stumble — the served-set snapshot the guard inspects was already taken. The copy's
/// side effects are instead refused by the home gate at their first write, with that diagnostic.
#[tokio::test]
async fn a_deferred_dynamic_import_is_caught_at_write_not_by_the_load_stumble() {
    let server = "lo_dynamic_import";
    prepare_server(server);

    // Producer writes state at top level, so the code-imported copy's write is refused at once.
    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { createState, echo } from "smudgy:core";
        const vitals = createState("vitals");
        vitals.set({ hp: 99 });
        echo("PROMPT_RAN");
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-prompt");

    // A main local module (allow-all, ungated) dynamically imports the producer AFTER load.
    write_main_module(
        server,
        "dyn.ts",
        r#"
        import { echo } from "smudgy:core";
        setTimeout(async () => {
            await import("smudgy://local/arctic-prompt");
            echo("DYN_IMPORTED");
        }, 200);
        echo("MAIN_RAN");
        "#,
    );

    let lines = run_session_real_provider(9808, server).await;

    assert!(has_line(&lines, "DYN_IMPORTED"), "the deferred dynamic import must complete; transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, "you code-imported smudgy://local/arctic-prompt"),
        "a dynamic import after the load graph settles must NOT trip the load-time stumble; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "[interop] smudgy://local/arctic-prompt: state write ignored"),
        "the copy's non-home write must be refused at write time with the teaching diagnostic; transcript:\n{lines:#?}"
    );
}

/// #3 (feasible form) — mixed producer provenance in ONE consumer isolate. A true cloud
/// producer needs the network, but the served-set contract is per-branch (each proven
/// elsewhere); what remains untested is one isolate consuming across provenance kinds at once.
/// Here a single main-isolate consumer consumes a LOCAL package producer AND a PLATFORM producer
/// (`smudgy:events/sys`, which never touches the provider) side by side — neither trips the
/// stumble, and the local one still delivers.
#[tokio::test]
async fn one_isolate_mixes_a_local_and_a_platform_producer_without_stumbling() {
    let server = "lo_mixed_provenance";
    prepare_server(server);

    write_local_package(
        server,
        "arctic-prompt",
        PROMPT_MANIFEST,
        r#"
        import { createEvent, echo } from "smudgy:core";
        const prompt = createEvent("prompt");
        echo("PROMPT_RAN");
        setTimeout(() => { prompt.emit({ hp: 8 }); }, 200);
        "#,
    );
    install_untrusted(server, "smudgy://local/arctic-prompt");

    // A trusted package consuming a local producer and a platform producer in the same module.
    write_local_package(
        server,
        "arctic-hud",
        r#"{ "name": "arctic-hud", "version": "1.0.0",
             "requires": ["smudgy://local/arctic-prompt"] }"#,
        r#"
        import { echo } from "smudgy:core";
        import prompt from "smudgy:events/local/arctic-prompt/prompt";
        import { connect } from "smudgy:events/sys";
        prompt.on((p: any) => echo("HUD_EVENT:" + p.hp));
        connect.on(() => {});
        echo("HUD_RAN");
        "#,
    );
    install_trusted(server, "smudgy://local/arctic-hud");

    let lines = run_session_real_provider(9809, server).await;

    assert!(has_line(&lines, "HUD_RAN"), "the mixed-provenance consumer must load; transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, "you code-imported"),
        "consuming a local package and a platform producer together must not stumble on either; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "HUD_EVENT:8"),
        "the local producer's event must still deliver alongside the platform consume; transcript:\n{lines:#?}"
    );
}
