//! End-to-end: the session store (`docs/interop.md` §2) through a real session
//! runtime — the `__smudgy_store` host hook backed by the store ops, the turn-batched write
//! journal (read-your-writes + flush-before-dispatch), and turn-coalesced watch delivery.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// Module exercising the store from the main isolate:
/// - set-at-path + synchronous read-your-writes within the writing turn (`RW`), with
///   case-insensitive path lookups;
/// - absent-vs-stored-null distinction (`ABSENT` / `NIL`);
/// - turn-coalesced watch: three same-turn writes produce ONE delivery carrying the final
///   state, delivered asynchronously (after `SYNC-AFTER-WRITES` echoes);
/// - flush-before-dispatch: a later turn writes then emits, and the event handler reads the
///   flushed value (`EMITREAD`).
const STORE_TS: &str = r#"
import { echo, createEvent, events } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;

store.set(null, "Char.Vitals", { hp: 10, maxhp: 20 });
echo("RW:" + store.get("user", "char.vitals.HP"));
echo("ABSENT:" + String(store.get("user", "nope")));
store.set(null, "nil", null);
echo("NIL:" + String(store.get("user", "nil")));

let deliveries = 0;
store.watch("user", "Char.Vitals", (snap: any) => {
    deliveries++;
    echo("WATCH" + deliveries + ":" + JSON.stringify(snap));
});
store.set(null, "Char.Vitals.hp", 11);
store.set(null, "Char.Vitals.hp", 12);
echo("SYNC-AFTER-WRITES");

const check = createEvent("check");
events.lookup("user", "check").on(() => {
    echo("EMITREAD:" + store.get("user", "Char.Vitals.hp"));
});
setTimeout(() => {
    store.set(null, "Char.Vitals.hp", 99);
    check.emit({});
}, 10);
"#;

