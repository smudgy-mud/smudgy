//! End-to-end request/reply, ancestry, permissions, and lifecycle tests.
use std::{
    pin::Pin,
    rc::Rc,
    sync::{Arc, OnceLock},
    time::Duration,
};

use futures::{Stream, StreamExt};
use smudgy_core::{
    models::shared_packages::{self, UpdateMode},
    session::{
        PackageProviderFactory, SessionEvent, SessionId, SessionParams, TaggedSessionEvent,
        runtime::RuntimeAction, spawn, spawn_with_package_provider,
    },
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage, SmudgyCapabilities,
};

fn prepare(server: &str, source: &str) {
    static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
    let home = HOME.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        smudgy_core::set_smudgy_home(&path);
        path
    });
    std::fs::create_dir_all(home.join(server).join("modules")).unwrap();
    std::fs::create_dir_all(home.join(server).join("logs")).unwrap();
    std::fs::write(home.join(server).join("modules/main.ts"), source).unwrap();
}

fn package(
    name: &str,
    source: &str,
    capabilities: SmudgyCapabilities,
) -> (ResolvedPackage, PackagePermissions) {
    (
        ResolvedPackage {
            key: PackageKey {
                owner: "rpc".into(),
                name: name.into(),
            },
            resolved_version: "1.0.0".into(),
            integrity: format!("test-{name}"),
            manifest: PackageManifest::parse(&format!(
                r#"{{"name":"{name}","version":"1.0.0","requires":["smudgy://rpc/producer"]}}"#
            ))
            .unwrap(),
            modules: vec![PackageModuleSource {
                subpath: "index.js".into(),
                text: source.into(),
            }],
        },
        PackagePermissions {
            smudgy: capabilities,
            ..Default::default()
        },
    )
}

fn install(
    server: &str,
    packages: Vec<(ResolvedPackage, PackagePermissions)>,
) -> PackageProviderFactory {
    for (pkg, permissions) in &packages {
        let spec = format!("smudgy://{}/{}", pkg.key.owner, pkg.key.name);
        shared_packages::install_package(server, &spec, UpdateMode::Auto, true).unwrap();
        shared_packages::record_consent(server, &spec, permissions).unwrap();
    }
    Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        for (pkg, _) in &packages {
            provider.insert(pkg.clone());
        }
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    })
}

struct LiveSession {
    events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>>,
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeAction>,
    lines: Vec<String>,
}

impl LiveSession {
    async fn start(
        server: &str,
        id: u32,
        profile: &str,
        provider: Option<PackageProviderFactory>,
    ) -> Self {
        let params = Arc::new(SessionParams {
            session_id: SessionId::from(id),
            server_name: Arc::new(server.into()),
            profile_name: Arc::new(profile.into()),
            profile_subtext: Arc::new(String::new()),
            mapper: None,
            package_client: None,
            extra_script_extensions: Arc::new(Vec::new),
            on_engine_rebuild: None,
        });
        let events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>> = match provider {
            Some(provider) => Box::pin(spawn_with_package_provider(params, provider)),
            None => Box::pin(spawn(params)),
        };
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut session = Self {
            events,
            tx,
            lines: Vec::new(),
        };
        session.ready().await;
        session
    }

    fn collect(&mut self, event: &SessionEvent) {
        if let SessionEvent::UpdateBuffer(updates) = event {
            for update in updates.iter() {
                if let Some(line) = update.main_text() {
                    self.lines.push(line.text.clone());
                }
            }
        }
    }

    async fn ready(&mut self) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let event = self.events.next().await.unwrap();
                self.collect(&event.event);
                if let SessionEvent::RuntimeReady(tx) = event.event {
                    self.tx = tx;
                    break;
                }
            }
        })
        .await
        .unwrap();
    }

    fn command(&self, text: &str) {
        self.tx
            .send(RuntimeAction::Send(Arc::new(text.into())))
            .unwrap();
    }

    async fn until(&mut self, marker: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let event = self.events.next().await.unwrap();
                self.collect(&event.event);
                if self.lines.iter().any(|s| s.contains(marker)) {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("missing {marker}; transcript: {:?}", self.lines));
    }

    fn has(&self, text: &str) -> bool {
        self.lines.iter().any(|s| s.contains(text))
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        let _ = self.tx.send(RuntimeAction::Shutdown);
    }
}

