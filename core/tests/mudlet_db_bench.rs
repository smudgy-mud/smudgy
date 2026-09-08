//! Timings for `official/mudlet-db` inside Smudgy's embedded runtime: the
//! workload of `packages/mudlet-db/test/perf.test.ts`, run from a sandboxed
//! package against a file in its data dir, so the figures are what a trigger
//! callback pays. Ignored by default; run with
//!
//! ```text
//! cargo test -p smudgy_core --test mudlet_db_bench --release -- --ignored --nocapture
//! ```
//!
//! Each pass opens a fresh file. Every timing is echoed as a `DBBENCH` JSON
//! line and printed here as a table of medians. The row generator, repeat
//! counts and operation names match the Mudlet-side script kept with the
//! comparison results, so the two clients can be read side by side.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::{RuntimeAction, RuntimeThreadJoinOutcome, join_runtime_thread};
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams,
    spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackageProvider,
    ResolvedPackage,
};

const MUDLET_DB_SPEC: &str = "smudgy://official/mudlet-db";
const BENCH_SPEC: &str = "smudgy://wbk/dbbench";
const QUIET_PERIOD: Duration = Duration::from_millis(1500);

fn module(subpath: &str, text: &str) -> PackageModuleSource {
    PackageModuleSource {
        subpath: subpath.to_string(),
        text: text.to_string(),
    }
}

fn mudlet_db_package() -> ResolvedPackage {
    let manifest =
        PackageManifest::parse(include_str!("../../packages/mudlet-db/smudgy.package.json"))
            .expect("valid mudlet-db manifest");
    ResolvedPackage {
        key: PackageKey {
            owner: "official".to_string(),
            name: "mudlet-db".to_string(),
        },
        resolved_version: manifest.version.clone(),
        manifest,
        integrity: "bench-mudlet-db".to_string(),
        modules: vec![
            module(
                "index.ts",
                include_str!("../../packages/mudlet-db/index.ts"),
            ),
            module("copy.ts", include_str!("../../packages/mudlet-db/copy.ts")),
            module(
                "database.ts",
                include_str!("../../packages/mudlet-db/database.ts"),
            ),
            module(
                "errors.ts",
                include_str!("../../packages/mudlet-db/errors.ts"),
            ),
            module(
                "names.ts",
                include_str!("../../packages/mudlet-db/names.ts"),
            ),
            module(
                "query.ts",
                include_str!("../../packages/mudlet-db/query.ts"),
            ),
            module(
                "schema.ts",
                include_str!("../../packages/mudlet-db/schema.ts"),
            ),
            module(
                "sqlite.ts",
                include_str!("../../packages/mudlet-db/sqlite.ts"),
            ),
            module(
                "values.ts",
                include_str!("../../packages/mudlet-db/values.ts"),
            ),
        ],
    }
}