/// Module exercising unwatch + first-published key casing:
/// - a watch cancelled by its token receives no further deliveries;
/// - enumeration shows first-published casing while later differently-cased writes land on the
///   same folded keys.
const UNWATCH_TS: &str = r#"
import { echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;

const sub = store.watch("user", "x", (s: any) => echo("W:" + s));
store.set(null, "x", 1);
store.set(null, "Grp.Foo", 1);
setTimeout(() => {
    sub.unwatch();
    store.set(null, "x", 2);
    store.set(null, "GRP.Bar", 2);
    setTimeout(() => {
        echo("FINAL:" + JSON.stringify(store.get("user", "grp")));
        echo("X:" + store.get("user", "x"));
    }, 50);
}, 100);
"#;

/// Module exercising widget bindings (interop.md §7) from the main isolate:
/// - `bind()` mints a plain frozen token carrying a numeric id;
/// - re-binding a case-respelled path reuses the same id (dedup per folded path);
/// - each flushed turn that writes a comparable path emits ONE `StoreBindingsChanged`
///   repaint wake on the session event stream;
/// - a turn writing only unbound paths emits none.
const BIND_TS: &str = r#"
import { createState, echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;
const vitals = createState<{ hp: number; maxhp: number }>('vitals');

const b: any = vitals.bind('hp');
echo("TOKEN:" + typeof b.__smudgyStoreBinding + ":" + Object.isFrozen(b));
const respelled: any = vitals.bind('HP');
echo("DEDUP:" + (b.__smudgyStoreBinding === respelled.__smudgyStoreBinding));
const other: any = vitals.bind('maxhp');
echo("DISTINCT:" + (b.__smudgyStoreBinding !== other.__smudgyStoreBinding));

vitals.set({ hp: 10, maxhp: 20 });      // turn 1: bound subtree written -> wake 1
setTimeout(() => {
    vitals.set("hp", 11);                // turn 2: bound path written -> wake 2
    setTimeout(() => {
        store.set(null, "elsewhere", 1); // turn 3: no binding under this path -> no wake
        setTimeout(() => echo("DONE"), 50);
    }, 50);
}, 50);
"#;

/// Module exercising the per-write cadence (interop.md §2) through the `__smudgy_store` hook:
/// one delivery per set-at-path in write order, value-identical writes included, carrying the
/// written (producer-relative) path; non-comparable writes are silent; deliveries are
/// asynchronous (after `SYNC`).
const ONWRITE_TS: &str = r#"
import { echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;

let n = 0;
store.onWrite("user", "chat", (path: string, snap: unknown) => {
    n++;
    echo("OW" + n + ":" + path + "=" + JSON.stringify(snap));
});
store.set(null, "chat.last", "hi");
store.set(null, "chat.last", "hi");
store.set(null, "elsewhere", 1);
echo("SYNC");
"#;

/// Module exercising the producer mutation proxy (interop.md §4a): assignments publish set-at-path
/// at exactly the assigned path (verified through a per-write watch), reads are live
/// (read-your-writes), delete rewrites the parent, and assigning `.value` replaces the root.
const VALUE_PROXY_TS: &str = r#"
import { createState, echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;
const paths: string[] = [];
store.onWrite("user", "vitals", (path: string) => { paths.push(path); });

const vitals = createState<any>('vitals');
vitals.set({ hp: 10, stats: { str: 5 } });
vitals.value.hp = 11;
echo("VAL:" + vitals.value.hp);
vitals.value.stats.str = 6;
echo("NEST:" + (vitals.value as any).stats.str);
delete vitals.value.stats;
echo("DEL:" + String((vitals.value as any).stats));
echo("KEYS:" + JSON.stringify(Object.keys(vitals.value)));
vitals.value = { fresh: 1 };
echo("WHOLE:" + JSON.stringify(vitals.value));
setTimeout(() => echo("PATHS:" + JSON.stringify(paths)), 50);
"#;

/// Module exercising the leaf-aware read path (`docs/interop-pre-gmcp-plan.md` §2) through
/// `.value` on both seats. The whole module is ONE turn against an EMPTY committed tree, so
/// every read below resolves through the journal overlay (read-your-writes) — a
/// journal-blind tagged get / keys / has fails all of it. Covers absent-vs-null through the
/// proxy, frozen array payloads, deeper-proxy hops, lazy accessor descriptors (with
/// spread/`JSON.stringify` still materializing), the shape protocols (`defineProperty`,
/// `freeze`) throwing teaching errors on both seats, same-turn kind changes, root
/// replacement replacing the key set, the read-only consumer view (mutation throws; reads
/// share the overlay), and the consumer root hop over scalar/array/absent roots.
const PROXY_READ_TS: &str = r#"
import { createState, echo } from "smudgy:core";

const t = createState<any>('t');
t.set({ nil: null, arr: [1, 2], obj: { k: 1 }, s: "x" });
const v: any = t.value;
echo("NIL:" + String(v.nil));
echo("ABSENT:" + String(v.nope));
echo("HAS:" + ("nil" in v) + ":" + ("nope" in v));
echo("ARR:" + JSON.stringify(v.arr) + ":" + Object.isFrozen(v.arr));
echo("DEEP:" + v.obj.k);
echo("KEYS:" + JSON.stringify(Object.keys(v)));
// Descriptors are lazy accessors: attribute-only protocols never materialize the value;
// invoking the getter resolves the same hop the get trap does (a deeper proxy here).
const desc: any = Object.getOwnPropertyDescriptor(v, "obj");
echo("DESC:" + typeof desc.get + ":" + String(desc.value) + ":" + desc.enumerable + ":" + desc.configurable + ":" + desc.get().k);
echo("DESCABSENT:" + String(Object.getOwnPropertyDescriptor(v, "nope")));
// Spread and JSON.stringify materialize through [[Get]] as before.
echo("SPREAD:" + JSON.stringify({ ...v }));
echo("JSON:" + JSON.stringify(v));
// Shape protocols on the producer view: teaching TypeErrors, not silent no-ops.
try { Object.defineProperty(v, "dp", { value: 1, enumerable: true, configurable: true, writable: true }); echo("PDEF:no-throw"); }
catch (e) { echo("PDEF:" + (e instanceof TypeError) + ":" + String(e).includes("assignment or set()")); }
try { Object.freeze(v); echo("PFREEZE:no-throw"); }
catch (e) { echo("PFREEZE:" + (e instanceof TypeError) + ":" + String(e).includes("copy the data")); }
// A same-turn write below a scalar changes its kind: the proxy hop sees an object.
t.set("s.sub", 1);
echo("KIND:" + typeof v.s + ":" + v.s.sub);
// Root replacement within the turn replaces the key set -- no stale keys leak.
t.value = { fresh: 1 };
echo("REPLACED:" + JSON.stringify(Object.keys(v)) + ":" + String(v.nil));

// The consumer seat over the same subtree (the same factory the smudgy:state stubs call):
// a read-only live view -- reads share the overlay, mutation throws.
const consumers = (globalThis as any).__smudgy_interop_consumer("user");
const tc = consumers.state("t");
echo("CVAL:" + tc.value.fresh);
try { tc.value.fresh = 2; echo("CSET:no-throw"); }
catch (e) { echo("CSET:" + (e instanceof TypeError) + ":" + String(e).includes("read-only")); }
try { delete tc.value.fresh; echo("CDEL:no-throw"); }
catch { echo("CDEL:threw"); }
try { Object.defineProperty(tc.value, "x", { value: 1 }); echo("CDEF:no-throw"); }
catch (e) { echo("CDEF:" + (e instanceof TypeError) + ":" + String(e).includes("read-only")); }
try { Object.freeze(tc.value); echo("CFREEZE:no-throw"); }
catch (e) { echo("CFREEZE:" + (e instanceof TypeError) + ":" + String(e).includes("read-only")); }
echo("CKEYS:" + JSON.stringify(Object.keys(tc.value)));

// The consumer root hop is honest about non-object roots: a scalar root reads as the
// scalar, an array root as the frozen array, an absent producer as undefined.
const sc = createState<any>('sc');
sc.set(7);
const ar = createState<any>('ar');
ar.set([1, 2]);
echo("CSCALAR:" + typeof consumers.state("sc").value + ":" + consumers.state("sc").value);
const av: any = consumers.state("ar").value;
echo("CARRAY:" + Array.isArray(av) + ":" + JSON.stringify(av) + ":" + Object.isFrozen(av));
echo("CGHOST:" + String(consumers.state("ghost").value));
"#;

/// Module exercising `previousValue` (`docs/interop-pre-gmcp-plan.md` §5) on both seats: the
/// state before the newest write batch — the open journal's base while the writer is
/// mid-turn, else the generation the last committing flush retained; absent before the first
/// commit; read-only everywhere. Turn separation is driven through dispatched event
/// deliveries, never timers (idle timers coalesce into one pump in this harness, which would
/// fold the write batches together and leave nothing "previous" to observe).
const PREVIOUS_TS: &str = r#"
import { createState, createEvent, events, echo } from "smudgy:core";

const t = createState<any>('t');
const consumers = (globalThis as any).__smudgy_interop_consumer("user");
const tc = consumers.state("t");

// Before any write: no batch, no commit -- absent on both seats.
echo("P0:" + String(t.previousValue) + ":" + String(tc.previousValue));
t.set({ hp: 1, stats: { str: 5 } });
// Open journal, no prior commit: the first batch's base is absence.
echo("P1:" + String(t.previousValue) + ":" + t.value.hp);

const step2 = createEvent("step2");
const step3 = createEvent("step3");
events.lookup("user", "step2").on(() => {
    // A fresh turn after the first commit: the state before the first batch is absence.
    echo("P2:" + String(t.previousValue) + ":" + String(tc.previousValue));
    t.set("hp", 2);
    // Mid-batch: previousValue is this batch's base (the committed first generation)
    // while value already reads the journal (read-your-writes).
    echo("P3:" + JSON.stringify(t.previousValue) + ":" + t.value.hp);
    echo("P3KEYS:" + JSON.stringify(Object.keys(t.previousValue)));
    step3.emit({});
});
events.lookup("user", "step3").on(() => {
    // Between batches: the retained generation (before hp=2), on both seats, leaf-priced.
    echo("P4:" + t.previousValue.hp + ":" + tc.previousValue.hp + ":" + t.value.hp);
    echo("P4NEST:" + tc.previousValue.stats.str);
    // Read-only on BOTH seats, each with its own teaching: the producer already holds the
    // publishing seat, so its message names the snapshot base (write through .value/set()),
    // never the consumer-seat line; the consumer keeps the seat message.
    try { (t.previousValue as any).hp = 9; echo("PREVRO:no-throw"); }
    catch (e) { echo("PREVRO:" + (e instanceof TypeError) + ":" + String(e).includes("snapshot base") + ":" + String(e).includes("producer's seat")); }
    try { (tc.previousValue as any).hp = 9; echo("CPREVRO:no-throw"); }
    catch (e) { echo("CPREVRO:" + (e instanceof TypeError) + ":" + String(e).includes("producer's seat")); }
    // A new batch re-anchors previous to ITS base (hp=2), superseding the retained hp=1.
    t.set("hp", 3);
    echo("P5:" + t.previousValue.hp + ":" + t.value.hp);
    // A non-object generation reads whole via the root hop, like .value.
    sc.set(8);
    echo("PSCALAR:" + typeof sc.previousValue + ":" + sc.previousValue);
});

// A scalar-rooted handle committed in the module turn, rewritten in step3.
const sc = createState<any>('sc');
sc.set(7);
step2.emit({});
"#;

/// Module exercising `createDerived()` (interop.md §4b): computed over a watched source, published into
/// the deriving script's own subtree (hence readable/bindable like any state), recomputed
/// per source turn, and stopped by `off()`. Turn separation is driven through dispatched
/// deliveries (an event subscriber, then the derived-output watch), NOT nested timers —
/// idle timers coalesce into one pump, which would fold the writes into a single flush.
const DERIVED_TS: &str = r#"
import { createState, createEvent, events, createDerived, echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;

const vitals = createState<{ hp: number; maxhp: number }>('vitals');
vitals.set({ hp: 10, maxhp: 20 });
// A consumer view of our own subtree (user handles have no scheme; plan 12b).
const source = {
    get value() { return store.get("user", "vitals"); },
    watch(fn: (snap: unknown) => void) { const s = store.watch("user", "vitals", fn); return { off: () => s.unwatch() }; },
};
const pct = createDerived('hpPct', source as any, (v: any) => v.hp / v.maxhp);
echo("INIT:" + pct.value);
const token: any = pct.bind();
echo("BINDABLE:" + typeof token.__smudgyStoreBinding);

store.watch("user", "hpPct", (snap: any) => {
    echo("RECOMPUTED:" + snap);
    if (snap === 0.75) {
        // Runs in its own dispatched turn with no source delivery in flight: stopping here
        // means the write below must NOT recompute.
        pct.off();
        vitals.set({ hp: 1, maxhp: 20 });
        setTimeout(() => echo("AFTER-OFF:" + pct.value), 100);
    }
});
// A separate turn (the subscriber dispatch) re-publishes the source -> recompute to 0.75.
const tick = createEvent("tick");
events.lookup("user", "tick").on(() => vitals.set({ hp: 15, maxhp: 20 }));
tick.emit({});
"#;

/// Module exercising procedures on the `user` producer (interop.md §6):
/// host-stamped senders, async next-pump delivery (after `SYNC`), and the queue-briefly
/// buffer (a post before any receiver registers is drained FIFO at registration).
const PROCEDURES_TS: &str = r#"
import { createProcedure, echo } from "smudgy:core";
const consumers = (globalThis as any).__smudgy_interop_consumer("user");

let delivered = false;
export const req = createProcedure((payload: { n: number }, sender) => {
    delivered = true;
    echo("MSG:" + payload.n + ":" + sender);
});
consumers.procedure("req").post({ n: 1 });
echo("SYNC:" + delivered);

// Post BEFORE the implementation exists: buffered (the producer -- user -- is addressable),
// drained when the implementation registers a turn later (dynamic creation, explicit name).
consumers.procedure("early").post({ n: 7 });
setTimeout(() => {
    createProcedure('early', (p: any) => echo("EARLY:" + p.n));
}, 10);
"#;

/// Module proving transpile-time name inference end to end (interop.md §4): a top-level
/// `export const vitals = createState()` names itself after the binding — the published
/// subtree is `vitals` — while dynamic (nested-scope) creation without a name throws the
/// teaching `TypeError` instead of silently minting an unnamed handle.
const INFERRED_NAMES_TS: &str = r#"
import { createState, echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;

export const vitals = createState<{ hp: number }>();
vitals.set({ hp: 42 });
echo("NAMED:" + JSON.stringify(store.get("user", "vitals")));

try {
    (function () { return createState(); })();
    echo("DYNAMIC:no-throw");
} catch (e) {
    echo("DYNAMIC:" + (e instanceof TypeError) + ":" + String(e).includes("could not infer"));
}
"#;

/// Module exercising the M6 verb surface (interop.md §2/§11): path-scoped `watch`/`onWrite`
/// (woken only for writes at/above/below the scoped path, onWrite paths still
/// handle-relative), argless `once()` resolving a promise, and deep-frozen event payloads.
const VERBS_TS: &str = r#"
import { createState, createEvent, echo } from "smudgy:core";
const consumers = (globalThis as any).__smudgy_interop_consumer("user");

export const vitals = createState<any>();
const vc = consumers.state("vitals");
const writes: string[] = [];
vc.onWrite("stats", (path: string) => { writes.push(path); });
let scopedWatches = 0;
vc.watch("stats.str", (snap: any) => {
    scopedWatches++;
    echo("SCOPED" + scopedWatches + ":" + JSON.stringify(snap));
    // Per-write deliveries replay AHEAD of coalesced summaries within a flush, so by the
    // second scoped watch the onWrite log is complete. (Timers coalesce into one pump in
    // this harness — turn separation must ride dispatched deliveries, not setTimeout.)
    if (scopedWatches === 2) echo("WRITES:" + JSON.stringify(writes));
});

export const ping = createEvent<any>();
const pc = consumers.event("ping");
pc.once().then((p: any) => echo("ONCE:" + p.n + ":" + Object.isFrozen(p)));
pc.on((p: any) => {
    try { p.n = 99; echo("MUT:silent"); } catch { echo("MUT:threw"); }
});

vitals.set({ hp: 1, stats: { str: 5 } });      // turn 1: root write -> in scope (ancestor)
setTimeout(() => {
    vitals.set("hp", 2);                        // turn 2: outside the scoped subtree
    setTimeout(() => {
        vitals.set("stats.str", 6);             // turn 3: inside
        ping.emit({ n: 7 });
    }, 30);
}, 30);
"#;

/// Module exercising the interned-identity seams (interop-pre-gmcp-plan.md §3): a malformed
/// creator fails loudly at resolve time (handle/API construction, before any per-call op
/// runs), a platform spec that is not a store producer (`sys` — event-only) still fails at
/// the READ (consumer roots resolve lazily, so scheme imports stay link-safe), a platform
/// spec that IS a store producer (`gmcp`) constructs and reads as absent before the host
/// publishes, and a forged root id is refused loudly.
const INTERN_TS: &str = r#"
import { echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;

try { store.set("not json", "x", 1); echo("BADCREATOR:ok"); }
catch (e: any) { echo("BADCREATOR:" + (e?.message ?? String(e))); }

const sysConsumers = (globalThis as any).__smudgy_interop_consumer("sys");
const sys = sysConsumers.state("Something");
echo("SYS_CONSTRUCTED:" + (typeof sys.watch === "function"));
try { const v = sys.value; echo("SYS_READ:ok:" + String(v)); }
catch (e: any) { echo("SYS_READ:" + (e?.message ?? String(e))); }

const consumers = (globalThis as any).__smudgy_interop_consumer("gmcp");
const gmcp = consumers.state("Char");
echo("GMCP_CONSTRUCTED:" + (typeof gmcp.watch === "function"));
try { const v = gmcp.value; echo("GMCP_READ:ok:" + String(v)); }
catch (e: any) { echo("GMCP_READ:" + (e?.message ?? String(e))); }

try { store.setAt(999999, "x", 1); echo("FORGED:ok"); }
catch (e: any) { echo("FORGED:" + (e?.message ?? String(e))); }
"#;

/// Module pinning the two-arg `set` empty-path rejection: a dynamically computed subpath
/// that comes up empty (or blank — the path parser trims) must fail loudly at the call site
/// instead of silently replacing the handle's whole subtree, which stays expressible only
/// through the deliberate forms (single-arg `set`, `.value` assignment).
const EMPTY_SET_TS: &str = r#"
import { createState, echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;

const vitals = createState<any>('vitals');
vitals.set({ hp: 1, mp: 2 });
try { vitals.set("", { hp: 10 }); echo("EMPTY:no-throw"); }
catch (e: any) { echo("EMPTY:" + (e instanceof TypeError) + ":" + String(e?.message).includes("non-empty path")); }
try { vitals.set("  ", { hp: 10 }); echo("BLANK:no-throw"); }
catch (e: any) { echo("BLANK:" + (e instanceof TypeError)); }
echo("INTACT:" + JSON.stringify(store.get("user", "vitals")));
vitals.set({ whole: true });
echo("WHOLE:" + JSON.stringify(store.get("user", "vitals")));
"#;

/// Module pinning the interned-id hardening seams (interop-pre-gmcp-plan.md §3) through the
/// public surface (raw ops are not reachable from modules — `Deno.core` is not exposed):
/// - repeated construction of ONE handle (the per-matched-line author mistake) retains no
///   host memory — far more constructions than the identity cap admits as distinct entries,
///   so the loop only completes if interning dedups;
/// - a consumer root id presented to `setAt` is refused (the seatless branch of
///   `gate_interop_write`), never treated as the non-home no-op or a landed write;
/// - root and event ids live in disjoint tagged spaces, so an id from the wrong family
///   fails loudly even when its index is in range in the other table;
/// - unbounded DISTINCT identities (the consumer glue's ungated resolve looped over fresh
///   specs — the capability-free growth channel) hit the cap's teaching error, while
///   identities interned before the cap keep working.
const ID_HARDENING_TS: &str = r#"
import { createState, createEvent, echo } from "smudgy:core";
const store = (globalThis as any).__smudgy_store;

const vitals = createState<any>('vitals');
vitals.set({ hp: 1 });
const pulse = createEvent<any>('pulse');
for (let i = 0; i < 10000; i++) {
    createState('vitals');
    createEvent('pulse');
}
echo("HOT:ok");

// Resolve a consumer root, then present every plausible root id to setAt: the consumer
// root must surface the read-only refusal (creator/producer roots accept the write, which
// is why ATCAP below reads a leaf rather than the whole subtree).
store.get("user", "");
let refusal = "";
for (let id = 0; id < 64; id++) {
    try { store.setAt(id, "probe", true); }
    catch (e: any) {
        const msg = String(e?.message ?? e);
        if (msg.includes("read-only consumer root")) { refusal = msg; break; }
    }
}
echo("CONSUMERWRITE:" + (refusal !== ""));

// 2147483648 is the first event-tagged id (the tag bit alone): a root-taking op must
// refuse the family, not index its own table.
try { store.getAt(2147483648, ""); echo("EVENTASROOT:no-throw"); }
catch (e: any) { echo("EVENTASROOT:" + String(e?.message ?? e)); }

let capError = "";
try {
    for (let i = 0; i < 20000; i++) store.get("o" + i + "/n", "");
} catch (e: any) { capError = String(e?.message ?? e); }
echo("CAP:" + capError.includes("identity table is full"));
createState('vitals');
echo("ATCAP:" + store.get("user", "vitals.hp"));
"#;

async fn run_module(session_id: u32, server: &str, source: &str) -> Vec<String> {
    run_module_counting_wakes(session_id, server, source).await.0
}

/// Like [`run_module`], but also counts the `StoreBindingsChanged` repaint wakes observed on
/// the session event stream.
async fn run_module_counting_wakes(
    session_id: u32,
    server: &str,
    source: &str,
) -> (Vec<String>, usize) {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let modules_dir = home.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("store_test.ts"), source).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines: Vec<String> = Vec::new();
    let mut wakes = 0usize;
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            SessionEvent::StoreBindingsChanged => wakes += 1,
            _ => {}
        }
    };
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        match event.event {
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            SessionEvent::StoreBindingsChanged => wakes += 1,
            _ => {}
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();
    (lines, wakes)
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

fn position(lines: &[String], needle: &str) -> Option<usize> {
    lines.iter().position(|l| l.contains(needle))
}

#[tokio::test]
async fn scoped_verbs_once_promise_and_frozen_event_payloads() {
    let lines = run_module(7319, "VerbsTest", VERBS_TS).await;
    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "SCOPED1:5"),
        "an ancestor (root) write wakes the scoped watch with the scoped snapshot.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "SCOPED2:6"),
        "a write inside the scope wakes it again.\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("SCOPED3")),
        "the out-of-scope write (hp) must not wake the scoped watch.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"WRITES:["","stats.str"]"#),
        "scoped onWrite hears the ancestor + in-scope writes, handle-relative, and not hp.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ONCE:7:true"),
        "argless once() resolves with the (frozen) payload.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "MUT:threw"),
        "event payloads are delivered deep-frozen; strict-mode mutation throws.\n{transcript}"
    );
}

#[tokio::test]
async fn interned_identity_fails_loudly_and_resolves_consumer_roots_lazily() {
    let lines = run_module(7321, "InternTest", INTERN_TS).await;
    let transcript = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("BADCREATOR:") && l.contains("malformed interop creator")),
        "a malformed creator descriptor fails loudly at resolve (construction) time.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "SYS_CONSTRUCTED:true"),
        "consumer construction for a platform spec must not throw (roots resolve lazily).\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("SYS_READ:") && l.contains("unknown store producer")),
        "an event-only platform spec fails at the read, exactly as before interning.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "GMCP_CONSTRUCTED:true"),
        "consumer construction for the gmcp store producer must not throw.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "GMCP_READ:ok:undefined"),
        "the gmcp producer reads as absent (undefined) before the host publishes.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("FORGED:") && l.contains("unknown interop root id")),
        "a forged root id is refused loudly.\n{transcript}"
    );
}

#[tokio::test]
async fn two_arg_set_rejects_an_empty_path() {
    let lines = run_module(7322, "EmptySetTest", EMPTY_SET_TS).await;
    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "EMPTY:true:true"),
        "two-arg set with an empty path throws the teaching TypeError.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "BLANK:true"),
        "a blank (whitespace-only) path is rejected the same way.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"INTACT:{"hp":1,"mp":2}"#),
        "the rejected calls replaced nothing; sibling keys survive.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"WHOLE:{"whole":true}"#),
        "whole-subtree replacement stays expressible through single-arg set.\n{transcript}"
    );
}

