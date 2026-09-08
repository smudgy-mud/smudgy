//! The `official/mudlet-db` package inside Smudgy's embedded runtime.
//!
//! Host Node runs the package's own tests; these prove the part only the real
//! runtime can: `node:sqlite` opens a file-backed database from a **sandboxed**
//! package isolate under the package's `read`/`write: ["$DATA"]` grants, the
//! same code is refused (catchably, with the package's `permission` diagnostic)
//! without them, a **trusted** local module imports the package with the main
//! isolate's full authority, and the converted `calendar-todo-list` package
//! runs end to end from typed input.
//!
//! Packages are served from memory (`spawn_with_package_provider`) with their
//! real sources, so the manifests under `packages/` are the ones exercised.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
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
const CALENDAR_SPEC: &str = "smudgy://smudgy-mud/calendar-todo-list";
const QUIET_PERIOD: Duration = Duration::from_millis(900);

// V8 snapshot deserialization is not safe when several session runtimes start at once on
// Windows (see package_isolates_sandbox.rs); every test here holds this for its whole run.
static RUNTIME_LOCK: Mutex<()> = Mutex::new(());
static NEXT_SESSION: AtomicU32 = AtomicU32::new(7300);

fn module(subpath: &str, text: &str) -> PackageModuleSource {
    PackageModuleSource {
        subpath: subpath.to_string(),
        text: text.to_string(),
    }
}

/// The library, from its checked-in sources.
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
        integrity: "test-mudlet-db".to_string(),
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

/// The converted calendar, from its checked-in sources.
fn calendar_package() -> ResolvedPackage {
    let manifest = PackageManifest::parse(include_str!(
        "../../packages/calendar-todo-list/smudgy.package.json"
    ))
    .expect("valid calendar-todo-list manifest");
    ResolvedPackage {
        key: PackageKey {
            owner: "smudgy-mud".to_string(),
            name: "calendar-todo-list".to_string(),
        },
        resolved_version: manifest.version.clone(),
        manifest,
        integrity: "test-calendar-todo-list".to_string(),
        modules: vec![
            module(
                "index.ts",
                include_str!("../../packages/calendar-todo-list/index.ts"),
            ),
            module(
                "calendar.ts",
                include_str!("../../packages/calendar-todo-list/calendar.ts"),
            ),
        ],
    }
}

/// A consumer package: `{ "version": "1.0.0", "dependencies": [mudlet-db]<extra> }` over one
/// `index.js`. `manifest_extra` is spliced into the manifest object verbatim.
fn consumer_package(name: &str, manifest_extra: &str, source: &str) -> ResolvedPackage {
    let manifest_json = format!(
        r#"{{ "version": "1.0.0", "dependencies": ["{MUDLET_DB_SPEC}"]{manifest_extra} }}"#
    );
    ResolvedPackage {
        key: PackageKey {
            owner: "wbk".to_string(),
            name: name.to_string(),
        },
        resolved_version: "1.0.0".to_string(),
        manifest: PackageManifest::parse(&manifest_json).expect("valid consumer manifest"),
        integrity: format!("test-{name}"),
        modules: vec![module("index.js", source)],
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

/// Set the (first-setter-wins) smudgy home and create `<home>/<server>/` with `modules/` +
/// `logs/`. Returns the server directory.
fn prepare_server(server: &str) -> PathBuf {
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

/// Escape a path for a double-quoted JS string literal.
fn js_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

fn has_line(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|line| line == needle)
}

fn line_starting(lines: &[String], prefix: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(prefix).map(str::to_string))
}

/// Walk `root` for a file called `name`.
fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    None
}