#[tokio::test]
async fn results_errors_and_startup_rejection_are_awaitable() {
    let server = "rpc_results";
    prepare(
        server,
        r#"
import { createAlias, createProcedure, createState, echo, session } from "smudgy:core";
const c = (globalThis as any).__smudgy_interop_consumer("user");
const state = createState<{ n: number }>("state");
export const value = createProcedure((p: any, caller) => {
    state.set({ n: p.n });
    return { n: p.n, origin: caller.origin, profile: caller.session.profile.name };
});
export const string = createProcedure(() => "must-not-send");
export const empty = createProcedure(() => {});
export const nullValue = createProcedure(() => null);
export const thrown = createProcedure(() => { throw new Error("sync failure"); });
export const rejected = createProcedure(async () => {
    await new Promise(r => setTimeout(r, 5)); throw new TypeError("async failure");
});
export const cyclic = createProcedure(() => { const v: any = {}; v.self = v; return v; });
export const callable = createProcedure(() => () => {});
export const huge = createProcedure(() => "x".repeat(1048577));
export const thenable = createProcedure(() => ({ then(resolve: any) { resolve(23); } }));
createAlias("^must-not-send$", () => echo("BAD_SEND"));
try { await c.procedure("empty").call({}); } catch (e: any) { echo("STARTUP:" + e.code); }
createAlias("^test$", async () => {
    const original = { n: 1 };
    const pending = c.procedure("value").call(original);
    original.n = 99;
    const result = await pending;
    echo("VALUE:" + result.n + ":" + result.origin + ":" + result.profile);
    echo("STATE:" + state.value?.n);
    echo("STRING:" + await c.procedure("string").call({}));
    echo("VOID:" + (await c.procedure("empty").call({}) === undefined));
    echo("NULL:" + (await c.procedure("nullValue").call({}) === null));
    echo("THENABLE:" + await c.procedure("thenable").call({}));
    for (const name of ["thrown", "rejected", "cyclic", "callable", "huge", "missing"]) {
        try { await c.procedure(name).call({}); }
        catch (e: any) { echo("ERR:" + name + ":" + e.code + ":" + e.name + ":" + e.message); }
    }
    try { await c.procedure("value").call({ n: 1n }); }
    catch (e: any) { echo("ARGS:" + e.code); }
    try { await c.procedure("value").call({ n: "x".repeat(1048577) }); }
    catch (e: any) { echo("ARGS_SIZE:" + e.code); }
    for (const timeoutMs of [0, -1, 0.5, NaN, Infinity, 60001]) {
        try { await c.procedure("empty").call({}, { timeoutMs }); }
        catch (e: any) { echo("INVALID:" + e.code); }
    }
    const bound = c.procedure("empty").to(session);
    echo("BOUND:" + (await bound.call({}) === undefined) + ":" + ("to" in bound));
    echo("DONE");
});
"#,
    );
    let mut s = LiveSession::start(server, 9801, "Caller", None).await;
    s.command("test");
    s.until("DONE").await;
    for expected in [
        "STARTUP:NotReady",
        "VALUE:1:user:Caller",
        "STATE:1",
        "STRING:must-not-send",
        "VOID:true",
        "NULL:true",
        "THENABLE:23",
        "ERR:thrown:ImplementationError:Error:sync failure",
        "ERR:rejected:ImplementationError:TypeError:async failure",
        "ERR:cyclic:Serialization",
        "ERR:callable:Serialization",
        "ERR:huge:ResponseTooLarge",
        "ERR:missing:Unavailable",
        "ARGS:Serialization",
        "ARGS_SIZE:RequestTooLarge",
        "BOUND:true:false",
    ] {
        assert!(s.has(expected), "missing {expected}; {:?}", s.lines);
    }
    assert_eq!(
        s.lines
            .iter()
            .filter(|s| s.contains("INVALID:InvalidTimeout"))
            .count(),
        6
    );
    assert!(!s.has("BAD_SEND"));
}