#[tokio::test]
async fn interned_ids_are_deduped_capped_and_family_tagged() {
    let lines = run_module(7323, "IdHardeningTest", ID_HARDENING_TS).await;
    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "HOT:ok"),
        "per-construction resolution of one identity never grows the table (no cap hit).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CONSUMERWRITE:true"),
        "a consumer root id presented to set is refused loudly, never a silent no-op.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("EVENTASROOT:") && l.contains("names an event, not a root")),
        "an event-tagged id handed to a root op fails loudly (disjoint id spaces).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CAP:true"),
        "unbounded distinct identities are refused with the teaching cap error.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ATCAP:1"),
        "identities interned before the cap keep resolving after it is reached.\n{transcript}"
    );
}

#[tokio::test]
async fn inferred_handle_names_flow_from_bindings() {
    let lines = run_module(7317, "InferTest", INFERRED_NAMES_TS).await;
    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == r#"NAMED:{"hp":42}"#),
        "the binding name is the published subtree.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "DYNAMIC:true:true"),
        "nameless dynamic creation throws the teaching TypeError.\n{transcript}"
    );
}

#[tokio::test]
async fn store_round_trip_watch_coalescing_and_flush_before_dispatch() {
    let lines = run_module(7301, "StoreTest", STORE_TS).await;
    let transcript = lines.join("\n");

    // Read-your-writes, with case-folded path lookups (write `Char.Vitals`, read `char.vitals.HP`).
    assert!(lines.iter().any(|l| l == "RW:10"), "read-your-writes within the turn.\n{transcript}");
    // Absent path vs stored null are distinguishable.
    assert!(lines.iter().any(|l| l == "ABSENT:undefined"), "absent reads as undefined.\n{transcript}");
    assert!(lines.iter().any(|l| l == "NIL:null"), "a stored null reads as null.\n{transcript}");

    // Turn-coalesced watch: the module turn's three writes produce ONE delivery with the final
    // state, and it arrives after the turn's synchronous echoes.
    assert!(
        lines.iter().any(|l| l == r#"WATCH1:{"hp":12,"maxhp":20}"#),
        "one coalesced delivery with the turn's final state (order preserved as published).\n{transcript}"
    );
    let sync = position(&lines, "SYNC-AFTER-WRITES").expect("sync marker");
    let watch1 = position(&lines, "WATCH1:").expect("first watch delivery");
    assert!(
        sync < watch1,
        "the watch delivery is asynchronous (next pump), never inside the writing turn.\n{transcript}"
    );

    // The timer turn wrote hp=99 then emitted: the handler observed the flushed value
    // (flush-before-dispatch), and the watcher coalesced that turn into one more delivery.
    assert!(
        lines.iter().any(|l| l == "EMITREAD:99"),
        "an event handler queued after a write reads the flushed value.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"WATCH2:{"hp":99,"maxhp":20}"#),
        "the second turn's write produces a second coalesced delivery.\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("WATCH3:")),
        "two writing turns produce exactly two deliveries.\n{transcript}"
    );
}

#[tokio::test]
async fn unwatch_stops_deliveries_and_enumeration_keeps_first_published_casing() {
    let lines = run_module(7302, "StoreUnwatch", UNWATCH_TS).await;
    let transcript = lines.join("\n");

    assert!(lines.iter().any(|l| l == "W:1"), "the live watch delivers.\n{transcript}");
    assert!(
        !lines.iter().any(|l| l == "W:2"),
        "no delivery after unwatch.\n{transcript}"
    );
    // First-published casing wins for the subtree key (`Grp`) and both folded writes landed.
    assert!(
        lines.iter().any(|l| l == r#"FINAL:{"Foo":1,"Bar":2}"#),
        "first-published casing is preserved; later-cased writes land on the folded keys.\n{transcript}"
    );
    assert!(lines.iter().any(|l| l == "X:2"), "the post-unwatch write still lands.\n{transcript}");
}

#[tokio::test]
async fn on_write_replays_every_write_in_order_with_paths() {
    let lines = run_module(7304, "StoreOnWrite", ONWRITE_TS).await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == r#"OW1:chat.last="hi""#),
        "the first write delivers with its written path.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"OW2:chat.last="hi""#),
        "a value-identical write is a second occurrence (what coalesced watch folds away).\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("OW3:")),
        "a non-comparable write delivers nothing.\n{transcript}"
    );
    let sync = position(&lines, "SYNC").expect("sync marker");
    let first = position(&lines, "OW1:").expect("first per-write delivery");
    assert!(
        sync < first,
        "per-write deliveries arrive on a later pump, never inside the writing turn.\n{transcript}"
    );
}

#[tokio::test]
async fn value_proxy_publishes_set_at_path_per_assignment() {
    let lines = run_module(7305, "StoreValueProxy", VALUE_PROXY_TS).await;
    let transcript = lines.join("\n");

    assert!(lines.iter().any(|l| l == "VAL:11"), "proxy reads see the turn's writes.\n{transcript}");
    assert!(lines.iter().any(|l| l == "NEST:6"), "nested assignment lands at its path.\n{transcript}");
    assert!(lines.iter().any(|l| l == "DEL:undefined"), "delete removes the key.\n{transcript}");
    assert!(lines.iter().any(|l| l == r#"KEYS:["hp"]"#), "enumeration reads live state.\n{transcript}");
    assert!(
        lines.iter().any(|l| l == r#"WHOLE:{"fresh":1}"#),
        "assigning .value replaces the whole published value.\n{transcript}"
    );
    // The proxy's writes each published at exactly the assigned path (interop.md §4a): the bulk
    // set, two leaf assignments, the delete's parent rewrite, and the whole-value assign.
    assert!(
        lines.iter().any(|l| l == r#"PATHS:["vitals","vitals.hp","vitals.stats.str","vitals","vitals"]"#),
        "every assignment is one set-at-path at the assigned path.\n{transcript}"
    );
}

#[tokio::test]
async fn value_proxy_reads_are_leaf_aware_with_overlay_and_consumer_view_is_read_only() {
    let lines = run_module(7320, "StoreProxyRead", PROXY_READ_TS).await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == "NIL:null"),
        "a stored null reads as null through the proxy (absent-vs-null preserved).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ABSENT:undefined"),
        "an absent key reads undefined through the proxy.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "HAS:true:false"),
        "`in` distinguishes stored-null from absent.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ARR:[1,2]:true"),
        "arrays materialize whole and arrive deep-frozen.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "DEEP:1"),
        "object hops mint deeper proxies that read leaves.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"KEYS:["nil","arr","obj","s"]"#),
        "enumeration reads the journal overlay in publish order.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "DESC:function:undefined:true:true:1"),
        "descriptors are lazy accessors (configurable + enumerable, no eager value); the getter resolves the get trap's view.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "DESCABSENT:undefined"),
        "an absent key has no descriptor.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == r#"SPREAD:{"nil":null,"arr":[1,2],"obj":{"k":1},"s":"x"}"#),
        "spread materializes values through the getters.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == r#"JSON:{"nil":null,"arr":[1,2],"obj":{"k":1},"s":"x"}"#),
        "JSON.stringify serializes the live view whole.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "PDEF:true:true"),
        "defineProperty on the producer view throws the teaching TypeError.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "PFREEZE:true:true"),
        "Object.freeze on the producer view throws the teaching TypeError.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "KIND:object:1"),
        "a same-turn write below a scalar changes the path's kind.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"REPLACED:["fresh"]:undefined"#),
        "a same-turn root replace replaces the key set (no stale committed keys).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CVAL:1"),
        "the consumer view reads leaves live through the same overlay.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CSET:true:true"),
        "consumer .value assignment throws the teaching TypeError.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CDEL:threw"),
        "consumer .value delete throws.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CDEF:true:true"),
        "defineProperty on the consumer view throws the read-only teaching TypeError.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CFREEZE:true:true"),
        "Object.freeze on the consumer view throws the read-only teaching TypeError.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"CKEYS:["fresh"]"#),
        "consumer enumeration sees the same overlay.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CSCALAR:number:7"),
        "a scalar-rooted consumer .value reads as the scalar itself.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CARRAY:true:[1,2]:true"),
        "an array-rooted consumer .value reads as the frozen array itself.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CGHOST:undefined"),
        "an absent producer's consumer .value reads as undefined, not a truthy view.\n{transcript}"
    );
}

