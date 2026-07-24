//! Trigger **churn from hot context** — the costs of create/delete vs
//! enable/disable that neither `engine_scan` (steady state) nor
//! `engine_build` (the isolated rebuild stall) shows end to end. The engine's
//! asymmetry (`core/src/session/runtime/trigger.rs`): create/delete mark the
//! `PatternSet`s dirty and the NEXT incoming line pays a full four-tier
//! rebuild over the ENTIRE trigger population, while enable/disable just
//! flips a flag — but `PatternSet::build` includes disabled triggers, so a
//! disabled trigger keeps paying scan cost on every line, forever. Neither
//! cost is visible from the API; these numbers are the source for the docs'
//! guidance ("disable for mode switches, delete for permanent removal", or
//! whatever they actually say).
//!
//! Group `churn_residue` (Manager-level, no V8 — the steady-state side of
//! the trade): per-line scan cost over one log corpus, comparing D triggers
//! **disabled** against the same D **absent**, with the enabled set held
//! IDENTICAL across each pair (composition, not just count — different name
//! subsets hit the corpus differently and would confound the delta).
//! `disabled_D − absent_D` is the residue disabling leaves in a scan tier —
//! measured per tier, because the tiers should differ:
//! - `literal` pairs (D item-name literals): the Aho-Corasick pass is
//!   O(line) regardless of pattern count, so the expected residue is ~zero
//!   and automaton-representation shifts can even make the LARGER set scan
//!   faster. A ~zero delta here is a finding, not a failed measurement: at
//!   the literal tier, disable-and-forget is cheap.
//! - `regex` pairs (D of the corpus-matching `REGEX_TRIGGERS`): a disabled
//!   match-prone regex still pays its prefilter hits and full-regex
//!   confirmations before the per-hit `enabled` check discards the match —
//!   the tier where residue is real work.
//!
//! Group `churn_packet` (live sessions — the mutation side, in your face
//! mid-combat): a ~4 KB packet (50 same-length filler lines) processed by a
//! session carrying 1000 resident triggers, with a mode-switch line at the
//! front whose handler mutates 20 combat triggers. The first filler line
//! after the mutation pays the full-population rebuild inside the dispatch
//! path. Cells against the same session/population:
//! - `clean`: the packet alone — the floor.
//! - `toggle20`: the mode line flips `enabled` on 20 pre-created triggers —
//!   should sit on the floor (no dirty flag).
//! - `create_delete20`: the mode line creates 20 triggers (next pass deletes
//!   them, alternating) — the floor plus the rebuild stall.
//! - `create_delete20_x4pkg` (own session): the same 20 mutations split
//!   across 4 sandboxed package isolates (5 each) — adds per-isolate
//!   dispatch entry to the same shared-Manager rebuild.
//!
//! The `create_delete20 − clean` delta is the mid-packet stall a user feels
//! when a script mutates triggers during combat; `toggle20 − clean` ≈ 0 is
//! the payoff of enable/disable. Cross-check `engine_build/dirty_rebuild`
//! for the same stall without the session around it.
//!
//! Env vars: `SMUDGY_BENCH_LINES=n` truncates the residue corpus
//! (truncate-only); `SMUDGY_BENCH_SKIP_SANITY=1` skips the checks that
//! disabled triggers don't fire (and enabled ones do), and that the mode
//! handler's create/delete really take effect through live dispatch.