#[tokio::test]
async fn timeout_and_capacity_release_slots_and_ignore_late_results() {
    let server = "rpc_limits";
    prepare(
        server,
        r#"
import { createAlias, createProcedure, echo } from "smudgy:core";
const c = (globalThis as any).__smudgy_interop_consumer("user");
export const never = createProcedure(() => new Promise(() => {}));
export const fast = createProcedure((p: any) => p.n);
export const late = createProcedure(async () => {
    await new Promise(r => setTimeout(r, 100)); echo("LATE_RESULT"); return 999;
});
createAlias("^test$", async () => {
    for (let pass = 0; pass < 2; pass++) {
        const results = await Promise.allSettled(Array.from({ length: 300 }, () =>
            c.procedure("never").call({}, { timeoutMs: 150 })));
        echo("LIMIT:" + results.filter((r: any) => r.reason?.code === "Capacity").length
            + ":" + results.filter((r: any) => r.reason?.code === "Timeout").length);
    }
    try { await c.procedure("late").call({}, { timeoutMs: 20 }); }
    catch (e: any) { echo("LATE_TIMEOUT:" + e.code); }
    await new Promise(r => setTimeout(r, 150));
    const replies = await Promise.all(Array.from({ length: 256 }, (_, n) => c.procedure("fast").call({ n })));
    echo("RECOVERED:" + replies.length + ":" + replies[255]);
    echo("DONE");
});
"#,
    );
    let mut s = LiveSession::start(server, 9802, "Caller", None).await;
    s.command("test");
    s.until("DONE").await;
    assert_eq!(
        s.lines
            .iter()
            .filter(|s| s.contains("LIMIT:44:256"))
            .count(),
        2,
        "{:?}",
        s.lines
    );
    assert!(
        s.has("LATE_TIMEOUT:Timeout") && s.has("LATE_RESULT") && s.has("RECOVERED:256:255"),
        "{:?}",
        s.lines
    );
}

#[tokio::test]
async fn call_ancestry_survives_timer_await_and_inherits_deadlines() {
    let server = "rpc_ancestry";
    prepare(
        server,
        r#"
import { createAlias, createProcedure, echo } from "smudgy:core";
const c = (globalThis as any).__smudgy_interop_consumer("user");
export const loop = createProcedure(async (p: any) => {
    await new Promise(r => setTimeout(r, p.delay));
    return c.procedure("loop").call(p, { timeoutMs: 60000 });
});
export const custom = createProcedure((p: any) => ({ then(resolve: any, reject: any) {
    setTimeout(() => c.procedure("custom").call(p, { timeoutMs: 60000 }).then(resolve, reject), 1);
} }));
createAlias("^test$", async () => {
    try { await c.procedure("loop").call({ delay: 1 }, { timeoutMs: 5000 }); }
    catch (e: any) { echo("HOPS:" + e.message); }
    try { await c.procedure("custom").call({}, { timeoutMs: 5000 }); }
    catch (e: any) { echo("CUSTOM_HOPS:" + e.message); }
    try { await c.procedure("loop").call({ delay: 20 }, { timeoutMs: 70 }); }
    catch (e: any) { echo("DEADLINE:" + e.code); }
    echo("DONE");
});
"#,
    );
    let mut s = LiveSession::start(server, 9803, "Caller", None).await;
    s.command("test");
    s.until("DONE").await;
    assert!(
        s.has("HOPS:procedure call hop limit reached")
            && s.has("CUSTOM_HOPS:procedure call hop limit reached")
            && s.has("DEADLINE:Timeout"),
        "{:?}",
        s.lines
    );
}