#[tokio::test]
async fn previous_value_anchors_to_the_newest_write_batch() {
    let lines = run_module(7324, "StorePrevious", PREVIOUS_TS).await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == "P0:undefined:undefined"),
        "before any write there is no previous state on either seat.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "P1:undefined:1"),
        "the first batch's base is absence, while value reads the journal.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "P2:undefined:undefined"),
        "after the first commit the state before the first batch is still absence.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"P3:{"hp":1,"stats":{"str":5}}:2"#),
        "mid-batch, previousValue is the batch's committed base (materializable whole).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"P3KEYS:["hp","stats"]"#),
        "the previous view enumerates the base generation's keys.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "P4:1:1:2"),
        "between batches both seats read the retained generation, not the head.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "P4NEST:5"),
        "the previous view resolves per hop like value.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "PREVRO:true:true:false"),
        "producer-seat previousValue mutation throws the snapshot-base teaching TypeError, not the consumer-seat one.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CPREVRO:true:true"),
        "consumer-seat previousValue mutation throws the consumer-seat teaching TypeError.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "P5:2:3"),
        "a new batch re-anchors previousValue to its own base, superseding the retained one.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "PSCALAR:number:7"),
        "a scalar generation reads whole via the root hop, like value.\n{transcript}"
    );
}