/// The workload. Keep the row generator, repeat counts and operation names in
/// step with the Mudlet-side Lua script.
const BENCH_SOURCE: &str = r#"
    import { echo, getDataDir } from "smudgy:core";
    import { rmSync } from "node:fs";
    import { openDatabase, eq, like, timestamp, CURRENT_TIMESTAMP } from "smudgy://official/mudlet-db";

    const ROWS = 10000;
    const AUTOCOMMIT_ROWS = 1000;
    const PASSES = 3;
    const REPEAT = { fetch_indexed: 100, fetch_compound: 100, fetch_all: 5, fetch_like: 20, count: 100, update_one: 100, set_20: 20 };
    const schema = {
      docking_records: {
        character: "", name: "", type: "", owner: "", planet: "", dock: "",
        time: timestamp(CURRENT_TIMESTAMP),
        _index: ["name", ["character", "name"]],
      },
    };
    const location = { name: "dbbench", directory: getDataDir() };
    const row = (index) => ({
      character: `Char${index % 7}`,
      name: `Ship ${index % 500}`,
      type: index % 3 === 0 ? "YT-1300" : "Lambda shuttle",
      owner: `Owner ${index % 50}`,
      planet: `Planet ${index % 20}`,
      dock: `Dock ${index % 9}`,
    });

    function timed(pass, op, n, work) {
      const start = performance.now();
      const result = work();
      const ms = performance.now() - start;
      echo("DBBENCH " + JSON.stringify({ pass, op, ms, n }));
      return result;
    }

    function freshFile() {
      try { rmSync(getDataDir() + "/Database_dbbench.db"); } catch {}
    }

    for (let pass = 1; pass <= PASSES; pass += 1) {
      freshFile();
      const db = timed(pass, "open_fresh", 1, () => openDatabase(location, schema));
      const sheet = db.sheets.docking_records;
      const f = sheet.fields;

      timed(pass, "add_autocommit", AUTOCOMMIT_ROWS, () => {
        for (let index = 0; index < AUTOCOMMIT_ROWS; index += 1) sheet.add(row(index));
      });
      timed(pass, "add_transaction", ROWS - AUTOCOMMIT_ROWS, () => {
        db.transaction(() => {
          for (let index = AUTOCOMMIT_ROWS; index < ROWS; index += 1) sheet.add(row(index));
        });
      });
      if (sheet.count() !== ROWS) throw new Error("row count " + sheet.count());

      let rows = timed(pass, "fetch_indexed", REPEAT.fetch_indexed, () => {
        let r; for (let i = 0; i < REPEAT.fetch_indexed; i += 1) r = sheet.fetch(eq(f.name, "Ship 42")); return r;
      });
      if (rows.length !== 20) throw new Error("Ship 42 rows " + rows.length);
      rows = timed(pass, "fetch_compound", REPEAT.fetch_compound, () => {
        let r; for (let i = 0; i < REPEAT.fetch_compound; i += 1) r = sheet.fetch([eq(f.character, "Char0"), eq(f.name, "Ship 42")]); return r;
      });
      if (rows.length < 1) throw new Error("compound rows");
      rows = timed(pass, "fetch_all", REPEAT.fetch_all, () => {
        let r; for (let i = 0; i < REPEAT.fetch_all; i += 1) r = sheet.fetch(); return r;
      });
      if (rows.length !== ROWS) throw new Error("all rows " + rows.length);
      rows = timed(pass, "fetch_like", REPEAT.fetch_like, () => {
        let r; for (let i = 0; i < REPEAT.fetch_like; i += 1) r = sheet.fetch(like(f.name, "Ship 4%")); return r;
      });
      if (rows.length === 0) throw new Error("like rows");
      timed(pass, "count", REPEAT.count, () => {
        let c; for (let i = 0; i < REPEAT.count; i += 1) c = sheet.count(); return c;
      });
      const target = sheet.fetchOne(eq(f.name, "Ship 42"));
      timed(pass, "update_one", REPEAT.update_one, () => {
        for (let i = 1; i <= REPEAT.update_one; i += 1) { target.dock = "Dock X" + i; sheet.update(target); }
      });
      timed(pass, "set_20", REPEAT.set_20, () => {
        for (let i = 1; i <= REPEAT.set_20; i += 1) sheet.set("planet", "Elsewhere " + i, eq(f.name, "Ship 42"));
      });
      timed(pass, "delete_20", 1, () => sheet.delete(eq(f.name, "Ship 43")));
      if (sheet.count() !== ROWS - 20) throw new Error("after delete " + sheet.count());
      timed(pass, "close", 1, () => db.close());
    }
    freshFile();
    echo("DBBENCH done");
"#;

fn bench_package() -> ResolvedPackage {
    let manifest_json = format!(
        r#"{{ "version": "1.0.0", "dependencies": ["{MUDLET_DB_SPEC}"], "permissions": {{ "read": ["$DATA"], "write": ["$DATA"] }} }}"#
    );
    ResolvedPackage {
        key: PackageKey {
            owner: "wbk".to_string(),
            name: "dbbench".to_string(),
        },
        resolved_version: "1.0.0".to_string(),
        manifest: PackageManifest::parse(&manifest_json).expect("valid bench manifest"),
        integrity: "bench-dbbench".to_string(),
        modules: vec![module("index.js", BENCH_SOURCE)],
    }
}

