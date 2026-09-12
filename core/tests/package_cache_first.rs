//! Offline session start, served cache-first from the persistent package cache.
//!
//! A **published** package whose version is staged in the lockfile (pinned, or an Auto
//! install's `last_resolved_version`) names immutable content; once the disk cache holds
//! its metadata and code blobs, a session must boot it with the cloud entirely
//! unreachable — and, stronger, without a single connection attempt: the staged-version
//! disk verification replaced `cap_version`'s start-time probe, and the solve walks read
//! cached metas. These tests populate the cache + lockfile by hand, then boot a real
//! session through the genuine `SmudgyPackageProvider` behind a **counting** dead cloud:
//! every accepted connection is tallied and then dropped, so an attempt both registers
//! and fails fast. Zero accepted connections across a whole warm-cache start is the
//! strict form of the offline guarantee; the fallback tests assert the opposite — a
//! cache gap or a consent mismatch must reach for the network path (and, with the cloud
//! dead, land on the legacy offline behavior). The lockfile carries nothing beyond the
//! staged version and its last verified stamp: a pre-existing install is servable offline
//! from plain cache metadata.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use smudgy_cloud::{Credential, CredentialSource, PackageApiClient, ResolvedDependency};
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::runtime::package_cache::{CachedModule, CachedResolution, PackageCache};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};
use smudgy_script::{PackageKey, PackageManifest};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

// ---------------------------------------------------------------------------
// Harness — real provider, warm package cache, counting dead cloud
// ---------------------------------------------------------------------------

/// First-setter-wins process-global smudgy home; create `<home>/<server>/{modules,logs}`.
/// Each integration file is its own test binary, so the `OnceLock` home is clean here;
/// tests within the file share the winning home and are kept apart by server name.
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

/// A cloud endpoint that must never be needed: every accepted connection is counted,
/// then dropped without a response, so an attempt both registers in `hits` and errors
/// fast at the client. A load that succeeds with `hits == 0` was necessarily served
/// with zero package HTTP.
struct DeadCloud {
    base_url: String,
    hits: Arc<AtomicUsize>,
}