use std::{
    cell::RefCell,
    collections::VecDeque,
    env,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use smudgy_bench::{
    REGEX_TRIGGERS, load_item_names_10k, load_log_lines,
    session::{BenchPackage, BenchSession, bench_runtime, styled},
};
use smudgy_core::session::{
    runtime::{
        IsolateId, Manager, Origin, PushTriggerParams, RuntimeAction, ScriptAction,
        SharedAutomationRegistry,
    },
    styled_line::StyledLine,
};
use smudgy_script::{PackagePermissions, SmudgyCapabilities};

type Queue = Rc<RefCell<VecDeque<RuntimeAction>>>;

// ---------------------------------------------------------------------------
// churn_residue: disabled-vs-absent, Manager-level
// ---------------------------------------------------------------------------

/// Literal triggers registered in the residue cells.
const RESIDUE_POPULATION: usize = 10_000;
/// How many of them are disabled (or absent, in the paired cell).
const RESIDUE_D: &[usize] = &[1_000, 5_000];

/// `trigger_engine`'s push helper plus the `enabled` knob this bench is
/// about.
fn push_one_trigger(
    mgr: &mut Manager,
    name: String,
    pattern: String,
    action: ScriptAction,
    enabled: bool,
) {
    let trigger_name = Arc::new(name);
    let patterns = Arc::new(vec![pattern]);
    let empty: Arc<Vec<String>> = Arc::new(Vec::new());
    mgr.push_trigger(PushTriggerParams {
        isolate: IsolateId::Main,
        origin: Origin::User,
        name: &trigger_name,
        patterns: &patterns,
        raw_patterns: &empty,
        anti_patterns: &empty,
        action,
        prompt: false,
        enabled,
        priority: 0,
        fallthrough: false,
        fire_limit: None,
        line_limit: None,
        source: None,
    })
    .expect("push_trigger");
}

/// A Manager whose ENABLED sets are `names[lit_d..population]` and
/// `REGEX_TRIGGERS[rex_d..]` — identical across each disabled/absent pair
/// (the enabled triggers' corpus hit-rate must not vary between the two
/// cells, or subset composition confounds the residue delta). The first
/// `lit_d` names / `rex_d` regexes are pushed disabled when
/// `include_disabled`, and not pushed at all otherwise. Warmed past the
/// initial build.
fn residue_manager(
    names: &[String],
    population: usize,
    lit_d: usize,
    rex_d: usize,
    include_disabled: bool,
) -> (Manager, Queue) {
    let queue: Queue = Rc::new(RefCell::new(VecDeque::new()));
    let registry = SharedAutomationRegistry::default();
    let mut mgr = Manager::new(queue.clone(), Arc::new(String::from(";")), registry);
    let lit_start = if include_disabled { 0 } else { lit_d };
    for (i, name) in names[lit_start..population].iter().enumerate() {
        push_one_trigger(
            &mut mgr,
            format!("item_{i}"),
            regex::escape(name),
            ScriptAction::Noop,
            lit_start + i >= lit_d,
        );
    }
    let rex_start = if include_disabled { 0 } else { rex_d };
    for (i, pattern) in REGEX_TRIGGERS[rex_start..].iter().enumerate() {
        push_one_trigger(
            &mut mgr,
            format!("regex_{i}"),
            (*pattern).to_owned(),
            ScriptAction::Noop,
            rex_start + i >= rex_d,
        );
    }
    mgr.process_incoming_line(&Arc::new(StyledLine::new("warmup", Vec::new())))
        .expect("warmup build");
    queue.borrow_mut().clear();
    (mgr, queue)
}

/// Disabled triggers must not fire; enabling one must take effect with no
/// explicit rebuild step (the flag flip is the whole mutation).
fn residue_sanity() {
    let probe = Arc::new(StyledLine::new("zzqx residue probe zzqx", Vec::new()));
    let queue: Queue = Rc::new(RefCell::new(VecDeque::new()));
    let registry = SharedAutomationRegistry::default();
    let mut mgr = Manager::new(queue.clone(), Arc::new(String::from(";")), registry);
    push_one_trigger(
        &mut mgr,
        "zz_probe".to_owned(),
        regex::escape("zzqx residue probe zzqx"),
        ScriptAction::SendRaw(Arc::new(String::from("zz"))),
        false,
    );
    mgr.process_incoming_line(&probe).expect("probe disabled");
    assert!(
        queue.borrow().is_empty(),
        "a disabled trigger must not fire"
    );
    mgr.enable_trigger(&IsolateId::Main, &Origin::User, "zz_probe", true);
    mgr.process_incoming_line(&probe).expect("probe enabled");
    assert!(
        !queue.borrow().is_empty(),
        "an enable flag-flip must take effect on the next line without a create/delete"
    );
    eprintln!("  residue sanity: disabled triggers are inert; enable is a live flag flip");
}

fn churn_residue(c: &mut Criterion) {
    let names = load_item_names_10k();
    let population = RESIDUE_POPULATION.min(names.len());
    let lines = load_log_lines();
    let styled_lines: Vec<Arc<StyledLine>> = lines
        .iter()
        .map(|l| Arc::new(StyledLine::new(l, Vec::new())))
        .collect();
    eprintln!(
        "churn_residue: {population} literal + {} regex triggers, {} corpus lines; \
         disabled_D vs absent_D over D in {RESIDUE_D:?}",
        REGEX_TRIGGERS.len(),
        styled_lines.len()
    );
    if env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
        residue_sanity();
    }

    let mut group = c.benchmark_group("churn_residue");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Elements(styled_lines.len() as u64));

    let scan = |mgr: &mut Manager, queue: &Queue, b: &mut criterion::Bencher| {
        b.iter(|| {
            for line in &styled_lines {
                mgr.process_incoming_line(line).expect("process line");
            }
            queue.borrow_mut().clear();
        });
    };

    // The full-population reference (everything enabled). Only the
    // disabled/absent PAIRS are directly comparable — each pair shares one
    // enabled set; `full` has a different (larger) one.
    let (mut mgr, queue) = residue_manager(&names, population, 0, 0, false);
    group.bench_function(BenchmarkId::new("full", population), |b| {
        scan(&mut mgr, &queue, b);
    });
    drop((mgr, queue));

    // The literal-tier pairs (expected residue ~zero; see the header).
    for &d in RESIDUE_D {
        let (mut mgr, queue) = residue_manager(&names, population, d, 0, true);
        group.bench_function(BenchmarkId::new("literal_disabled", d), |b| {
            scan(&mut mgr, &queue, b);
        });
        drop((mgr, queue));

        let (mut mgr, queue) = residue_manager(&names, population, d, 0, false);
        group.bench_function(BenchmarkId::new("literal_absent", d), |b| {
            scan(&mut mgr, &queue, b);
        });
        drop((mgr, queue));
    }

    // The regex-tier pair: half the corpus-matching regexes disabled vs
    // absent — the tier where a disabled trigger still pays prefilter +
    // full-regex work before the enabled check discards its hits.
    let rex_d = REGEX_TRIGGERS.len() / 2;
    let (mut mgr, queue) = residue_manager(&names, population, 0, rex_d, true);
    group.bench_function(BenchmarkId::new("regex_disabled", rex_d), |b| {
        scan(&mut mgr, &queue, b);
    });
    drop((mgr, queue));
    let (mut mgr, queue) = residue_manager(&names, population, 0, rex_d, false);
    group.bench_function(BenchmarkId::new("regex_absent", rex_d), |b| {
        scan(&mut mgr, &queue, b);
    });
    drop((mgr, queue));
    group.finish();
}

