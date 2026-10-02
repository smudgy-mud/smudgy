mod packages;
mod scenario;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering::Relaxed},
    time::{Duration, Instant},
};

use deno_core::OpState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    schema: u32,
    engine: String,
    layout_package_path: Option<String>,
    mapper_package_path: Option<String>,
    scripts_package_path: Option<String>,
    area_keys: Vec<String>,
    modes: Vec<String>,
    repetitions: u32,
    case_duration_ms: u64,
    output_label: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: 1,
            engine: "synthetic-current".into(),
            layout_package_path: None,
            mapper_package_path: None,
            scripts_package_path: None,
            area_keys: vec!["synthetic".into()],
            modes: vec![
                "baseline".into(),
                "quiet".into(),
                "quiet-topology".into(),
                "tidy".into(),
                "perfect".into(),
            ],
            repetitions: 1,
            case_duration_ms: 10_000,
            output_label: "synthetic-host".into(),
        }
    }
}

/// The same monotonic clock timestamps native probes, event receipt and inputs.
pub struct Sink {
    start: Instant,
    samples: Mutex<Vec<Sample>>,
    probe_ns: std::sync::atomic::AtomicU64,
    probe_max_ns: std::sync::atomic::AtomicU64,
    probe_calls: std::sync::atomic::AtomicU64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    ms: f64,
    kind: String,
    data: Value,
}

impl Sink {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            samples: Mutex::new(Vec::new()),
            probe_ns: std::sync::atomic::AtomicU64::new(0),
            probe_max_ns: std::sync::atomic::AtomicU64::new(0),
            probe_calls: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn record(&self, kind: &str, data: Value) {
        let ms = self.start.elapsed().as_secs_f64() * 1000.0;
        self.samples.lock().unwrap().push(Sample {
            ms,
            kind: kind.into(),
            data,
        });
    }

    pub fn snapshot(&self) -> Vec<Sample> {
        self.samples.lock().unwrap().clone()
    }

    pub fn has(&self, kind: &str) -> bool {
        self.samples
            .lock()
            .unwrap()
            .iter()
            .any(|sample| sample.kind == kind)
    }

    pub fn active_job(&self) -> Option<Value> {
        self.active_jobs().into_iter().next()
    }

    pub fn latest_planner(&self, realm: &str) -> Option<Value> {
        let samples = self.samples.lock().unwrap();
        samples
            .iter()
            .rev()
            .find(|sample| sample.kind == "planner" && sample.data["realm"] == realm)
            .map(|sample| sample.data.clone())
    }

    pub fn active_jobs(&self) -> Vec<Value> {
        ["mapper", "scripts"]
            .iter()
            .filter_map(|realm| self.latest_planner(realm))
            .filter(|job| matches!(job["status"].as_str(), Some("planning" | "repairing")))
            .collect()
    }

    pub fn mode_active(&self, mode: &str) -> bool {
        let realm = if mode == "perfect" {
            "scripts"
        } else {
            "mapper"
        };
        self.latest_planner(realm).is_some_and(|job| {
            matches!(job["status"].as_str(), Some("planning" | "repairing"))
                && match mode {
                    "quiet" => job["source"] == "nukefire:auto-polish",
                    // Repair phases originate in actual Worker progress events.
                    "quiet-topology" => {
                        job["source"] == "nukefire:auto-polish" && job["status"] == "repairing"
                    }
                    "tidy" => job["source"] == "nukefire:tidy",
                    "perfect" => true,
                    _ => false,
                }
        })
    }

    pub fn initial_chart_ready(&self) -> bool {
        self.samples.lock().unwrap().iter().any(|sample| {
            sample.kind == "snapshot-synchronized" && sample.data["allowExistingReflow"] == false
        })
    }

    pub fn probe_cost(&self) -> Value {
        json!({"calls":self.probe_calls.load(Relaxed),
            "totalNs":self.probe_ns.load(Relaxed),"maxNs":self.probe_max_ns.load(Relaxed)})
    }
}

#[deno_core::op2(fast)]
fn op_packing_measure(state: &OpState, #[string] kind: &str, #[string] payload: &str) {
    // One sparse callback per planner phase/terminal; heartbeat payload is tiny.
    let sink = state.borrow::<Arc<Sink>>();
    let started = Instant::now();
    sink.record(kind, serde_json::from_str(payload).unwrap_or(Value::Null));
    let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    sink.probe_ns.fetch_add(elapsed, Relaxed);
    sink.probe_max_ns.fetch_max(elapsed, Relaxed);
    sink.probe_calls.fetch_add(1, Relaxed);
}