/// Spawn a headless session resolving `smudgy://` from `factory`, drain its output until it
/// goes quiet, then send each of `inputs` (waiting for quiet after each) and drain again.
async fn run_session(
    server: &str,
    factory: PackageProviderFactory,
    inputs: &[&str],
) -> Vec<String> {
    let session_id = SessionId::from(NEXT_SESSION.fetch_add(1, Ordering::SeqCst));
    let params = Arc::new(SessionParams {
        session_id,
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
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    };

    tx.send(RuntimeAction::ApplySettings {
        command_separator: Arc::new(";".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: true,
        bold_is_bright: false,
        script_settings: Box::new(smudgy_core::models::settings::ScriptSettings::default()),
    })
    .unwrap();

    let mut pending = inputs.iter();
    loop {
        match tokio::time::timeout(QUIET_PERIOD, events.next()).await {
            Ok(Some(event)) => {
                if let SessionEvent::UpdateBuffer(updates) = event.event {
                    for update in updates.iter() {
                        if let BufferUpdate::Append(line) = update {
                            lines.push(line.text.clone());
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(_) => match pending.next() {
                Some(input) => tx
                    .send(RuntimeAction::Send(Arc::new((*input).to_string())))
                    .expect("runtime accepts input"),
                None => break,
            },
        }
    }

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

/// Install `spec` untrusted (its own sandboxed isolate) with the consent `permissions`, plus
/// `echo` so the package can report.
fn install_with_consent(
    server: &str,
    spec: &str,
    mut permissions: smudgy_script::PackagePermissions,
) {
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    permissions.smudgy.echo = true;
    shared_packages::record_consent(server, spec, &permissions).unwrap();
}

/// A consumer that opens a file database in its data dir, writes, reads, closes and reopens.
const PROBE_SOURCE: &str = r#"
    import { echo, getDataDir } from "smudgy:core";
    import { openDatabase, eq, timestamp, CURRENT_TIMESTAMP, MudletDbError } from "smudgy://official/mudlet-db";
    const dir = getDataDir();
    echo("DATADIR:" + dir);
    const schema = { kills: { mob: "", count: 0, seen: timestamp(CURRENT_TIMESTAMP), _unique: ["mob"], _violations: "REPLACE" } };
    try {
      const db = openDatabase({ name: "probe", directory: dir }, schema);
      echo("OPENED:" + db.path);
      const { kills } = db.sheets;
      kills.add({ mob: "goblin", count: 3 }, { mob: "orc", count: 1 });
      kills.add({ mob: "goblin", count: 4 });
      const goblin = kills.fetchOne(eq(kills.fields.mob, "goblin"));
      echo("GOBLIN:" + goblin.count + ":" + (goblin.seen instanceof Date) + ":" + kills.count());
      goblin.count = 5;
      kills.update(goblin);
      db.close();
      const again = openDatabase({ name: "probe", directory: dir }, schema);
      const back = again.sheets.kills.fetchOne(eq(again.sheets.kills.fields.mob, "goblin"));
      echo("REOPENED:" + again.sheets.kills.count() + ":" + back.count + ":" + back._row_id);
      again.close();
      try { again.sheets.kills.fetch(); } catch (e) { echo("CLOSED:" + (e instanceof MudletDbError ? e.code : "other")); }
    } catch (e) {
      echo("OPEN_ERR:" + (e instanceof MudletDbError ? e.code : (e?.name ?? String(e))) + ":" + e.message);
    }
    echo("DONE");
"#;

const REGRESSION_SOURCE: &str = r#"
    import { echo, getDataDir } from "smudgy:core";
    import { linkSync, readFileSync, writeFileSync } from "node:fs";
    import { openDatabase, copyDatabase, MudletDbError } from "smudgy://official/mudlet-db";
    function check(condition, message) { if (!condition) throw new Error(message); }
    function fails(work, code, message) {
      try { work(); } catch (e) {
        check(e instanceof MudletDbError && e.code === code && e.message.includes(message), String(e));
        return;
      }
      throw new Error("expected " + code);
    }
    const dir = getDataDir().replaceAll("\\", "/");
    const canSnapshot = __CAN_SNAPSHOT__;
    const path = dir + "/regression.db";
    const original = { people: { name: "", city: "", _unique: ["name"], _violations: "ROLLBACK" } };
    let db = openDatabase(path, original);
    db.sheets.people.add({ name: "Ada", city: "Boston" });
    db.close();

    // Identity checks must work with the embedded runtime's stat implementation.
    linkSync(path, dir + "/alias.db");
    const before = readFileSync(path).toString("hex");
    fails(() => copyDatabase(path, dir + "/alias.db", { overwrite: true }), "argument", "two different files");
    check(readFileSync(path).toString("hex") === before, "copy damaged source");
    if (canSnapshot) {
      copyDatabase(path, dir + "/copy.db");
      copyDatabase(path, dir + "/copy.db", { overwrite: true });
    } else {
      fails(() => copyDatabase(path, dir + "/copy.db"), "unsupported", "local module");
      fails(() => copyDatabase(path, dir + "/alias.db", { overwrite: true }), "argument", "two different files");
    }
    writeFileSync(dir + "/corrupt.db", "not a database");
    fails(() => copyDatabase(dir + "/corrupt.db", path, { overwrite: true }), "sqlite", "");
    check(readFileSync(path).toString("hex") === before, "failed copy damaged destination");
    echo("REGRESSION_COPY_OK");

    fails(() => openDatabase(path, {
      people: { name: "", city: "", _unique: ["city"], _violations: "ROLLBACK" }
    }), "schema-mismatch", "unique");
    check(readFileSync(path).toString("hex") === before, "schema refusal changed the file");
    echo("REGRESSION_SCHEMA_OK");

    db = openDatabase(path, { people: { name: "", _unique: ["name"], _violations: "ROLLBACK" } });
    const people = db.sheets.people;
    people.update({ ...people.fetchOne(), name: "Alice" });
    people.mergeUnique([{ name: "Alice" }]);
    check(people.fetchOne().city === "Boston", "extra column did not survive update and merge");
    echo("REGRESSION_EXTRA_COLUMNS_OK");

    db.begin();
    people.add({ name: "pending" });
    fails(() => people.add({ name: "Alice" }), "sqlite", "UNIQUE constraint failed");
    check(!db.inTransaction && people.count() === 1, "manual transaction state was not reset");
    db.begin();
    people.add({ name: "Ben" });
    db.commit();
    fails(() => db.transaction(() => {
      people.add({ name: "pending" });
      fails(() => people.add({ name: "Alice" }), "sqlite", "UNIQUE constraint failed");
      fails(() => people.add({ name: "must not commit" }), "transaction", "rolled back");
    }), "transaction", "rolled back");
    check(!db.inTransaction && people.count() === 2, "callback allowed unplanned commits");
    db.transaction(() => people.add({ name: "Carol" }));
    check(people.count() === 3, "database did not recover");
    db.close();
    echo("REGRESSION_TRANSACTION_OK");
    echo("DONE");
"#;

#[allow(clippy::await_holding_lock)] // RUNTIME_LOCK serialises whole sessions, awaits included
#[tokio::test]
async fn package_regressions_hold_in_the_sandboxed_runtime() {
    let _guard = RUNTIME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = "mudlet_db_regressions";
    prepare_server(server);
    let consumer = consumer_package(
        "dbregressions",
        r#", "permissions": { "read": ["$DATA"], "write": ["$DATA"] }"#,
        &REGRESSION_SOURCE.replace("__CAN_SNAPSHOT__", "false"),
    );
    let factory = factory_for(vec![mudlet_db_package(), consumer]);
    install_with_consent(
        server,
        "smudgy://wbk/dbregressions",
        factory().closure_permissions(),
    );
    let lines = run_session(server, factory, &[]).await;
    assert_regression_lines(&lines);
}

#[allow(clippy::await_holding_lock)] // RUNTIME_LOCK serialises whole sessions, awaits included
#[tokio::test]
async fn package_regressions_hold_in_a_trusted_local_module() {
    let _guard = RUNTIME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = "mudlet_db_regressions_main";
    let server_dir = prepare_server(server);
    std::fs::write(
        server_dir.join("modules").join("regressions.ts"),
        REGRESSION_SOURCE.replace("__CAN_SNAPSHOT__", "true"),
    )
    .unwrap();
    let lines = run_session(server, factory_for(vec![mudlet_db_package()]), &[]).await;
    assert_regression_lines(&lines);
}

fn assert_regression_lines(lines: &[String]) {
    for expected in [
        "REGRESSION_COPY_OK",
        "REGRESSION_SCHEMA_OK",
        "REGRESSION_EXTRA_COLUMNS_OK",
        "REGRESSION_TRANSACTION_OK",
        "DONE",
    ] {
        assert!(
            has_line(lines, expected),
            "missing {expected:?}; transcript:\n{lines:#?}"
        );
    }
}

#[allow(clippy::await_holding_lock)] // RUNTIME_LOCK serialises whole sessions, awaits included
#[tokio::test]
async fn sandboxed_consumer_keeps_a_file_database_in_its_data_dir() {
    let _guard = RUNTIME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = "mudlet_db_sandboxed";
    let server_dir = prepare_server(server);
    let consumer = consumer_package(
        "dbprobe",
        r#", "permissions": { "read": ["$DATA"], "write": ["$DATA"] }"#,
        PROBE_SOURCE,
    );
    let factory = factory_for(vec![mudlet_db_package(), consumer]);
    install_with_consent(
        server,
        "smudgy://wbk/dbprobe",
        factory().closure_permissions(),
    );

    let lines = run_session(server, factory, &[]).await;

    let data_dir = PathBuf::from(line_starting(&lines, "DATADIR:").expect("DATADIR line"));
    assert!(
        data_dir.starts_with(server_dir.join(".isolate-storage")),
        "getDataDir() must be the package's isolate storage; transcript:\n{lines:#?}"
    );
    let opened = line_starting(&lines, "OPENED:")
        .unwrap_or_else(|| panic!("no OPENED line; transcript:\n{lines:#?}"));
    assert_eq!(
        Path::new(&opened),
        data_dir.join("Database_probe.db"),
        "the file carries Mudlet's name inside $DATA; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "GOBLIN:4:true:2"),
        "REPLACE upsert + timestamp; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "REOPENED:2:5:3"),
        "rows persist across close/reopen; the REPLACE upsert re-inserted goblin as _row_id 3; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "CLOSED:closed"),
        "a closed handle throws the closed diagnostic; transcript:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("OPEN_ERR:")),
        "transcript:\n{lines:#?}"
    );
    assert!(has_line(&lines, "DONE"));
    assert!(
        data_dir.join("Database_probe.db").exists(),
        "the database file must be on disk after the session; transcript:\n{lines:#?}"
    );
}

#[allow(clippy::await_holding_lock)] // RUNTIME_LOCK serialises whole sessions, awaits included
#[tokio::test]
async fn sandboxed_consumer_without_file_grants_gets_a_permission_diagnostic() {
    let _guard = RUNTIME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = "mudlet_db_denied";
    prepare_server(server);
    let consumer = consumer_package("dbdenied", "", PROBE_SOURCE);
    let factory = factory_for(vec![mudlet_db_package(), consumer]);
    install_with_consent(
        server,
        "smudgy://wbk/dbdenied",
        factory().closure_permissions(),
    );

    let lines = run_session(server, factory, &[]).await;

    let error = line_starting(&lines, "OPEN_ERR:").unwrap_or_else(|| {
        panic!("the open must fail without read/write grants; transcript:\n{lines:#?}")
    });
    assert!(
        error.starts_with("permission:") && error.contains("\"read\" and \"write\""),
        "the failure must carry the package's permission code and name the manifest fix; got {error}; transcript:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("OPENED:")),
        "transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "DONE"),
        "the denial is catchable; transcript:\n{lines:#?}"
    );
}

#[allow(clippy::await_holding_lock)] // RUNTIME_LOCK serialises whole sessions, awaits included
#[tokio::test]
async fn trusted_local_module_imports_the_package_with_main_authority() {
    let _guard = RUNTIME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = "mudlet_db_main";
    let server_dir = prepare_server(server);
    let path = server_dir.join("anywhere").join("main_probe.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let module_src = r#"
        import { echo } from "smudgy:core";
        import { openDatabase, eq, MudletDbError } from "smudgy://official/mudlet-db";
        try {
          const db = openDatabase("__PATH__", { notes: { text: "" } });
          db.sheets.notes.add({ text: "from main" });
          echo("MAIN_DB:" + db.sheets.notes.fetchOne(eq(db.sheets.notes.fields.text, "from main")).text);
          db.close();
        } catch (e) { echo("MAIN_DB_ERR:" + (e instanceof MudletDbError ? e.code : (e?.name ?? String(e))) + ":" + e.message); }
        echo("DONE");
    "#
    .replace("__PATH__", &js_path(&path));
    std::fs::write(server_dir.join("modules").join("main_db.ts"), module_src).unwrap();

    // Nothing installed: the package is only imported by the trusted local module.
    let lines = run_session(server, factory_for(vec![mudlet_db_package()]), &[]).await;

    assert!(
        has_line(&lines, "MAIN_DB:from main"),
        "transcript:\n{lines:#?}"
    );
    assert!(
        path.exists(),
        "the main isolate may open a file anywhere; transcript:\n{lines:#?}"
    );
}

#[allow(clippy::await_holding_lock)] // RUNTIME_LOCK serialises whole sessions, awaits included
#[tokio::test]
async fn calendar_package_runs_end_to_end_from_typed_input() {
    let _guard = RUNTIME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = "mudlet_db_calendar";
    let server_dir = prepare_server(server);
    let calendar = calendar_package();
    let factory = factory_for(vec![mudlet_db_package(), calendar.clone()]);
    // Exactly the manifest's own asks: proves the declared grants are the ones it needs.
    install_with_consent(server, CALENDAR_SPEC, calendar.manifest.permissions.clone());

    let lines = run_session(
        server,
        factory,
        &[
            "todo visit shipyard;todo buy bait;todo",
            "done 1;done;reset done;todo",
            "event Council = 01/01/01;event;delevent 1;recycle events",
        ],
    )
    .await;

    for expected in [
        "To-do item 'visit shipyard' added to the database.",
        "To-do item 'buy bait' added to the database.",
        "To-do item 'visit shipyard' marked as done.",
        "Cleared all todos marked as complete.",
        "Event 'Council' at '01/01/01' added to the database.",
        "Event 'Council' at '01/01/01' deleted from database.",
    ] {
        assert!(
            has_line(&lines, expected),
            "missing {expected:?}; transcript:\n{lines:#?}"
        );
    }
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("%% 1      %% visit shipyard")),
        "the open-items listing shows the first to-do at position 1; transcript:\n{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("%% 1      %% buy bait")),
        "after reset done the remaining to-do is at position 1; transcript:\n{lines:#?}"
    );
    let file = find_file(&server_dir.join(".isolate-storage"), "Database_calendar.db")
        .unwrap_or_else(|| panic!("Database_calendar.db must exist under the package's data dir; transcript:\n{lines:#?}"));
    assert!(file.metadata().unwrap().len() > 0);
}