#[tokio::test]
async fn derived_computes_publishes_and_stops_on_off() {
    let lines = run_module(7306, "StoreDerived", DERIVED_TS).await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == "INIT:0.5"),
        "derived computes immediately from the source's current value.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "BINDABLE:number"),
        "the derived value is bindable like any published state.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "RECOMPUTED:0.75"),
        "a source write recomputes and republishes.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "AFTER-OFF:0.75"),
        "after off() the source write no longer recomputes; the last value stays readable.\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l == "RECOMPUTED:0.05"),
        "the post-off write must not recompute.\n{transcript}"
    );
}

#[tokio::test]
async fn procedures_deliver_async_with_stamped_sender_and_queue_briefly() {
    let lines = run_module(7307, "StoreProcedures", PROCEDURES_TS).await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == "MSG:1:user"),
        "a post reaches the implementation with the host-stamped sender.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "SYNC:false"),
        "delivery rides the action queue — never synchronously inside the posting turn.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "EARLY:7"),
        "a post before the implementation exists is buffered and drained at registration (queue-briefly).\n{transcript}"
    );
}

#[tokio::test]
async fn bindings_mint_deduped_tokens_and_wake_per_writing_turn() {
    let (lines, wakes) = run_module_counting_wakes(7303, "StoreBind", BIND_TS).await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == "TOKEN:number:true"),
        "bind() returns a frozen token carrying a numeric id.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "DEDUP:true"),
        "a case-respelled path reuses the same binding id.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "DISTINCT:true"),
        "a different path gets its own binding id.\n{transcript}"
    );
    assert!(lines.iter().any(|l| l == "DONE"), "the module ran to completion.\n{transcript}");
    // Turn 1 (subtree write) and turn 2 (bound-path write) each wake the UI once; turn 3
    // (a write with no binding at, above, or below it) wakes nothing.
    assert_eq!(
        wakes, 2,
        "exactly one repaint wake per flushed turn that touched a bound path.\n{transcript}"
    );
}