#[tokio::test]
async fn cross_isolate_calls_preserve_state_and_enforce_both_capabilities() {
    let server = "rpc_isolates";
    prepare(
        server,
        r#"
import { createAlias, echo } from "smudgy:core";
import { request } from "smudgy:procedures/rpc/producer";
import { state } from "smudgy:state/rpc/producer";
createAlias("^test$", async () => {
    const replies = await Promise.all([request.call({ n: 1, delay: 30 }), request.call({ n: 2, delay: 1 })]);
    echo("REPLIES:" + replies.map(r => r.n).join(","));
    echo("STATE:" + state.value?.n); echo("DONE");
});
"#,
    );
    let producer = package(
        "producer",
        r#"
import { createProcedure, createState } from "smudgy:core";
export const state = createState("state");
export const request = createProcedure(async (p, caller) => {
    await new Promise(r => setTimeout(r, p.delay ?? 1));
    state.set({ n: p.n }); return { n: p.n, origin: caller.origin };
});
"#,
        SmudgyCapabilities {
            interop_write: true,
            ..Default::default()
        },
    );
    let denied = |name: &str, read, write| {
        package(
            name,
            &format!(
                r#"
import {{ createAlias, echo }} from "smudgy:core";
import {{ request }} from "smudgy:procedures/rpc/producer";
createAlias("^denied-{name}$", async () => {{
    try {{ await request.call({{}}); }} catch (e) {{ echo("DENIED-{name}:" + e.message); }}
}});
"#
            ),
            SmudgyCapabilities {
                echo: true,
                create_aliases: true,
                interop_read: read,
                interop_write: write,
                ..Default::default()
            },
        )
    };
    let allowed = package(
        "caller",
        r#"
import { createAlias, echo } from "smudgy:core";
import { request } from "smudgy:procedures/rpc/producer";
createAlias("^sandbox$", async () => echo("ORIGIN:" + (await request.call({ n: 3 })).origin));
"#,
        SmudgyCapabilities {
            echo: true,
            create_aliases: true,
            interop_read: true,
            interop_write: true,
            ..Default::default()
        },
    );
    let provider = install(
        server,
        vec![
            producer,
            denied("no-read", false, true),
            denied("no-write", true, false),
            allowed,
        ],
    );
    let mut s = LiveSession::start(server, 9804, "Caller", Some(provider)).await;
    s.command("denied-no-read");
    s.until("DENIED-no-read:").await;
    s.command("denied-no-write");
    s.until("DENIED-no-write:").await;
    s.command("sandbox");
    s.until("ORIGIN:").await;
    s.command("test");
    s.until("DONE").await;
    for expected in [
        "interop:read",
        "interop:write",
        "ORIGIN:smudgy://rpc/caller",
        "REPLIES:1,2",
        "STATE:1",
    ] {
        assert!(s.has(expected), "missing {expected}; {:?}", s.lines);
    }
}

#[tokio::test]
async fn directed_calls_observe_state_and_reject_when_target_reloads() {
    let server = "rpc_sessions";
    prepare(
        server,
        r#"
import { createAlias, createProcedure, createState, echo, getSessions, session } from "smudgy:core";
const c = (globalThis as any).__smudgy_interop_consumer("user");
const state = createState("state");
export const request = createProcedure((p: any, caller) => {
    if (p.wait) { echo("INVOKED"); return new Promise(() => {}); }
    state.set({ n: p.n }); return caller.session.profile.name;
});
if (session.profile.name === "Caller") {
    createAlias("^test$", async () => {
        const target = getSessions().find(s => s.profile.name === "Target")!;
        const result = await c.procedure("request").to(target).call({ n: 42 });
        echo("REMOTE:" + result + ":" + c.state("state").from(target).value.n);
        try { await c.procedure("request").to(target).call({ wait: true }); }
        catch (e: any) { echo("RELOAD:" + e.code); }
        echo("DONE");
    });
}
"#,
    );
    let mut target = LiveSession::start(server, 9805, "Target", None).await;
    let mut caller = LiveSession::start(server, 9806, "Caller", None).await;
    caller.command("test");
    target.until("INVOKED").await;
    target.tx.send(RuntimeAction::Reload).unwrap();
    target.ready().await;
    caller.until("DONE").await;
    assert!(
        caller.has("REMOTE:Caller:42") && caller.has("RELOAD:Reloaded"),
        "{:?}",
        caller.lines
    );
}