fn factory_for(packages: Vec<ResolvedPackage>) -> PackageProviderFactory {
    Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        for package in &packages {
            provider.insert(package.clone());
        }
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    })
}

/// Spawn a headless session and collect every line until the package's `DBBENCH done`.
async fn run_session(server: &str, factory: PackageProviderFactory) -> Vec<String> {
    let session_id = SessionId::from(7400);
    let params = Arc::new(SessionParams {
        session_id,
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("bench".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn_with_package_provider(params, factory));
    let mut lines: Vec<String> = Vec::new();
    let mut tx = None;
    loop {
        let event = match tokio::time::timeout(Duration::from_mins(5), events.next()).await {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(elapsed) => panic!("{elapsed} waiting for the benchmark; transcript:\n{lines:#?}"),
        };
        match event.event {
            SessionEvent::RuntimeReady(sender) => tx = Some(sender),
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
        if tx.is_some() && lines.iter().any(|line| line == "DBBENCH done") {
            break;
        }
    }
    // Let any trailing buffer flush arrive before shutting down.
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
    }
    let tx = tx.expect("runtime became ready");
    tx.send(RuntimeAction::Shutdown)
        .expect("runtime accepts shutdown");
    drop(tx);
    drop(events);
    let joined = tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
        .await
        .expect("runtime join task does not panic");
    assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });
    lines
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        f64::midpoint(values[middle - 1], values[middle])
    } else {
        values[middle]
    }
}

#[tokio::test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
#[allow(clippy::too_many_lines)]
async fn mudlet_db_workload_in_the_embedded_runtime() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let server = "mudlet_db_bench";
    let server_dir = smudgy_core::get_smudgy_home().unwrap().join(server);
    std::fs::create_dir_all(server_dir.join("modules")).unwrap();
    std::fs::create_dir_all(server_dir.join("logs")).unwrap();

    let factory = factory_for(vec![mudlet_db_package(), bench_package()]);
    shared_packages::install_package(server, BENCH_SPEC, UpdateMode::Auto, true).unwrap();
    let mut consent = factory().closure_permissions();
    consent.smudgy.echo = true;
    shared_packages::record_consent(server, BENCH_SPEC, &consent).unwrap();

    let lines = run_session(server, factory).await;

    // op -> (n, per-pass total ms)
    let mut samples: BTreeMap<String, (u64, Vec<f64>)> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for line in &lines {
        let Some(json) = line.strip_prefix("DBBENCH ") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            continue;
        };
        let op = value["op"].as_str().unwrap_or_default().to_string();
        let ms = value["ms"].as_f64().unwrap_or_default();
        let n = value["n"].as_u64().unwrap_or(1);
        if !order.contains(&op) {
            order.push(op.clone());
        }
        samples
            .entry(op)
            .or_insert_with(|| (n, Vec::new()))
            .1
            .push(ms);
    }
    assert!(
        lines.iter().any(|line| line == "DBBENCH done") && !samples.is_empty(),
        "the benchmark package must report; transcript:\n{lines:#?}"
    );

    println!();
    println!(
        "{:<16} {:>6} {:>12} {:>12} {:>14}  passes (total ms)",
        "op", "n", "median ms", "per-op ms", "min..max ms"
    );
    for op in &order {
        let (n, totals) = &samples[op];
        let mut sorted = totals.clone();
        let med = median(&mut sorted);
        let min = sorted.first().copied().unwrap_or_default();
        let max = sorted.last().copied().unwrap_or_default();
        let passes = totals
            .iter()
            .map(|ms| format!("{ms:.1}"))
            .collect::<Vec<_>>()
            .join(" ");
        #[allow(clippy::cast_precision_loss)]
        let per_op = med / *n as f64;
        println!("{op:<16} {n:>6} {med:>12.2} {per_op:>12.4} {min:>6.1}..{max:<6.1}  {passes}");
    }
    println!();
}