// ---------------------------------------------------------------------------
// churn_packet: the mid-packet mutation stall, live sessions
// ---------------------------------------------------------------------------

/// Resident (never-matching) triggers the session carries — the population
/// the rebuild is proportional to.
const RESIDENT_TRIGGERS: usize = 1_000;
/// Filler lines per packet (~80 bytes each ≈ a 4 KB server packet).
const PACKET_LINES: usize = 50;
/// Combat triggers the mode handler mutates per packet.
const COMBAT_TRIGGERS: usize = 20;

const PACKET_DONE: &str = "ZZPKTDONE";

/// The filler line: matches nothing, fixed length.
const FILLER: &str =
    "the caravan rolls slowly across the tundra under a pale sun xxxxxxxxxxxxxxxxxxx";

/// The main-session module: residents + barrier + the two mode handlers
/// (toggle / create-delete) + the combat sanity probe.
fn packet_main_script(residents: usize, combat: usize) -> String {
    format!(
        r#"
import {{ echo, createTrigger }} from "smudgy:core";

// The resident population: distinct never-matching literal patterns.
for (let i = 0; i < {residents}; i++) {{
    createTrigger(new RegExp("^zzresident_" + String(i).padStart(4, "0") + " never$"), () => {{}}, {{ name: "resident_" + i }});
}}

// Pre-created combat set for the TOGGLE cell: enable/disable is a flag
// flip, no PatternSet dirty mark.
const toggles = [];
for (let i = 0; i < {combat}; i++) {{
    toggles.push(createTrigger(new RegExp("^zztoggle_" + i + " \\d+$"), () => {{}}, {{ name: "toggle_" + i }}));
}}
let togglesOn = true;
createTrigger(/^ZZTOGGLE$/, () => {{
    togglesOn = !togglesOn;
    for (const t of toggles) t.enabled = togglesOn;
}}, {{ name: "mode_toggle" }});

// The CREATE/DELETE cell: alternate passes create and delete the set, so
// every pass mutates and the next line pays the rebuild.
let combatSet = null;
createTrigger(/^ZZMODE$/, () => {{
    if (combatSet === null) {{
        combatSet = [];
        for (let i = 0; i < {combat}; i++) {{
            combatSet.push(createTrigger(
                new RegExp("^zzcombat_" + i + " \\d+$"),
                () => {{ if (i === 0) echo("ZZCOMBAT_HIT"); }},
                {{ name: "combat_" + i }},
            ));
        }}
    }} else {{
        for (const t of combatSet) t.delete();
        combatSet = null;
    }}
}}, {{ name: "mode_churn" }});

createTrigger(/^ZZPKTSYNC$/, () => {{ echo("{PACKET_DONE}"); }}, {{ name: "pkt_sync" }});
echo("PACKET_READY");
"#
    )
}