#[tokio::test]
async fn caller_reload_discards_old_replies_without_settling_new_calls() {
    let server = "rpc_caller_reload";
    prepare(
        server,
        r#"
import { createAlias, createProcedure, echo, getSessions, session } from "smudgy:core";
const c = (globalThis as any).__smudgy_interop_consumer("user");
const releases = new Map();
export const request = createProcedure((p: any) => {
    echo("INVOKED:" + p.n);
    return new Promise(resolve => releases.set(p.n, () => resolve(p.n)));
});
createAlias("^release (\\d+)$", (m) => {
    const n = Number(m[1]); releases.get(n)?.(); releases.delete(n);
    echo("RELEASED:" + n);
});
if (session.profile.name === "Caller") {
    createAlias("^call (\\d+)$", async (m) => {
        const n = Number(m[1]);
        const target = getSessions().find(s => s.profile.name === "Target")!;
        echo("ANSWER:" + n + ":" + await c.procedure("request").to(target).call({ n }));
    });
}
"#,
    );
    let mut target = LiveSession::start(server, 9807, "Target", None).await;
    let mut caller = LiveSession::start(server, 9808, "Caller", None).await;
    caller.command("call 1");
    target.until("INVOKED:1").await;
    caller.tx.send(RuntimeAction::Reload).unwrap();
    caller.ready().await;
    caller.command("call 2");
    target.until("INVOKED:2").await;
    target.command("release 1");
    target.until("RELEASED:1").await;
    target.command("release 2");
    caller.until("ANSWER:2:").await;
    assert!(caller.has("ANSWER:2:2"), "{:?}", caller.lines);
    assert!(!caller.has("ANSWER:1:"), "{:?}", caller.lines);
}

#[tokio::test]
async fn mixed_post_event_and_call_recursion_cannot_restart_the_hop_count() {
    let server = "rpc_mixed_depth";
    prepare(
        server,
        r#"
import { createAlias, createEvent, createProcedure, echo } from "smudgy:core";
const c = (globalThis as any).__smudgy_interop_consumer("user");
export const bounced = createEvent();
let fires = 0;
const fail = (e: any) => { echo("STOP:" + e.code + ":" + fires); echo("DONE"); };
const bounceImpl = (p: any) => {
    if (++fires >= 90) { echo("UNBOUNDED"); echo("DONE"); return; }
    c.procedure("relay").call(p).catch(fail);
};
export const bounce = createProcedure(bounceImpl);
c.event("bounced").on(bounceImpl);
export const relay = createProcedure((p: any) => {
    if (p.event) bounced.emit(p); else c.procedure("bounce").post(p);
    return 1;
});
createAlias("^test (post|event)$", (m) => {
    fires = 0; c.procedure("bounce").post({ event: m[1] === "event" });
});
"#,
    );
    let mut s = LiveSession::start(server, 9811, "Caller", None).await;
    for mode in ["post", "event"] {
        s.lines.clear();
        s.command(&format!("test {mode}"));
        s.until("DONE").await;
        assert!(s.has("STOP:HopLimit:32"), "{mode}: {:?}", s.lines);
        assert!(!s.has("UNBOUNDED"), "{mode}: {:?}", s.lines);
    }
}