deno_core::extension!(
    packing_measure,
    ops = [op_packing_measure],
    options = { sink: Arc<Sink> },
    state = |state, options| state.put(options.sink),
    customizer = |ext: &mut deno_core::Extension| {
        ext.esm_files = std::borrow::Cow::Borrowed(PROBE_ESM);
        ext.esm_entry_point = Some("ext:packing_measure/probe.js");
    },
);

static PROBE_ESM: &[deno_core::ExtensionFileSource] = &[deno_core::ExtensionFileSource::new(
    "ext:packing_measure/probe.js",
    deno_core::ascii_str!("globalThis.__packingMeasure = Deno.core.ops.op_packing_measure;"),
)];

/// Reject absolute paths, traversal and symlink escapes for artifacts/configs.
pub fn below(root: &Path, relative: &str) -> PathBuf {
    let path = Path::new(relative);
    assert!(
        path.components().all(|component| matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )),
        "relative path required"
    );
    let candidate = root.join(path);
    let mut ancestor = candidate.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().expect("benchmark root exists");
    }
    assert!(
        ancestor.canonicalize().unwrap().starts_with(root),
        "path must remain inside benchmark root"
    );
    if candidate.exists() {
        candidate.canonicalize().unwrap()
    } else {
        candidate
    }
}

pub fn create_below(root: &Path, relative: &str) -> PathBuf {
    let candidate = below(root, relative);
    std::fs::create_dir_all(&candidate).unwrap();
    let resolved = candidate.canonicalize().unwrap();
    assert!(
        resolved.starts_with(root),
        "created directory must remain inside benchmark root"
    );
    resolved
}

pub fn unique() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

pub async fn run() {
    let root = std::env::var_os("SMUDGY_PACKING_BENCH_ROOT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/packing-host-synthetic"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let config: Config = std::env::var("SMUDGY_PACKING_BENCH_CONFIG").map_or_else(
        |_| Config::default(),
        |name| serde_json::from_slice(&std::fs::read(below(&root, &name)).unwrap()).unwrap(),
    );
    assert_eq!(config.schema, 1);
    assert!((1_000..=10_000).contains(&config.case_duration_ms));
    assert!((1..=20).contains(&config.repetitions));
    for mode in &config.modes {
        assert!(
            ["baseline", "quiet", "quiet-topology", "tidy", "perfect"].contains(&mode.as_str())
        );
    }
    let run_dir = create_below(&root, &format!("host/{}-{}", config.output_label, unique()));
    let home = create_below(&run_dir, "home");
    smudgy_core::set_smudgy_home(&home);
    assert_eq!(
        smudgy_core::get_smudgy_home()
            .unwrap()
            .canonicalize()
            .unwrap(),
        home.canonicalize().unwrap()
    );
    let config_bytes = serde_json::to_vec_pretty(&config).unwrap();
    let config_hash = format!("{:x}", sha2::Sha256::digest(&config_bytes));
    std::fs::write(run_dir.join("config.json"), config_bytes).unwrap();
    std::fs::write(run_dir.join("config.sha256"), config_hash).unwrap();
    let mut results = Vec::new();
    for area_key in &config.area_keys {
        for repetition in 0..config.repetitions {
            for mode in &config.modes {
                let label = format!("{area_key}-{mode}-{repetition}");
                let case_dir = create_below(&run_dir, &label);
                let result =
                    scenario::run_case(&root, &case_dir, &home, &config, area_key, mode).await;
                results.push(result);
                println!("host benchmark completed {mode} repetition {repetition}");
            }
        }
    }
    std::fs::write(
        run_dir.join("summary.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
    println!("host benchmark results: {}", run_dir.display());
}

pub fn duration(config: &Config) -> Duration {
    Duration::from_millis(config.case_duration_ms)
}

#[test]
fn simultaneous_mapper_activity_does_not_hide_scripts_activity() {
    let sink = Sink::new();
    sink.record(
        "planner",
        json!({"realm":"mapper","status":"planning","source":"nukefire:auto-polish"}),
    );
    sink.record("planner", json!({"realm":"scripts","status":"repairing"}));
    assert!(sink.mode_active("quiet"));
    assert!(!sink.mode_active("quiet-topology"));
    sink.record(
        "planner",
        json!({"realm":"mapper","status":"repairing","phase":"constraint compaction","source":"nukefire:auto-polish"}),
    );
    assert!(sink.mode_active("quiet-topology"));
    assert!(sink.mode_active("perfect"));
    assert_eq!(sink.active_jobs().len(), 2);
}