impl DeadCloud {
    fn client(&self) -> PackageApiClient {
        PackageApiClient::new(
            self.base_url.clone(),
            CredentialSource::new(Some(Credential::ApiKey("test".into()))),
        )
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn spawn_dead_cloud() -> DeadCloud {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let base_url = format!("http://{}", listener.local_addr().expect("local_addr"));
    let hits = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&hits);
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            count.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    DeadCloud { base_url, hits }
}

fn sha256_hex(body: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

/// Warm the process-global package cache with a fully offline-servable published
/// version: its meta (carrying the given locked dep edges) plus the single `index.ts`
/// code blob. Returns the integrity fingerprint a verified load would stamp into the lockfile.
fn cache_package(
    owner: &str,
    name: &str,
    version: &str,
    manifest_json: &str,
    index_src: &str,
    deps: &[ResolvedDependency],
) -> String {
    let cache = PackageCache::new().expect("package cache under smudgy home");
    let hash = sha256_hex(index_src);
    cache.write_blob(&hash, index_src).expect("write blob");
    let integrity = format!("index.ts={hash}");
    let meta = CachedResolution {
        version: version.to_string(),
        integrity: integrity.clone(),
        manifest: PackageManifest::parse(manifest_json).expect("valid manifest"),
        modules: vec![CachedModule {
            subpath: "index.ts".to_string(),
            content_hash: hash,
            media_type: "application/typescript".to_string(),
            byte_size: index_src.len() as i64,
            is_entry: true,
        }],
        dependencies: deps.to_vec(),
    };
    let key = PackageKey {
        owner: owner.to_string(),
        name: name.to_string(),
    };
    cache.write_meta(&key, version, &meta).expect("write meta");
    integrity
}

/// Record the consented closure grant on an installed row, as the install flow does.
fn record_consent(server: &str, specifier: &str, manifest_json: &str) {
    let permissions = PackageManifest::parse(manifest_json)
        .expect("valid manifest")
        .permissions;
    let lock = shared_packages::load_lock(server).expect("lock loads");
    let row = lock.find(specifier).expect("installed row");
    assert!(
        shared_packages::record_consent_if_unchanged(server, row, &permissions)
            .expect("record consent"),
        "the freshly installed row is unchanged"
    );
}

fn dep(owner: &str, name: &str, range: &str, version: &str) -> ResolvedDependency {
    ResolvedDependency {
        owner_nickname: owner.to_string(),
        name: name.to_string(),
        range: range.to_string(),
        resolved_version: version.to_string(),
        kind: smudgy_cloud::DependencyKind::Dependency,
    }
}

/// Spawn the session against the REAL cloud-backed provider and the given client,
/// collecting every appended line (notices included) until quiet.
async fn run_session(session_id: u32, server: &str, client: PackageApiClient) -> Vec<String> {
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: Some(client),
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
        if let Some(line) = update.main_text() {
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

/// The marquee offline start, strict form: a sandboxed Auto install **with a locked
/// dependency** (staged via `last_resolved_version`, closure consented), a trusted
/// pinned install, and a trusted Auto install staged the same way — all fully cached.
/// The session boots every one of them with **zero connection attempts** at the cloud:
/// the staged-version disk verification covers the sandbox's consent check, the solve
/// walks read cached metas, and the code loads serve cache-first.
#[tokio::test]
async fn offline_session_start_loads_published_packages_cache_first() {
    let server = "OfflineCacheFirst";
    prepare_server(server);
    let cloud = spawn_dead_cloud();

    // Sandboxed: an Auto install whose staged version determines the cache-first serve,
    // importing a locked dependency (also cached) — the consent verification folds BOTH
    // manifests from disk. The root grants itself `session: echo`; the dep grants
    // nothing, so the closure union equals the root's grant.
    let dep_manifest = r#"{ "name": "offline-base", "version": "1.0.0" }"#;
    cache_package(
        "arctic",
        "offline-base",
        "1.0.0",
        dep_manifest,
        r#"export const base = "DEP";"#,
        &[],
    );
    let sandbox_manifest = r#"{ "name": "offline-widget", "version": "1.0.0",
         "dependencies": ["smudgy://arctic/offline-base@^1"],
         "permissions": { "smudgy": { "session": ["echo"] } } }"#;
    let sandbox_integrity = cache_package(
        "arctic",
        "offline-widget",
        "1.0.0",
        sandbox_manifest,
        r#"import { base } from "smudgy://arctic/offline-base";
           import { echo } from "smudgy:core";
           echo("SANDBOX:CACHE:LOADED:" + base);"#,
        &[dep("arctic", "offline-base", "^1", "1.0.0")],
    );
    let sandbox_spec = "smudgy://arctic/offline-widget";
    shared_packages::install_package(server, sandbox_spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_resolution(server, sandbox_spec, "1.0.0", &sandbox_integrity).unwrap();
    record_consent(server, sandbox_spec, sandbox_manifest);

    // Trusted: a PINNED install (no prior resolution recorded), so the pin alone
    // determines the version. Runs in the main isolate, allow-all.
    let trusted_manifest = r#"{ "name": "offline-trusted", "version": "2.0.0" }"#;
    cache_package(
        "arctic",
        "offline-trusted",
        "2.0.0",
        trusted_manifest,
        r#"import { echo } from "smudgy:core"; echo("TRUSTED:CACHE:LOADED");"#,
        &[],
    );
    let trusted_spec = "smudgy://arctic/offline-trusted";
    shared_packages::install_package(
        server,
        trusted_spec,
        UpdateMode::Pinned {
            version: "2.0.0".to_string(),
        },
        true,
    )
    .unwrap();
    shared_packages::set_trusted(server, trusted_spec, true).unwrap();

    // Trusted: an AUTO install with a staged prior resolution — the main-isolate solve
    // must resolve it AT the staged version from cached meta instead of asking the
    // cloud what latest means.
    let auto_manifest = r#"{ "name": "offline-auto", "version": "3.1.0" }"#;
    let auto_integrity = cache_package(
        "arctic",
        "offline-auto",
        "3.1.0",
        auto_manifest,
        r#"import { echo } from "smudgy:core"; echo("TRUSTED:AUTO:CACHE:LOADED");"#,
        &[],
    );
    let auto_spec = "smudgy://arctic/offline-auto";
    shared_packages::install_package(server, auto_spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_resolution(server, auto_spec, "3.1.0", &auto_integrity).unwrap();
    shared_packages::set_trusted(server, auto_spec, true).unwrap();

    let lines = run_session(9901, server, cloud.client()).await;

    assert!(
        has_line(&lines, "SANDBOX:CACHE:LOADED:DEP"),
        "the sandboxed Auto install loads offline at its staged version, dependency included: {lines:?}"
    );
    assert!(
        has_line(&lines, "TRUSTED:CACHE:LOADED"),
        "the trusted pinned install loads offline from cache: {lines:?}"
    );
    assert!(
        has_line(&lines, "TRUSTED:AUTO:CACHE:LOADED"),
        "the trusted Auto install loads offline at its staged version: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("not loaded")),
        "no package load was refused: {lines:?}"
    );
    assert_eq!(
        cloud.hits(),
        0,
        "a warm-cache session start performs zero package HTTP attempts"
    );
}

/// A staged version whose cached closure union EXCEEDS the consented grant must not
/// ride the disk fast path. The load falls back to the `cap_version` walk, which tries
/// the cloud and then reuses the cached metadata to enforce the same permission gate
/// offline. The package stays blocked until the user grants the added permission; it is
/// never run with a silently reduced sandbox.
#[tokio::test]
async fn staged_version_exceeding_consent_is_refused_offline() {
    let server = "StagedOverAsk";
    prepare_server(server);
    let cloud = spawn_dead_cloud();

    // The cached manifest asks for `net` on top of the echo grant; consent only ever
    // covered echo (an update escalation the user has not accepted).
    let manifest = r#"{ "name": "over-asker", "version": "1.0.0",
         "permissions": { "smudgy": { "session": ["echo"] }, "net": ["203.0.113.9"] } }"#;
    let integrity = cache_package(
        "arctic",
        "over-asker",
        "1.0.0",
        manifest,
        r#"import { echo } from "smudgy:core"; echo("OVERASK:LOADED");"#,
        &[],
    );
    let spec = "smudgy://arctic/over-asker";
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_resolution(server, spec, "1.0.0", &integrity).unwrap();
    record_consent(
        server,
        spec,
        r#"{ "name": "over-asker", "version": "1.0.0",
             "permissions": { "smudgy": { "session": ["echo"] } } }"#,
    );

    let lines = run_session(9902, server, cloud.client()).await;

    assert!(
        !has_line(&lines, "OVERASK:LOADED"),
        "a version that exceeds consent must not run while offline: {lines:?}"
    );
    assert!(
        has_line(
            &lines,
            "available versions need more permissions than you've granted"
        ),
        "the session explains why the cached version stayed blocked: {lines:?}"
    );
    assert!(
        cloud.hits() > 0,
        "an over-asking closure union must decline the disk fast path and reach for the network"
    );
}

/// A staged version whose cached meta declares a dependency with NO cached meta of its
/// own must not ride the disk fast path either — an incomplete closure fold proves
/// nothing about consent. The load falls back to the legacy walk (network attempts),
/// which offline still serves the root from cache.
#[tokio::test]
async fn missing_dep_meta_falls_back_to_the_legacy_path() {
    let server = "StagedDepGap";
    prepare_server(server);
    let cloud = spawn_dead_cloud();

    // The root's meta names a dep the cache has never seen; the root's own code does
    // not import it, so the offline load itself can still succeed.
    let manifest = r#"{ "name": "gap-root", "version": "1.0.0",
         "permissions": { "smudgy": { "session": ["echo"] } } }"#;
    let integrity = cache_package(
        "arctic",
        "gap-root",
        "1.0.0",
        manifest,
        r#"import { echo } from "smudgy:core"; echo("GAPROOT:LOADED");"#,
        &[dep("arctic", "gap-dep", "^1", "1.0.0")],
    );
    let spec = "smudgy://arctic/gap-root";
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    shared_packages::record_resolution(server, spec, "1.0.0", &integrity).unwrap();
    record_consent(server, spec, manifest);

    let lines = run_session(9903, server, cloud.client()).await;

    assert!(
        has_line(&lines, "GAPROOT:LOADED"),
        "the root still loads offline from its own cached content: {lines:?}"
    );
    assert!(
        cloud.hits() > 0,
        "a missing dep meta must decline the disk fast path and reach for the network"
    );
}