#[tokio::test]
async fn error_getters_are_read_once_and_error_encoding_cannot_strand_calls() {
    let server = "rpc_error_getters";
    prepare(
        server,
        r#"
import { createAlias, createProcedure, echo } from "smudgy:core";
const c = (globalThis as any).__smudgy_interop_consumer("user");
const stringify = JSON.stringify;
let nameReads = 0, messageReads = 0;
const changingError = () => ({
    get name() { return ++nameReads === 1 ? "OddError" : 1n; },
    get message() { return ++messageReads === 1 ? "readable" : 1n; },
});
export const changing = createProcedure(() => { throw changingError(); });
export const asyncChanging = createProcedure(async () => {
    await new Promise(r => setTimeout(r, 1)); throw changingError();
});
export const getterThrows = createProcedure(() => {
    throw { get message() { throw new Error("cannot read"); } };
});
export const encoderThrows = createProcedure(() => {
    JSON.stringify = () => { throw new Error("cannot encode"); };
    throw new Error("readable");
});
export const encoderUndefined = createProcedure(() => {
    JSON.stringify = () => undefined as any; throw new Error("readable");
});
export const resultEncoderThrows = createProcedure(() => {
    JSON.stringify = () => { throw new Error("cannot encode"); }; return 42;
});
createAlias("^test$", async () => {
    for (const name of ["changing", "asyncChanging", "getterThrows", "encoderThrows", "encoderUndefined", "resultEncoderThrows"]) {
        nameReads = messageReads = 0;
        try { await c.procedure(name).call({}, { timeoutMs: 1000 }); }
        catch (e: any) {
            JSON.stringify = stringify;
            echo("ERR:" + name + ":" + e.code + ":" + e.name + ":" + e.message);
            if (name === "changing" || name === "asyncChanging") echo("READS:" + nameReads + ":" + messageReads);
        }
    }
    echo("DONE");
});
"#,
    );
    let mut s = LiveSession::start(server, 9812, "Caller", None).await;
    s.command("test");
    s.until("DONE").await;
    for expected in [
        "ERR:changing:ImplementationError:OddError:readable",
        "ERR:asyncChanging:ImplementationError:OddError:readable",
        "ERR:getterThrows:ImplementationError:ProcedureCallError:Procedure failed with an unreadable error",
        "ERR:encoderThrows:ImplementationError:ProcedureCallError:Procedure failed with an unreadable error",
        "ERR:encoderUndefined:ImplementationError:ProcedureCallError:Procedure failed with an unreadable error",
        "ERR:resultEncoderThrows:Serialization:ProcedureCallError:Procedure failed with an unreadable error",
    ] {
        assert!(s.has(expected), "missing {expected}; {:?}", s.lines);
    }
    assert_eq!(
        s.lines.iter().filter(|s| s.contains("READS:1:1")).count(),
        2
    );
}

#[tokio::test]
async fn producer_reload_rejects_a_reply_prepared_behind_the_reload_action() {
    let server = "rpc_producer_reload";
    prepare(
        server,
        r#"
import { createAlias, createProcedure, echo, getSessions, session } from "smudgy:core";
const c = (globalThis as any).__smudgy_interop_consumer("user");
export const request = createProcedure(() => { session.reload(); return 42; });
if (session.profile.name === "Caller") {
    createAlias("^test$", async () => {
        const target = getSessions().find(s => s.profile.name === "Target")!;
        try { echo("RESULT:" + await c.procedure("request").to(target).call({}, { timeoutMs: 1000 })); }
        catch (e: any) { echo("RESULT:" + e.code); }
        echo("DONE");
    });
}
"#,
    );
    let mut target = LiveSession::start(server, 9813, "Target", None).await;
    let mut caller = LiveSession::start(server, 9814, "Caller", None).await;
    caller.command("test");
    target.ready().await;
    caller.until("DONE").await;
    assert!(caller.has("RESULT:Reloaded"), "{:?}", caller.lines);
}
