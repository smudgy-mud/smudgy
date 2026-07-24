//! The session store across the isolate set (`docs/interop.md` §2–§3): a
//! sandboxed package publishes into its own subtree and main-isolate code consumes it
//! cross-isolate; the `interop:*` capabilities gate the ops (with the legacy `events` manifest
//! tokens aliasing on); and the home-instance gate makes non-home writes inert — a forged
//! creator and a code-imported copy alike no-op with a teaching diagnostic.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams,
    spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage, SmudgyCapabilities,
};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

// ---------------------------------------------------------------------------
// Harness (the `package_isolates_op_capabilities.rs` shape)
// ---------------------------------------------------------------------------

fn make_package(owner: &str, name: &str, version: &str, src: &str) -> ResolvedPackage {
    let manifest_json = format!(r#"{{ "name": "{name}", "version": "{version}" }}"#);
    make_package_full(owner, name, version, &manifest_json, src)
}

/// Like [`make_package`], but with a caller-supplied manifest JSON — for exercising manifest
/// fields the default omits (`importable`, `requires`, `dependencies`, `permissions`).
fn make_package_full(
    owner: &str,
    name: &str,
    version: &str,
    manifest_json: &str,
    src: &str,
) -> ResolvedPackage {
    ResolvedPackage {
        key: PackageKey {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        resolved_version: version.to_string(),
        manifest: PackageManifest::parse(manifest_json).expect("valid manifest"),
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

/// First-setter-wins process-global smudgy home; create `<home>/<server>/{modules,logs}`.
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

/// A consent granting `echo` (the reporting channel) plus whatever `extra` adds.
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

/// Write a local `modules/` file for `server` (runs in the MAIN isolate, allow-all).
fn write_main_module(server: &str, name: &str, source: &str) {
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::write(home.join(server).join("modules").join(name), source).unwrap();
}

/// Spawn the session, collect every appended line (notices included) until quiet.
async fn run_session(
    session_id: u32,
    server: &str,
    factory: PackageProviderFactory,
) -> Vec<String> {
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

    let mut events = Box::pin(spawn_with_package_provider(params, factory));
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A sandboxed producer publishes into its own subtree; main-isolate code watches and reads it
/// cross-isolate. The producer's creator descriptor + isolate are its home, so the write lands;
/// the consumer addresses the subtree by the producer's `smudgy://owner/name`.
#[tokio::test]
async fn sandboxed_package_publishes_and_main_consumes_cross_isolate() {
    let server = "ss_publish";
    prepare_server(server);
    shared_packages::install_package(server, "smudgy://wbk/tracker", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://wbk/tracker",
        &consent_with(|s| s.interop_write = true),
    )
    .unwrap();

    let tracker_src = r#"
        import { echo } from "smudgy:core";
        const store = globalThis.__smudgy_store;
        const creator = { kind: "package", owner: "wbk", name: "tracker", version: "1.0.0" };
        store.set(creator, "prompt", { hp: 42, maxhp: 50 });
        echo("PKG_SET_OK");
    "#;
    // Main watches the package's subtree and reads a leaf after the producer has published.
    write_main_module(
        server,
        "consumer.ts",
        r#"
        import { echo } from "smudgy:core";
        const store = (globalThis as any).__smudgy_store;
        store.watch("smudgy://wbk/tracker", "prompt", (snap: any) => {
            echo("MAIN_SAW:" + JSON.stringify(snap));
        });
        setTimeout(() => {
            echo("MAIN_READ:" + store.get("smudgy://WBK/Tracker", "prompt.hp"));
        }, 300);
        "#,
    );

    let lines = run_session(
        9701,
        server,
        factory_for(vec![make_package("wbk", "tracker", "1.0.0", tracker_src)]),
    )
    .await;

    assert!(has_line(&lines, "PKG_SET_OK"), "the producer's write must succeed; transcript:\n{lines:#?}");
    assert!(
        has_line(&lines, r#"MAIN_SAW:{"hp":42,"maxhp":50}"#),
        "main's watcher must receive the sandboxed producer's flushed state; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "MAIN_READ:42"),
        "main must read the package subtree synchronously (folded producer spec); transcript:\n{lines:#?}"
    );
}

/// The `interop:*` capabilities gate the store ops: an echo-only package is denied `set` (naming
/// `interop:write`) and `watch` (naming `interop:read`); a consent whose capabilities came from
/// the LEGACY `events` manifest tokens grants the aliased interop capabilities end-to-end.
#[tokio::test]
async fn interop_capabilities_gate_store_ops_and_legacy_events_tokens_alias_on() {
    // Part A: echo-only — both store verbs throw, naming the missing capability.
    let server = "ss_caps_denied";
    prepare_server(server);
    shared_packages::install_package(server, "smudgy://wbk/nostore", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::record_consent(server, "smudgy://wbk/nostore", &consent_with(|_| {}))
        .unwrap();

    let denied_src = r#"
        import { echo } from "smudgy:core";
        const store = globalThis.__smudgy_store;
        const creator = { kind: "package", owner: "wbk", name: "nostore", version: "1.0.0" };
        try { store.set(creator, "x", 1); echo("SET_OK"); }
        catch (e) { echo("SET_DENIED:" + (e?.message ?? String(e))); }
        try { store.watch("user", "", () => {}); echo("WATCH_OK"); }
        catch (e) { echo("WATCH_DENIED:" + (e?.message ?? String(e))); }
        try { store.getTagged("user", "x"); echo("TAGGED_OK"); }
        catch (e) { echo("TAGGED_DENIED:" + (e?.message ?? String(e))); }
        try { store.keys("user", ""); echo("KEYS_OK"); }
        catch (e) { echo("KEYS_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    let lines = run_session(
        9702,
        server,
        factory_for(vec![make_package("wbk", "nostore", "1.0.0", denied_src)]),
    )
    .await;
    assert!(has_line(&lines, "DONE"), "the package must finish; transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, "SET_OK")
            && has_line(&lines, "SET_DENIED:")
            && has_line(&lines, "interop:write"),
        "an ungranted set must throw NotCapable('interop:write'); transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "WATCH_OK")
            && has_line(&lines, "WATCH_DENIED:")
            && has_line(&lines, "interop:read"),
        "an ungranted watch must throw NotCapable('interop:read'); transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "TAGGED_OK")
            && lines
                .iter()
                .any(|l| l.contains("TAGGED_DENIED:") && l.contains("interop:read")),
        "an ungranted tagged get must throw NotCapable('interop:read'); transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "KEYS_OK")
            && lines
                .iter()
                .any(|l| l.contains("KEYS_DENIED:") && l.contains("interop:read")),
        "an ungranted keys read must throw NotCapable('interop:read'); transcript:\n{lines:#?}"
    );

    // Part B: a consent recorded from the legacy wire form (`events: ["emit","subscribe"]`)
    // grants the aliased interop capabilities, so the same verbs work.
    let server = "ss_caps_alias";
    prepare_server(server);
    shared_packages::install_package(server, "smudgy://wbk/legacy", UpdateMode::Auto, true)
        .unwrap();
    let legacy_caps: SmudgyCapabilities = serde_json::from_value(serde_json::json!({
        "session": ["echo"],
        "events": ["emit", "subscribe"],
    }))
    .expect("legacy wire form parses");
    shared_packages::record_consent(
        server,
        "smudgy://wbk/legacy",
        &PackagePermissions {
            smudgy: legacy_caps,
            ..Default::default()
        },
    )
    .unwrap();

    let legacy_src = r#"
        import { echo } from "smudgy:core";
        const store = globalThis.__smudgy_store;
        const creator = { kind: "package", owner: "wbk", name: "legacy", version: "1.0.0" };
        try {
            store.set(creator, "x", 7);
            echo("LEGACY_SET:" + store.get("smudgy://wbk/legacy", "x"));
        } catch (e) { echo("LEGACY_ERR:" + (e?.message ?? String(e))); }
    "#;
    let lines = run_session(
        9703,
        server,
        factory_for(vec![make_package("wbk", "legacy", "1.0.0", legacy_src)]),
    )
    .await;
    assert!(
        has_line(&lines, "LEGACY_SET:7"),
        "legacy events tokens must alias onto interop read+write; transcript:\n{lines:#?}"
    );
}

/// The home-instance gate (`docs/interop.md` §3). With `wbk/tracker` installed
/// untrusted (home = its own sandbox):
/// - main-isolate code forging the tracker's creator descriptor writes NOTHING (origin alone is
///   insufficient — the write is a no-op with a teaching diagnostic, not a throw);
/// - a code-imported copy of the tracker evaluated in main (a local module imports it) also
///   cannot write, and the load emits the code-import stumble notice;
/// - the home instance's own write survives all of it.
#[tokio::test]
async fn non_home_writes_are_inert_with_teaching_diagnostics() {
    let server = "ss_home_gate";
    prepare_server(server);
    shared_packages::install_package(server, "smudgy://wbk/tracker", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://wbk/tracker",
        &consent_with(|s| s.interop_write = true),
    )
    .unwrap();

    let tracker_src = r#"
        import { echo } from "smudgy:core";
        const store = globalThis.__smudgy_store;
        const creator = { kind: "package", owner: "wbk", name: "tracker", version: "1.0.0" };
        store.set(creator, "prompt", { hp: 42 });
        echo("TRACKER_RAN");
    "#;
    // A local module (main isolate): forges the tracker's creator, then code-imports the
    // tracker itself (evaluating a COPY of it in main — whose own write must also be refused).
    write_main_module(
        server,
        "forger.ts",
        r#"
        import { echo } from "smudgy:core";
        import "smudgy://wbk/tracker";
        const store = (globalThis as any).__smudgy_store;
        const forged = { kind: "package", owner: "wbk", name: "tracker", version: "1.0.0" };
        store.set(forged, "prompt", { hp: 1 });
        echo("FORGE_ATTEMPTED:" + JSON.stringify(store.get("smudgy://wbk/tracker", "prompt")));
        setTimeout(() => {
            echo("SETTLED:" + JSON.stringify(store.get("smudgy://wbk/tracker", "prompt.hp")));
        }, 300);
        "#,
    );

    let lines = run_session(
        9704,
        server,
        factory_for(vec![make_package("wbk", "tracker", "1.0.0", tracker_src)]),
    )
    .await;

    // The copy evaluated in main ran (its echo works — main is allow-all)...
    assert!(has_line(&lines, "TRACKER_RAN"), "transcript:\n{lines:#?}");
    // ...but neither the forged write nor the copy's own write landed: main still reads the
    // subtree as absent right after forging (the forge turn's journal holds nothing).
    assert!(
        has_line(&lines, "FORGE_ATTEMPTED:undefined"),
        "a forged-creator write from main must be a no-op; transcript:\n{lines:#?}"
    );
    // The teaching diagnostics: the refused write and the load-time code-import stumble.
    assert!(
        has_line(&lines, "[interop] smudgy://wbk/tracker: state write ignored"),
        "the refused write must explain itself once; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "you code-imported smudgy://wbk/tracker"),
        "the code-import stumble notice must appear at load; transcript:\n{lines:#?}"
    );
    // The home instance's write is the one that survives.
    assert!(
        has_line(&lines, "SETTLED:42"),
        "the sandbox (home) instance's write must win; transcript:\n{lines:#?}"
    );
}

/// The main-isolate consumer for [`consumer_schemes_read_watch_and_subscribe_cross_isolate`]
/// (a top-level const so the test body stays within the line budget, like
/// `session_store.rs`'s modules).
const SCHEMES_CONSUMER_TS: &str = r#"
    import { echo } from "smudgy:core";
    import { promptState } from "smudgy:state/wbk/prompt";
    import promptViaSubpath from "smudgy:state/wbk/prompt/promptState";
    import { prompt } from "smudgy:events/wbk/prompt";
    import { ghostState } from "smudgy:state/wbk/ghost";

    promptState.watch((snap: any) => {
        echo("WATCHED:" + JSON.stringify(snap));
        if (snap && snap.hp === 43) {
            // The delivery proves the producer's second commit flushed, so the previous
            // generation is deterministic here: the state before the hp=43 batch,
            // materialized whole through the cross-isolate previous view.
            echo("PREV:" + JSON.stringify(promptState.previousValue));
        }
    });
    prompt.on((p: any) => echo("EVENT:" + p.hp));

    echo("SEAT:" + (("set" in (promptState as any)) ? "leaky" : "clean")
        + "/" + (("emit" in (prompt as any)) ? "leaky" : "clean"));

    // Local modules evaluate before the sandboxed producer publishes, so the reads (and
    // the read-only mutation probe, which wants a real generation) run after the
    // producer's delayed write.
    setTimeout(() => {
        const snap: any = JSON.parse(JSON.stringify((promptState as any).value ?? null));
        echo("READ:" + snap?.hp);
        echo("SUBPATH_SAME:" + ((promptViaSubpath as any).value?.maxhp));
        echo("GHOST_READ:" + String((ghostState as any).previousValue));
        try {
            (promptState as any).previousValue.hp = 0;
            echo("FROZEN:no");
        } catch {
            echo("FROZEN:yes");
        }
        // The read-only live view: a leaf read crosses one entry, not the tree, and
        // works cross-isolate; mutation through it throws; an uninstalled producer's
        // .value is undefined outright (the root hop, not a truthy keyed view).
        echo("LEAF:" + (promptState as any).value.hp);
        try {
            (promptState as any).value.hp = 0;
            echo("VALMUT:no-throw");
        } catch {
            echo("VALMUT:threw");
        }
        echo("GHOST_VALUE:" + String((ghostState as any).value));
    }, 500);
"#;

/// The consumer schemes end to end (`docs/interop.md` §4): a sandboxed
/// producer declares `state`/`event` handles (statically extractable — literal name
/// arguments); a main-isolate consumer imports its consumer handles from
/// `smudgy:state/wbk/prompt` (named + single-handle subpath forms) and
/// `smudgy:events/wbk/prompt`, and:
/// - materializes a synchronous snapshot through the read-only view, reads a `.value`
///   leaf, and receives a watch delivery for a later write;
/// - reads the cross-isolate `previousValue` (the generation before the producer's newest
///   batch) deterministically from inside the watch delivery that proves the batch flushed;
/// - receives the producer's event through the scheme handle;
/// - holds a seat with only its verbs (no `set` on a state consumer, no `emit` on an event
///   consumer) whose views are read-only on both surfaces (mutation throws);
/// - a *fetchable but uninstalled* producer links and reads as absent — consuming never
///   evaluates it (its top level never runs).
#[tokio::test]
async fn consumer_schemes_read_watch_and_subscribe_cross_isolate() {
    let server = "ss_schemes";
    prepare_server(server);
    shared_packages::install_package(server, "smudgy://wbk/prompt", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://wbk/prompt",
        &consent_with(|s| s.interop_write = true),
    )
    .unwrap();

    // The producer: an initial publish at load, then a later write + emit so a consumer that
    // subscribed after the load turn still sees a delivery.
    let prompt_src = r#"
        import { createState, createEvent, echo } from "smudgy:core";
        const promptState = createState("promptState");
        const prompt = createEvent("prompt");
        promptState.set({ hp: 42, maxhp: 50 });
        setTimeout(() => {
            promptState.set("hp", 43);
            prompt.emit({ hp: 43 });
        }, 200);
        echo("PRODUCER_RAN");
    "#;
    // A fetchable-but-uninstalled producer: resolvable through the provider (so the stub can
    // extract its handle names) but absent from the lock — it must never evaluate.
    let ghost_src = r#"
        import { createState, echo } from "smudgy:core";
        const ghostState = createState("ghostState");
        ghostState.set({ boo: 1 });
        echo("GHOST_RAN");
    "#;

    write_main_module(server, "consumer.ts", SCHEMES_CONSUMER_TS);

    let lines = run_session(
        9705,
        server,
        factory_for(vec![
            make_package("wbk", "prompt", "1.0.0", prompt_src),
            make_package("wbk", "ghost", "1.0.0", ghost_src),
        ]),
    )
    .await;

    assert!(has_line(&lines, "PRODUCER_RAN"), "transcript:\n{lines:#?}");
    // 42 or 43 depending on whether the producer's delayed write beat the consumer's timer —
    // either proves the cross-isolate sync snapshot; the watch assertions below pin the rest.
    assert!(
        has_line(&lines, "READ:42") || has_line(&lines, "READ:43"),
        "the consumer's sync snapshot must see the producer's published state; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "you code-imported smudgy://wbk/prompt"),
        "consuming through the schemes must NOT trip the code-import stumble notice; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "SUBPATH_SAME:50"),
        "the single-handle subpath default import must address the same handle; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, r#"WATCHED:{"hp":43,"maxhp":50}"#),
        "the watch must deliver the later write's flushed state; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "EVENT:43"),
        "the event must deliver through the smudgy:events consumer handle; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "SEAT:clean/clean"),
        "consumer handles must not carry producer verbs; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "FROZEN:yes"),
        "mutation through the consumer's previous view must throw; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, r#"PREV:{"hp":42,"maxhp":50}"#),
        "previousValue must read the generation before the producer's flushed batch, cross-isolate; transcript:\n{lines:#?}"
    );
    // 42 or 43 for the same reason as READ above: either proves the cross-isolate leaf read.
    assert!(
        has_line(&lines, "LEAF:42") || has_line(&lines, "LEAF:43"),
        "the consumer's .value leaf read must see the sandboxed producer's state; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "VALMUT:threw"),
        "mutation through the consumer's read-only .value must throw; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "GHOST_VALUE:undefined"),
        "an uninstalled producer's .value must be undefined (the root hop, not a truthy view); transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "GHOST_RAN") && has_line(&lines, "GHOST_READ:undefined"),
        "consuming an uninstalled producer must link and read absent without evaluating it; transcript:\n{lines:#?}"
    );
}

/// The sandboxed producer for
/// [`consumer_previous_anchor_ignores_the_producers_open_journal`]: two committed
/// generations driven by dispatched watch deliveries (turn separation must be dispatch, not
/// timers — idle timers coalesce into one pump and would fold the batches), then a third
/// batch opened FROM A TIMER armed in the stage-2 delivery. The holder's stage-2 block —
/// the LAST delivery of the gen2 flush (watchers fire in registration order; the holder
/// evaluates last) — expires this write timer and the consumer's probe timer together, so
/// the park that follows fires both wakers and both callbacks share ONE pump ahead of one
/// flush: the very coalescing the turn-separation trap warns about, used deliberately. The
/// consumer's probe runs in that pump while this batch's journal is open.
const GEN_PRODUCER_JS: &str = r#"
    import { createState, echo } from "smudgy:core";
    const store = globalThis.__smudgy_store;
    const genState = createState("genState");
    genState.set({ hp: 41 });
    let stage = 0;
    store.watch("smudgy://wbk/red", "genState", () => {
        stage++;
        if (stage === 1) {
            genState.set("hp", 42);
        } else if (stage === 2) {
            // Armed at this delivery's post-dispatch pump (polled before it expires),
            // fired at the park after the holder's block.
            setTimeout(() => {
                genState.set("hp", 43);
                echo("JOURNAL_OPEN");
            }, 40);
        }
    });
    echo("PRODUCER_RAN");
"#;

/// The holder for the same test: a bystander isolate whose stage-2 watch delivery blocks
/// past every other actor's timer deadline, expiring the producer's write timer and the
/// consumer's probe timers together so the following park coalesces them into one pump. It
/// must be the flush's LAST delivery (it evaluates — and so registers — last); an
/// earlier-slot block would let the later deliveries' dispatch pumps run the overdue timers
/// one isolate at a time instead.
const GEN_HOLDER_JS: &str = r#"
    import { echo } from "smudgy:core";
    const store = globalThis.__smudgy_store;
    let n = 0;
    store.watch("smudgy://wbk/red", "genState", () => {
        n++;
        if (n === 2) {
            const until = Date.now() + 300;
            while (Date.now() < until) { /* expire the write timer and the probe timers */ }
        }
    });
    echo("HOLDER_RAN");
"#;

/// The sandboxed consumer for the same test: arms a one-shot `previousValue` probe timer at
/// EVERY watch delivery (each delivery's dispatch pump polls this isolate, registering the
/// fresh timer with the driver) and echoes every probe, so the transcript's interleaving
/// with `JOURNAL_OPEN` places probes inside the open-journal pump. The probe deadline
/// (60ms) is deliberately LATER than the producer's write timer (40ms). Within the
/// coalesced pump the engine's ready-set drain runs the producer isolate ahead of this one
/// on this key set (asserted below — not a public contract, but stable), so the probes that
/// coalesce with the write read mid-journal.
const GEN_CONSUMER_JS: &str = r#"
    import { echo } from "smudgy:core";
    const store = globalThis.__smudgy_store;
    const tc = globalThis.__smudgy_interop_consumer("smudgy://wbk/red").state("genState");
    let probes = 0;
    store.watch("smudgy://wbk/red", "genState", () => {
        probes++;
        const n = probes;
        setTimeout(() => {
            const v = tc.value;
            const p = tc.previousValue;
            echo("PROBE" + n + ":" + (v === undefined ? "absent" : v.hp)
                + "|" + (p === undefined ? "absent" : JSON.stringify(p)));
            if (v !== undefined && v.hp === 43) echo("PROBES_DONE");
        }, 60);
    });
    echo("CONSUMER_RAN");
"#;

/// `previousValue`'s anchor is per reader (`docs/interop.md` §2): a producer
/// opening a write batch moves its OWN diff base to the committed head, but the open journal
/// is invisible to every other isolate, so a cross-isolate consumer's `previousValue` must
/// stay the retained generation until the batch commits. The producer commits hp=41 then
/// hp=42 (retaining 41), then opens a third batch (hp=43) inside the same engine pump as the
/// consumer's probe (both timers coalesced at one park by the holder's block); the
/// consumer's mid-journal probe must read `value` 42 (the flushed head) with `previousValue`
/// `{hp:41}` (the retained generation) — never `{hp:42}`, the committed-head anchor that
/// would mean the consumer's "previous" silently followed the producer's batch lifecycle.
#[tokio::test]
async fn consumer_previous_anchor_ignores_the_producers_open_journal() {
    let server = "ss_prev_anchor";
    prepare_server(server);
    shared_packages::install_package(server, "smudgy://wbk/red", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://wbk/red",
        &consent_with(|s| {
            s.interop_write = true;
            s.interop_read = true;
        }),
    )
    .unwrap();
    shared_packages::install_package(server, "smudgy://wbk/blue", UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://wbk/blue",
        &consent_with(|s| s.interop_read = true),
    )
    .unwrap();
    shared_packages::install_package(server, "smudgy://wbk/holder", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://wbk/holder",
        &consent_with(|s| s.interop_read = true),
    )
    .unwrap();

    let lines = run_session(
        9707,
        server,
        factory_for(vec![
            make_package("wbk", "red", "1.0.0", GEN_PRODUCER_JS),
            make_package("wbk", "blue", "1.0.0", GEN_CONSUMER_JS),
            make_package("wbk", "holder", "1.0.0", GEN_HOLDER_JS),
        ]),
    )
    .await;

    assert!(has_line(&lines, "PRODUCER_RAN"), "transcript:\n{lines:#?}");
    assert!(has_line(&lines, "CONSUMER_RAN"), "transcript:\n{lines:#?}");
    assert!(has_line(&lines, "HOLDER_RAN"), "transcript:\n{lines:#?}");
    assert!(
        has_line(&lines, "JOURNAL_OPEN"),
        "the producer must open its third batch; transcript:\n{lines:#?}"
    );
    assert!(has_line(&lines, "PROBES_DONE"), "the consumer must see the third commit; transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, r#":42|{"hp":42}"#),
        "the consumer's previousValue must never anchor to the committed head just because the producer holds an open journal; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, r#":43|{"hp":42}"#),
        "the committing flush advances every reader's anchor to the displaced generation; transcript:\n{lines:#?}"
    );
    // The load-bearing window: echoes preserve callback order, so the probes between
    // JOURNAL_OPEN (the write landing in the journal) and the first 43-probe (the flush
    // having landed) ran with the producer's journal open. There must BE such a probe (the
    // holder expired both timers before the park, and the write timer's earlier deadline
    // runs it first in the coalesced pump), and every one must still read the RETAINED
    // generation.
    let open = lines
        .iter()
        .position(|l| l.contains("JOURNAL_OPEN"))
        .expect("asserted above");
    let committed = lines
        .iter()
        .position(|l| l.contains(":43|"))
        .expect("asserted above");
    let mid_journal: Vec<&String> = lines[open..committed]
        .iter()
        .filter(|l| l.contains("PROBE"))
        .collect();
    assert!(
        !mid_journal.is_empty(),
        "no consumer probe ran inside the open-journal pump — the coalesced park must run \
         the producer's (earlier-deadline) write timer and then the consumer's probes in \
         one pump ahead of the flush; transcript:\n{lines:#?}"
    );
    assert!(
        mid_journal.iter().all(|l| l.contains(r#":42|{"hp":41}"#)),
        "a consumer probe inside the producer's open-journal pump must read the flushed \
         head (42) with the RETAINED generation (41) as previousValue; transcript:\n{lines:#?}"
    );
}

/// Across the isolate set: main consumes a sandboxed producer's state per-write
/// (`onWrite`, handle-relative paths, value-identical writes preserved) and posts to its
/// procedure through `smudgy:procedures/…`. The post happens at main's load — before the
/// producer's isolate has evaluated — so it exercises the queue-briefly buffer: the
/// producer's implementation registration (at construction) drains it, with the
/// host-stamped `user` sender.
#[tokio::test]
async fn per_write_watch_and_procedures_cross_isolates() {
    let server = "ss_phase4";
    prepare_server(server);
    shared_packages::install_package(server, "smudgy://wbk/tracker", UpdateMode::Auto, true)
        .unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://wbk/tracker",
        &consent_with(|s| s.interop_write = true),
    )
    .unwrap();

    let tracker_src = r#"
        import { createState, createProcedure, echo } from "smudgy:core";
        const vitals = createState("vitals");
        export const refresh = createProcedure((payload, sender) => {
            echo("PKG_GOT:" + payload.n + ":" + sender);
            // Answer by publishing state: two value-identical writes in one turn, which the
            // consumer's per-write watch must see as two occurrences.
            vitals.set("hp", 1);
            vitals.set("hp", 1);
        });
        echo("PRODUCER_READY");
    "#;

    write_main_module(
        server,
        "consumer.ts",
        r#"
        import { echo } from "smudgy:core";
        import { vitals } from "smudgy:state/wbk/tracker";
        import { refresh } from "smudgy:procedures/wbk/tracker";

        let n = 0;
        vitals.onWrite((path: string, snap: unknown) => {
            n++;
            echo("OW" + n + ":" + path + "=" + JSON.stringify(snap));
        });
        echo("SEAT:" + (("post" in (refresh as any)) ? "has-post" : "missing")
            + "/" + (("on" in (refresh as any)) ? "leaky" : "clean"));
        // Posted before the producer's isolate evaluates: buffered, drained when its
        // implementation registers at construction.
        refresh.post({ n: 7 });
        "#,
    );

    let lines = run_session(
        9706,
        server,
        factory_for(vec![make_package("wbk", "tracker", "1.0.0", tracker_src)]),
    )
    .await;

    assert!(has_line(&lines, "PRODUCER_READY"), "transcript:\n{lines:#?}");
    assert!(
        has_line(&lines, "PKG_GOT:7:user"),
        "the early post must drain to the producer at registration with the host-stamped sender; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "SEAT:has-post/clean"),
        "the procedure consumer seat carries post only; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "OW1:hp=1") && has_line(&lines, "OW2:hp=1"),
        "per-write watch must deliver both value-identical writes with handle-relative paths; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "OW3:"),
        "exactly two per-write deliveries; transcript:\n{lines:#?}"
    );
}

/// `importable: false` gates code IMPORTS, not interop consumption. A pure-library producer
/// (`wbk/lib`, `importable:false`) publishing an event is still consumable cross-package over
/// `smudgy:events/…` by a different-owner package (`cor/app`) that `requires` it — the
/// events-only-library switch (`REQUIRED-PACKAGES.md`). The consume must deliver, name no
/// import-deny error, and trip no code-import stumble (it never imports the producer's code).
/// The complementary denial — a cross-owner *code* import of an `importable:false` package — is
/// covered at the loader in `module_loader.rs`.
#[tokio::test]
async fn importable_false_blocks_code_import_but_not_interop_consumption() {
    let server = "ss_importable";
    prepare_server(server);

    // Producer: a non-importable library, installed untrusted (own sandbox home), consented to
    // emit.
    shared_packages::install_package(server, "smudgy://wbk/lib", UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://wbk/lib",
        &consent_with(|s| s.interop_write = true),
    )
    .unwrap();
    let lib = make_package_full(
        "wbk",
        "lib",
        "1.0.0",
        r#"{ "name": "lib", "version": "1.0.0", "importable": false }"#,
        r#"
        import { createEvent, echo } from "smudgy:core";
        const tick = createEvent("tick");
        echo("LIB_RAN");
        setTimeout(() => { tick.emit({ n: 1 }); }, 200);
        "#,
    );

    // Consumer: a different-owner package that `requires` the library and consumes its event.
    shared_packages::install_package(server, "smudgy://cor/app", UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(
        server,
        "smudgy://cor/app",
        &consent_with(|s| s.interop_read = true),
    )
    .unwrap();
    let app = make_package_full(
        "cor",
        "app",
        "1.0.0",
        r#"{ "name": "app", "version": "1.0.0", "requires": ["smudgy://wbk/lib"] }"#,
        r#"
        import { echo } from "smudgy:core";
        import tick from "smudgy:events/wbk/lib/tick";
        tick.on((p) => echo("APP_TICK:" + p.n));
        echo("APP_RAN");
        "#,
    );

    let lines = run_session(9706, server, factory_for(vec![lib, app])).await;

    assert!(has_line(&lines, "LIB_RAN") && has_line(&lines, "APP_RAN"), "both must load; transcript:\n{lines:#?}");
    assert!(
        has_line(&lines, "APP_TICK:1"),
        "an importable:false library's event must still deliver to a cross-owner consumer; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "not importable"),
        "interop consumption must not trip the import-deny gate; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "you code-imported smudgy://wbk/lib"),
        "consuming events must not be mistaken for a code import; transcript:\n{lines:#?}"
    );
}