/// One churn package for the cross-sandbox cell: toggles its own 5 combat
/// triggers on the shared ZZMODE line, in its own isolate.
fn packet_package_script(tag: usize, combat: usize) -> String {
    format!(
        r#"
import {{ createTrigger }} from "smudgy:core";
let combatSet = null;
createTrigger(/^ZZMODE$/, () => {{
    if (combatSet === null) {{
        combatSet = [];
        for (let i = 0; i < {combat}; i++) {{
            combatSet.push(createTrigger(new RegExp("^zzpkg{tag}_" + i + " \\d+$"), () => {{}}, {{ name: "pkg{tag}_combat_" + i }}));
        }}
    }} else {{
        for (const t of combatSet) t.delete();
        combatSet = null;
    }}
}}, {{ name: "pkg{tag}_mode" }});
"#
    )
}

struct PacketCell {
    id: &'static str,
    lines: Vec<Arc<StyledLine>>,
}

/// Feed one packet shape per pass and drain to the barrier echo.
fn packet_pass(
    rt: &tokio::runtime::Runtime,
    session: &mut BenchSession,
    lines: &[Arc<StyledLine>],
    iters: u64,
) -> Duration {
    rt.block_on(async {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            session.drain_stragglers();
            let start = Instant::now();
            for line in lines {
                session.feed(line);
            }
            session.drain_until(PACKET_DONE).await;
            total += start.elapsed();
        }
        total
    })
}

/// Warmup + the live-dispatch churn sanity: the mode handler's create must
/// make a combat trigger fire on the very next matching line, and its delete
/// must make the same line inert again.
fn packet_warmup(rt: &tokio::runtime::Runtime, session: &mut BenchSession, id: &str, sanity: bool) {
    let mut transcript = Vec::new();
    rt.block_on(async {
        assert!(
            session.drain_collect_until("PACKET_READY", &mut transcript).await,
            "{id}: packet module never loaded; transcript:\n{transcript:#?}"
        );
        if sanity {
            // Create pass: ZZMODE creates the set; the probe line must fire
            // combat_0's echo. Delete pass: the same probe must be inert.
            for line in [styled("ZZMODE"), styled("zzcombat_0 42"), styled("ZZPKTSYNC")] {
                session.feed(&line);
            }
            let mut t = Vec::new();
            assert!(session.drain_collect_until(PACKET_DONE, &mut t).await, "{id}: churn sanity barrier");
            assert!(
                t.iter().any(|l| l == "ZZCOMBAT_HIT"),
                "{id}: a created combat trigger must fire on the next matching line; transcript:\n{t:#?}"
            );
            for line in [styled("ZZMODE"), styled("zzcombat_0 42"), styled("ZZPKTSYNC")] {
                session.feed(&line);
            }
            let mut t = Vec::new();
            assert!(session.drain_collect_until(PACKET_DONE, &mut t).await, "{id}: delete sanity barrier");
            assert!(
                !t.iter().any(|l| l == "ZZCOMBAT_HIT"),
                "{id}: a deleted combat trigger must be inert; transcript:\n{t:#?}"
            );
        } else {
            // Even without checks, run one ZZMODE pair so the alternating
            // state starts consistently and the first build is paid.
            for line in [styled("ZZMODE"), styled("ZZMODE"), styled("ZZPKTSYNC")] {
                session.feed(&line);
            }
            let mut t = Vec::new();
            assert!(session.drain_collect_until(PACKET_DONE, &mut t).await, "{id}: warmup barrier");
        }
    });
    session.drain_stragglers();
}

fn churn_packet(c: &mut Criterion) {
    let sanity = env::var("SMUDGY_BENCH_SKIP_SANITY").is_err();
    eprintln!(
        "churn_packet: {PACKET_LINES}-line packets against {RESIDENT_TRIGGERS} resident \
         triggers; mode line mutates {COMBAT_TRIGGERS} combat triggers; sanity checks {}",
        if sanity { "on" } else { "off" }
    );

    let rt = bench_runtime();
    let mut main_session = BenchSession::start(
        &rt,
        "ZZChurnMain",
        9601,
        &[(
            "bench.js",
            packet_main_script(RESIDENT_TRIGGERS, COMBAT_TRIGGERS),
        )],
        &[],
    );
    packet_warmup(&rt, &mut main_session, "main", sanity);

    // The cross-sandbox session: the same main module (residents + barrier +
    // its own mode handlers, unused there) plus 4 churn packages splitting
    // the 20 mutations. Consent: trigger creation only.
    let packages: Vec<BenchPackage> = (0..4)
        .map(|tag| BenchPackage {
            owner: "bench",
            name: match tag {
                0 => "churn-a",
                1 => "churn-b",
                2 => "churn-c",
                _ => "churn-d",
            },
            source: packet_package_script(tag, COMBAT_TRIGGERS / 4),
            consent: PackagePermissions {
                smudgy: SmudgyCapabilities {
                    create_triggers: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        })
        .collect();
    let mut x4_session = BenchSession::start(
        &rt,
        "ZZChurnX4",
        9602,
        &[(
            "bench.js",
            packet_main_script(RESIDENT_TRIGGERS, COMBAT_TRIGGERS),
        )],
        &packages,
    );
    packet_warmup(&rt, &mut x4_session, "x4", sanity);

    let filler: Vec<Arc<StyledLine>> = std::iter::repeat_with(|| styled(FILLER))
        .take(PACKET_LINES)
        .collect();
    let packet = |head: Option<&str>| -> Vec<Arc<StyledLine>> {
        head.map(styled)
            .into_iter()
            .chain(filler.iter().cloned())
            .chain(std::iter::once(styled("ZZPKTSYNC")))
            .collect()
    };

    let main_cells = [
        PacketCell {
            id: "clean",
            lines: packet(None),
        },
        PacketCell {
            id: "toggle20",
            lines: packet(Some("ZZTOGGLE")),
        },
        PacketCell {
            id: "create_delete20",
            lines: packet(Some("ZZMODE")),
        },
    ];

    let mut group = c.benchmark_group("churn_packet");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Elements(PACKET_LINES as u64));
    for cell in &main_cells {
        group.bench_function(cell.id, |b| {
            b.iter_custom(|iters| packet_pass(&rt, &mut main_session, &cell.lines, iters));
        });
    }
    {
        let lines = packet(Some("ZZMODE"));
        group.bench_function("create_delete20_x4pkg", |b| {
            b.iter_custom(|iters| packet_pass(&rt, &mut x4_session, &lines, iters));
        });
    }
    group.finish();
}

fn trigger_churn(c: &mut Criterion) {
    churn_residue(c);
    churn_packet(c);
}

criterion_group!(benches, trigger_churn);
criterion_main!(benches);
