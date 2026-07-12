//! A sandboxed package isolate runs under a **restricted**
//! `PermissionsContainer` built from its manifest `permissions`, unioned across its
//! `smudgy://` dependency closure (`script/PACKAGE-ISOLATES-ENFORCEMENT.md`). The main
//! isolate (user scripts, local modules, trusted packages) stays allow-all.
//!
//! Each test is hermetic, adapting the permissions spike's pattern
//! (`script/tests/permissions_spike.rs`) to the real engine: the "allowed" net host is a
//! local one-shot-per-connection HTTP server on `127.0.0.1`; the "denied" host is a port
//! with nothing listening, rejected at the permission gate *before* any connect (so no
//! prompt, no hang). The permission-sensitive call runs at the package module's top level
//! (so it settles during `load_modules`, which pumps the event loop through evaluation)
//! and reports its outcome via `echo`, which the harness collects from the session buffer.
//!
//! Drives enforcement through a **real installed package's manifest**: packages are
//! injected via the in-memory `PackageProvider` seam (the `package_isolates_sandbox.rs`
//! style), each installed untrusted so the engine sandboxes it. The manifest's
//! `permissions` block — parsed by `PackageManifest`, unioned by the provider's
//! `closure_permissions()` — is what the factory turns into the isolate's container.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread::JoinHandle;
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

/// Time the collector waits for the next buffer event before declaring the session idle.
const QUIET_PERIOD: Duration = Duration::from_millis(900);

// ---------------------------------------------------------------------------
// Hermetic local HTTP server (the "allowed" net host)
// ---------------------------------------------------------------------------

/// Spawn an HTTP/1.1 server on `127.0.0.1:0` that answers every GET with `200 OK` / body
/// `ok` until its listener is dropped. Returns the bound port and the server thread's
/// handle (held by the caller to keep the port bound for the test's lifetime). Each
/// request's headers are drained before responding so closing the socket cannot
/// RST-truncate the response. Mirrors `permissions_spike.rs`, but loops so a test may make
/// more than one allowed request.
fn spawn_http_200_server() -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr").port();
    let handle = std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

            let mut request = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }

            let body = b"ok";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
        }
    });
    (port, handle)
}

/// Spawn an HTTP/1.1 server on `127.0.0.1:0` answering every GET with `200 OK` and `body` as an ES
/// module (`Content-Type: application/javascript`) until its listener is dropped. The "allowed"
/// *import* host: a sandboxed package's `import("http://127.0.0.1:<port>/…")` fetches real module
/// source the loader transpiles + evaluates. Mirrors [`spawn_http_200_server`], but the body is JS
/// so the fetched module actually loads.
fn spawn_js_module_server(body: &str) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr").port();
    let body = body.to_string();
    let handle = std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

            let mut request = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
        }
    });
    (port, handle)
}

/// A port with nothing listening (bound, then released). Used as the "denied" net target:
/// the permission gate rejects it before any connect, so its being closed never matters —
/// but a buggy gate that let it through would surface a *connection* error, not a
/// permission error, and fail the test.
fn unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    listener.local_addr().expect("local_addr").port()
}

// ---------------------------------------------------------------------------
// Package + provider fixtures
// ---------------------------------------------------------------------------

/// Build a `ResolvedPackage` whose manifest is `{ "name", "version" <extra> }` and whose
/// single module is `index.js` = `module_src`. `manifest_extra` is appended verbatim
/// inside the manifest object (e.g. `, "permissions": { "net": ["127.0.0.1:8080"] }`).
fn make_package(
    owner: &str,
    name: &str,
    version: &str,
    manifest_extra: &str,
    module_src: &str,
) -> ResolvedPackage {
    let manifest_json =
        format!(r#"{{ "name": "{name}", "version": "{version}"{manifest_extra} }}"#);
    ResolvedPackage {
        key: PackageKey {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        resolved_version: version.to_string(),
        manifest: PackageManifest::parse(&manifest_json).expect("valid manifest"),
        integrity: format!("test-{name}-{version}"),
        modules: vec![PackageModuleSource {
            subpath: "index.js".to_string(),
            text: module_src.to_string(),
        }],
    }
}

/// A provider factory serving a fixed package set from memory. Rebuilt per isolate (the
/// engine forks a provider per sandboxed isolate); each isolate compiles the same source
/// into its own heap, and `closure_permissions()` unions the held packages' manifests.
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

/// Escape a filesystem path for embedding in a double-quoted JS string literal (Windows
/// backslashes would otherwise read as escapes).
fn js_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

/// Extract the absolute data dir a package echoed via `echo("DATADIR:" + getDataDir())`. Lets a
/// test locate the package's `$DATA` dir (whose name carries a hash) without recomputing the slug.
fn data_dir_from(lines: &[String]) -> PathBuf {
    lines
        .iter()
        .find_map(|l| l.strip_prefix("DATADIR:"))
        .map(PathBuf::from)
        .expect("package must echo its DATADIR")
}

// ---------------------------------------------------------------------------
// Session harness
// ---------------------------------------------------------------------------

/// Set the (process-global, first-setter-wins) smudgy home and create `<home>/<server>/`
/// with `modules/` + `logs/`. Returns the per-server data dir (`$DATA`'s expansion). Safe
/// to call per test: a later call's temp dir is unused, but its unique `server` name keeps
/// the data dirs disjoint, and the returned path always reflects the *effective* home.
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

/// Install each `install_specifiers` entry untrusted (→ its own sandboxed isolate), recording
/// **full** consent (the whole closure union the provider reports), then drain the session.
///
/// Enforcement sources the **consented** union, not the live manifest union, so an un-consented
/// package is denied everything (`PACKAGE-ISOLATES-CONSENT-TRUST.md`). Recording the whole closure
/// union here grants each package exactly the authority its manifest declares, so these tests can
/// assert the manifest union is enforced. Each test here
/// installs a single top-level package, so the provider's `closure_permissions()` is precisely
/// that install's closure union. Call `prepare_server` first so the data dir exists before spawn.
async fn collect_session_lines(
    session_id: u32,
    server: &str,
    install_specifiers: &[&str],
    factory: PackageProviderFactory,
) -> Vec<String> {
    for spec in install_specifiers {
        shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
        // `echo` is itself a gated smudgy op, so record the manifest's deno union (what these tests
        // assert) PLUS the full smudgy capability set — letting the package echo its results without
        // changing the deno enforcement under test.
        let mut consent = factory().closure_permissions();
        consent.smudgy = SmudgyCapabilities::all();
        shared_packages::record_consent(server, spec, &consent).unwrap();
    }
    spawn_and_drain(session_id, server, factory).await
}

/// Like [`collect_session_lines`] but records an **explicit** consent union per install (`Some`),
/// or **no** consent record (`None`), instead of the full closure union — for the tests
/// that prove enforcement sources CONSENT: a partial consent withholds the manifest's other asks,
/// and a `None` record (a lock entry with no consent) denies everything.
async fn collect_session_lines_with_consent(
    session_id: u32,
    server: &str,
    installs: &[(&str, Option<PackagePermissions>)],
    factory: PackageProviderFactory,
) -> Vec<String> {
    for (spec, consent) in installs {
        shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
        if let Some(union) = consent {
            // `echo` is gated; grant it as the reporting channel so the package can echo its
            // outcome, while the test still controls every OTHER capability (deno + smudgy) via
            // `union`. A `None` consent stays fully deny-all (echo included), so a test of that case
            // must observe the denial out-of-band — the engine's load-failure notice — since the
            // package can no longer echo.
            let mut union = union.clone();
            union.smudgy.echo = true;
            shared_packages::record_consent(server, spec, &union).unwrap();
        }
    }
    spawn_and_drain(session_id, server, factory).await
}

/// Spawn a headless session resolving `smudgy://` from `factory` and collect every appended line
/// until the session goes quiet. No input is sent — the permission probes run at package load —
/// so this just drains the buffer. Callers install + record consent first.
async fn spawn_and_drain(
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
    // Collect from the first event: the probes echo during construction (package load),
    // before `RuntimeReady`, so the wait loop must capture buffer updates too.
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

    // Drain any remaining lines until the session is idle for a quiet period.
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();
    lines
}

fn has_line(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// net allow: a sandboxed package granted `net:["127.0.0.1:<allowed>"]` fetches
/// that host (→ 200), while the same host on an un-granted port is rejected at the gate
/// (`NotCapable` / "net access"), not connection-refused. Proves the manifest's `net` grant
/// reaches the isolate's container and is enforced host:port-exactly.
#[tokio::test]
async fn sandboxed_package_net_grant_is_enforced() {
    let (allowed_port, _server) = spawn_http_200_server();
    let denied_port = unused_port();
    prepare_server("pi_enf_net_allow");

    let src = r#"
        import { echo } from "smudgy:core";
        try {
          const res = await fetch("http://127.0.0.1:__ALLOWED__/");
          echo("NET_ALLOWED:" + res.status + ":" + (await res.text()));
        } catch (e) { echo("NET_ALLOWED_ERR:" + (e?.name ?? String(e))); }
        try {
          await fetch("http://127.0.0.1:__DENIED__/");
          echo("NET_OTHER:NO_ERROR");
        } catch (e) {
          echo("NET_OTHER_ERR:" + (e?.name ?? String(e)) + ":" + (e?.message ?? ""));
        }
        echo("DONE");
    "#
    .replace("__ALLOWED__", &allowed_port.to_string())
    .replace("__DENIED__", &denied_port.to_string());

    let pkg = make_package(
        "wbk",
        "fetcher",
        "1.0.0",
        &format!(r#", "permissions": {{ "net": ["127.0.0.1:{allowed_port}"] }}"#),
        &src,
    );
    let lines = collect_session_lines(
        9401,
        "pi_enf_net_allow",
        &["smudgy://wbk/fetcher"],
        factory_for(vec![pkg]),
    )
    .await;

    assert!(
        has_line(&lines, "NET_ALLOWED:200:ok"),
        "the granted host:port fetch must succeed with 200; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "NET_OTHER:NO_ERROR"),
        "the un-granted port must NOT be reachable; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "NET_OTHER_ERR:NotCapable")
            && has_line(&lines, "net access"),
        "the un-granted port must be a permission error (NotCapable / net access), not a \
         connection error; transcript:\n{lines:#?}"
    );
}

/// net deny by default: a sandboxed package declaring **no** `permissions` has an
/// empty closure union, so its container denies all net; any `fetch` rejects `NotCapable`.
/// This is the regression guard for the empty-allowlist recipe: the union maps an empty
/// `net` to `None` (deny), not `Some(vec![])` (which `deno_permissions` reads as allow-all).
#[tokio::test]
async fn sandboxed_package_with_no_permissions_denies_net() {
    let (port, _server) = spawn_http_200_server();
    prepare_server("pi_enf_net_deny");

    let src = r#"
        import { echo } from "smudgy:core";
        try {
          const res = await fetch("http://127.0.0.1:__PORT__/");
          echo("NET:NO_ERROR:" + res.status);
        } catch (e) { echo("NET_ERR:" + (e?.name ?? String(e))); }
        echo("DONE");
    "#
    .replace("__PORT__", &port.to_string());

    // No `permissions` block at all → empty union → deny-all.
    let pkg = make_package("wbk", "quiet", "1.0.0", "", &src);
    let lines = collect_session_lines(
        9402,
        "pi_enf_net_deny",
        &["smudgy://wbk/quiet"],
        factory_for(vec![pkg]),
    )
    .await;

    assert!(
        has_line(&lines, "DONE"),
        "the package must finish evaluating (the denial is caught, not thrown); transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "NET:NO_ERROR"),
        "a zero-permission package must NOT reach the network (empty allowlist must deny, not \
         allow-all); transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "NET_ERR:NotCapable"),
        "the fetch must reject with a permission error; transcript:\n{lines:#?}"
    );
}

/// import policy — None (the default): a sandboxed package with no `import` permission may not
/// download code from npm, jsr, OR the web. Every probe is rejected at the loader's gate — before
/// any registry/host fetch — so this is hermetic. (smudgy:// imports are governed separately and are
/// unaffected; covered by the `module_loader` unit tests.)
#[tokio::test]
async fn import_none_blocks_npm_jsr_and_web() {
    let (port, _server) = spawn_js_module_server("export const value = 42;");
    prepare_server("pi_enf_import_none");

    let src = r#"
        import { echo } from "smudgy:core";
        try { await import("npm:left-pad"); echo("NPM:NO_ERROR"); }
        catch (e) { echo("NPM_ERR:" + (e?.message ?? "")); }
        try { await import("jsr:@std/assert"); echo("JSR:NO_ERROR"); }
        catch (e) { echo("JSR_ERR:" + (e?.message ?? "")); }
        try { await import("http://127.0.0.1:__PORT__/mod.js"); echo("WEB:NO_ERROR"); }
        catch (e) { echo("WEB_ERR:" + (e?.message ?? "")); }
        echo("DONE");
    "#
    .replace("__PORT__", &port.to_string());

    // No `permissions` block ⇒ import defaults to None.
    let pkg = make_package("wbk", "iso", "1.0.0", "", &src);
    let lines =
        collect_session_lines(9410, "pi_enf_import_none", &["smudgy://wbk/iso"], factory_for(vec![pkg])).await;

    assert!(has_line(&lines, "DONE"), "the package finishes (denials are caught); transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, "NPM:NO_ERROR")
            && !has_line(&lines, "JSR:NO_ERROR")
            && !has_line(&lines, "WEB:NO_ERROR"),
        "None must block every external import; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "NPM_ERR") && has_line(&lines, "JSR_ERR") && has_line(&lines, "WEB_ERR"),
        "each external import must be rejected; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "blocked by this package's permissions"),
        "the rejection must be the import-policy gate (not a network error); transcript:\n{lines:#?}"
    );
}

/// import policy — Registries: npm + jsr are allowed, but an arbitrary web host is NOT. We probe the
/// arbitrary-web denial (a local server on `127.0.0.1`, which is not the `jsr.io` CDN) hermetically;
/// the npm/jsr *allow* direction would hit the real registries, so it's unit-tested instead
/// (`module_loader` / `package_resolver`). Proves Registries is strictly narrower than Any.
#[tokio::test]
async fn import_registries_blocks_arbitrary_web() {
    let (port, _server) = spawn_js_module_server("export const value = 42;");
    prepare_server("pi_enf_import_reg");

    let src = r#"
        import { echo } from "smudgy:core";
        try { await import("http://127.0.0.1:__PORT__/mod.js"); echo("WEB:NO_ERROR"); }
        catch (e) { echo("WEB_ERR:" + (e?.message ?? "")); }
        echo("DONE");
    "#
    .replace("__PORT__", &port.to_string());

    let pkg =
        make_package("wbk", "iso", "1.0.0", r#", "permissions": { "import": "registries" }"#, &src);
    let lines =
        collect_session_lines(9411, "pi_enf_import_reg", &["smudgy://wbk/iso"], factory_for(vec![pkg])).await;

    assert!(
        !has_line(&lines, "WEB:NO_ERROR"),
        "Registries must block an arbitrary web host; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "WEB_ERR") && has_line(&lines, "blocked by this package's permissions"),
        "the arbitrary-web import must be blocked at the gate; transcript:\n{lines:#?}"
    );
}

/// import policy — Any: arbitrary web imports load and run. A local ES-module server stands in for
/// "any host"; the sandboxed package imports it and runs its export. End-to-end proof that Any lifts
/// the web gate that None and Registries hold.
#[tokio::test]
async fn import_any_allows_arbitrary_web() {
    let (port, _server) = spawn_js_module_server("export const value = 42;");
    prepare_server("pi_enf_import_any");

    let src = r#"
        import { echo } from "smudgy:core";
        try { const m = await import("http://127.0.0.1:__PORT__/mod.js"); echo("WEB_OK:" + m.value); }
        catch (e) { echo("WEB_ERR:" + (e?.name ?? String(e)) + ":" + (e?.message ?? "")); }
        echo("DONE");
    "#
    .replace("__PORT__", &port.to_string());

    let pkg = make_package("wbk", "iso", "1.0.0", r#", "permissions": { "import": "any" }"#, &src);
    let lines =
        collect_session_lines(9412, "pi_enf_import_any", &["smudgy://wbk/iso"], factory_for(vec![pkg])).await;

    assert!(
        has_line(&lines, "WEB_OK:42"),
        "Any must allow an arbitrary web import to load and run; transcript:\n{lines:#?}"
    );
}

/// read/write subtree scoping via `getDataDir()`: a package granted `read`/`write` on
/// `$DATA/allowed` writes + reads back a file under that subtree, while a sibling *outside* it
/// (`$DATA/secret.txt`) is denied (`NotCapable` / "read access" / "write access"). Proves `$DATA`
/// host-expansion + the directory-grant-covers-subtree semantics end to end, and that
/// `getDataDir()` returns the dir the grant expands to. Nothing is pre-created on disk (the package
/// writes then reads back), so the test needn't know the hashed data-dir path in advance -- it
/// reads it from the echoed `DATADIR:` line.
#[tokio::test]
async fn sandboxed_package_read_write_is_scoped_to_subtree() {
    let server_dir = prepare_server("pi_enf_fs");

    let src = r#"
        import { echo, getDataDir } from "smudgy:core";
        const d = getDataDir();
        echo("DATADIR:" + d);
        try {
            await Deno.mkdir(d + "/allowed", { recursive: true });
            await Deno.writeTextFile(d + "/allowed/out.txt", "deep-ok");
            echo("WRITE_OK");
        } catch (e) { echo("WRITE_OK_ERR:" + (e?.name ?? String(e))); }
        try { echo("READ_OK:" + (await Deno.readTextFile(d + "/allowed/out.txt"))); }
        catch (e) { echo("READ_OK_ERR:" + (e?.name ?? String(e))); }
        try { await Deno.writeTextFile(d + "/secret.txt", "x"); echo("WRITE_ESCAPE:NO_ERROR"); }
        catch (e) { echo("WRITE_ESCAPE_ERR:" + (e?.name ?? String(e)) + ":" + (e?.message ?? "")); }
        try { await Deno.readTextFile(d + "/secret.txt"); echo("READ_SECRET:NO_ERROR"); }
        catch (e) { echo("READ_SECRET_ERR:" + (e?.name ?? String(e)) + ":" + (e?.message ?? "")); }
        echo("DONE");
    "#;

    let pkg = make_package(
        "wbk",
        "fsuser",
        "1.0.0",
        r#", "permissions": { "read": ["$DATA/allowed"], "write": ["$DATA/allowed"] }"#,
        src,
    );
    let lines = collect_session_lines(
        9403,
        "pi_enf_fs",
        &["smudgy://wbk/fsuser"],
        factory_for(vec![pkg]),
    )
    .await;

    let data_dir = data_dir_from(&lines);
    // `getDataDir()` is the package's own `.isolate-storage/<slug>/data`, not the server root.
    assert!(
        data_dir.starts_with(server_dir.join(".isolate-storage")),
        "getDataDir() must be under .isolate-storage, got {}; transcript:\n{lines:#?}",
        data_dir.display()
    );

    // Subtree grant covers a nested file (write + read-back).
    assert!(
        has_line(&lines, "WRITE_OK"),
        "a file under the granted dir must be writable; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "READ_OK:deep-ok"),
        "a file under the granted dir must be readable; transcript:\n{lines:#?}"
    );
    assert!(
        data_dir.join("allowed").join("out.txt").exists(),
        "the granted write must actually have hit disk; transcript:\n{lines:#?}"
    );
    // Siblings outside the granted subtree are denied.
    assert!(
        !has_line(&lines, "READ_SECRET:NO_ERROR") && has_line(&lines, "READ_SECRET_ERR:NotCapable"),
        "a sibling outside the grant must be a read-permission error; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "read access"),
        "the denied read must name read access; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "WRITE_ESCAPE:NO_ERROR")
            && has_line(&lines, "WRITE_ESCAPE_ERR:NotCapable"),
        "a write outside the grant must be a write-permission error; transcript:\n{lines:#?}"
    );
    assert!(
        !data_dir.join("secret.txt").exists(),
        "the denied write must NOT have hit disk; transcript:\n{lines:#?}"
    );
}

/// `getDataDir()` + `node:fs`: isolates share one thread (so there is no per-isolate cwd), so a
/// package builds ABSOLUTE paths from `getDataDir()` and uses `node:fs` `writeFileSync`/
/// `readFileSync`. With `read`/`write: ["$DATA"]` the file round-trips and lands in the package's
/// own data dir; the same code WITHOUT the grant is denied and writes nothing.
#[tokio::test]
async fn sandboxed_package_data_dir_fs_via_getdatadir() {
    let server_dir = prepare_server("pi_enf_getdatadir");

    let src = r#"
        import { echo, getDataDir } from "smudgy:core";
        import { writeFileSync, readFileSync } from "node:fs";
        const d = getDataDir();
        echo("DATADIR:" + d);
        try { writeFileSync(d + "/test.txt", "hi-from-fs"); echo("WRITE_OK"); }
        catch (e) { echo("WRITE_ERR:" + (e?.name ?? String(e)) + ":" + (e?.code ?? "")); }
        try { echo("READ_OK:" + readFileSync(d + "/test.txt", "utf8")); }
        catch (e) { echo("READ_ERR:" + (e?.name ?? String(e)) + ":" + (e?.code ?? "")); }
        echo("DONE");
    "#;

    // --- Granted: read/write on the whole data dir ($DATA) ---
    let granted = make_package(
        "wbk",
        "fsdata",
        "1.0.0",
        r#", "permissions": { "read": ["$DATA"], "write": ["$DATA"] }"#,
        src,
    );
    let lines = collect_session_lines(
        9411,
        "pi_enf_getdatadir",
        &["smudgy://wbk/fsdata"],
        factory_for(vec![granted]),
    )
    .await;

    let data_dir = data_dir_from(&lines);
    assert!(
        data_dir.starts_with(server_dir.join(".isolate-storage")) && data_dir.ends_with("data"),
        "getDataDir() must be the package's .isolate-storage/<slug>/data dir, got {}; transcript:\n{lines:#?}",
        data_dir.display()
    );
    assert!(
        has_line(&lines, "WRITE_OK") && has_line(&lines, "READ_OK:hi-from-fs"),
        "node:fs write+read via getDataDir() must round-trip with the $DATA grant; transcript:\n{lines:#?}"
    );
    let file = data_dir.join("test.txt");
    assert!(
        file.exists() && std::fs::read_to_string(&file).unwrap() == "hi-from-fs",
        "the file must land in the package data dir with the written content; checked {}; transcript:\n{lines:#?}",
        file.display()
    );

    // --- Denied: same code, NO read/write grant ---
    let denied = make_package("wbk", "fsdata_denied", "1.0.0", "", src);
    let denied_lines = collect_session_lines(
        9412,
        "pi_enf_getdatadir",
        &["smudgy://wbk/fsdata_denied"],
        factory_for(vec![denied]),
    )
    .await;

    let denied_dir = data_dir_from(&denied_lines);
    assert!(
        !has_line(&denied_lines, "WRITE_OK") && has_line(&denied_lines, "WRITE_ERR:"),
        "without the write grant, the node:fs write must be denied; transcript:\n{denied_lines:#?}"
    );
    assert!(
        !denied_dir.join("test.txt").exists(),
        "the denied write must NOT have hit disk; transcript:\n{denied_lines:#?}"
    );
}

/// closure union: root `R` declares **no** permissions; its `smudgy://` dependency
/// `D` declares `net:[127.0.0.1:<port>]`. `R` imports `D` (a declared dep), so `D` runs in
/// `R`'s isolate and its `net` grant is unioned into `R`'s container — `R`'s own `fetch`
/// reaches the host even though `R` asked for nothing. Proves the enforced authority is the
/// **closure union, not just the root's asks**.
///
/// Scope: with the in-memory provider the union is computed over the packages it holds (here
/// exactly `R`'s closure), so this exercises the factory → `closure_permissions()` → container
/// wiring and the union *semantics* — not the cloud `solve_closure` *walk* over the dep graph.
/// That walk's per-package fold reuses `PackagePermissions::merge` (unit-tested in
/// `package_resolver`) over the same DFS the resolution/param tests already cover.
#[tokio::test]
async fn closure_union_grants_a_dependency_declared_permission() {
    let (port, _server) = spawn_http_200_server();
    prepare_server("pi_enf_union");

    // The dependency D declares the net grant (and is otherwise inert).
    let dep = make_package(
        "wbk",
        "neturl",
        "1.0.0",
        &format!(r#", "permissions": {{ "net": ["127.0.0.1:{port}"] }}"#),
        "export const ready = true;",
    );
    // The root R declares NOTHING, but imports D (a declared dependency) and fetches the host.
    let root_src = r#"
        import { echo } from "smudgy:core";
        import "smudgy://wbk/neturl";
        try {
          const res = await fetch("http://127.0.0.1:__PORT__/");
          echo("UNION_NET:" + res.status + ":" + (await res.text()));
        } catch (e) { echo("UNION_NET_ERR:" + (e?.name ?? String(e))); }
        echo("DONE");
    "#
    .replace("__PORT__", &port.to_string());
    let root = make_package(
        "wbk",
        "app",
        "1.0.0",
        r#", "dependencies": ["smudgy://wbk/neturl"]"#,
        &root_src,
    );

    // Only R is installed (top-level → sandbox); D is a transitive dep the provider holds.
    let lines = collect_session_lines(
        9404,
        "pi_enf_union",
        &["smudgy://wbk/app"],
        factory_for(vec![root, dep]),
    )
    .await;

    assert!(
        has_line(&lines, "UNION_NET:200:ok"),
        "the root's fetch must succeed because its dependency's net grant joined the closure \
         union; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "UNION_NET_ERR"),
        "the closure union must have granted net to the root isolate; transcript:\n{lines:#?}"
    );
}

/// main isolate unaffected: a local module in the main (trusted) isolate keeps full
/// net + fs (allow-all). It fetches the local server and reads a file with no grant at all —
/// both succeed, guarding against over-restricting main when sandboxing the packages.
#[tokio::test]
async fn main_isolate_keeps_full_authority() {
    let (port, _server) = spawn_http_200_server();
    let server_dir = prepare_server("pi_enf_main");
    // A file the main module reads — there is no permission grant anywhere; main is allow-all.
    let any_file = server_dir.join("anywhere.txt");
    std::fs::write(&any_file, "main-ok").unwrap();

    let module_src = r#"
        import { echo } from "smudgy:core";
        try {
          const res = await fetch("http://127.0.0.1:__PORT__/");
          echo("MAIN_NET:" + res.status + ":" + (await res.text()));
        } catch (e) { echo("MAIN_NET_ERR:" + (e?.name ?? String(e))); }
        try { echo("MAIN_READ:" + (await Deno.readTextFile("__FILE__"))); }
        catch (e) { echo("MAIN_READ_ERR:" + (e?.name ?? String(e))); }
        echo("DONE");
    "#
    .replace("__PORT__", &port.to_string())
    .replace("__FILE__", &js_path(&any_file));
    std::fs::write(server_dir.join("modules").join("main_mod.ts"), &module_src).unwrap();

    // No packages, no installs — only the local module in main.
    let lines = collect_session_lines(9405, "pi_enf_main", &[], factory_for(vec![])).await;

    assert!(
        has_line(&lines, "MAIN_NET:200:ok"),
        "the main isolate must keep full net (allow-all); transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "MAIN_READ:main-ok"),
        "the main isolate must keep full fs read (allow-all); transcript:\n{lines:#?}"
    );
}

/// `$DATA` containment: a `read` grant using `$DATA/..` must NOT let a
/// package escape its data dir. The `..`-bearing entries are dropped before reaching
/// `deno_permissions` (`PACKAGE-ISOLATES-ENFORCEMENT.md`), so a file in the parent of the
/// per-server dir stays denied. Guards the directory-traversal vector (a manifest could
/// otherwise declare `$DATA/../../etc` and silently read anywhere).
#[tokio::test]
async fn sandboxed_package_data_grant_cannot_escape_via_dotdot() {
    let server_dir = prepare_server("pi_enf_escape");
    // A file OUTSIDE the per-server data dir (in its parent — the shared smudgy home).
    let outside = server_dir
        .parent()
        .expect("server dir has a parent")
        .join("outside.txt");
    std::fs::write(&outside, "outside-secret").unwrap();

    let src = r#"
        import { echo } from "smudgy:core";
        try { await Deno.readTextFile("__OUTSIDE__"); echo("ESCAPE:NO_ERROR"); }
        catch (e) { echo("ESCAPE_ERR:" + (e?.name ?? String(e))); }
        echo("DONE");
    "#
    .replace("__OUTSIDE__", &js_path(&outside));

    // Both grants try to climb above `$DATA` with `..` — both must be dropped, leaving deny.
    let pkg = make_package(
        "wbk",
        "escaper",
        "1.0.0",
        r#", "permissions": { "read": ["$DATA/..", "$DATA/../outside.txt"] }"#,
        &src,
    );
    let lines = collect_session_lines(
        9406,
        "pi_enf_escape",
        &["smudgy://wbk/escaper"],
        factory_for(vec![pkg]),
    )
    .await;

    assert!(
        !has_line(&lines, "ESCAPE:NO_ERROR"),
        "a `$DATA/..` grant must not allow escaping the data dir; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "ESCAPE_ERR:NotCapable"),
        "the escaping read must be denied (the `$DATA/..` entries are dropped); transcript:\n{lines:#?}"
    );
}

/// enforcement sources the **consented** union, and withholds the rest of
/// the manifest's asks. The package's manifest declares `net` for TWO live hosts, but the user
/// consented to only the first. The consented host is reachable (→ 200); the *withheld* host —
/// though its manifest grant exists and a live server is listening — is rejected at the
/// permission gate (`NotCapable` / "net access"), proving the isolate's container is built from
/// the stored consent, NOT the live manifest union (`PACKAGE-ISOLATES-CONSENT-TRUST.md`; the
/// withholding guarantee that keeps an un-accepted update escalation from taking effect).
#[tokio::test]
async fn consented_union_is_enforced_and_withholds_unconsented_asks() {
    let (consented_port, _server_a) = spawn_http_200_server();
    let (withheld_port, _server_b) = spawn_http_200_server();
    prepare_server("pi_consent_withhold");

    let src = r#"
        import { echo } from "smudgy:core";
        try {
          const res = await fetch("http://127.0.0.1:__CONSENTED__/");
          echo("CONSENTED_NET:" + res.status + ":" + (await res.text()));
        } catch (e) { echo("CONSENTED_NET_ERR:" + (e?.name ?? String(e))); }
        try {
          await fetch("http://127.0.0.1:__WITHHELD__/");
          echo("WITHHELD_NET:NO_ERROR");
        } catch (e) {
          echo("WITHHELD_NET_ERR:" + (e?.name ?? String(e)) + ":" + (e?.message ?? ""));
        }
        echo("DONE");
    "#
    .replace("__CONSENTED__", &consented_port.to_string())
    .replace("__WITHHELD__", &withheld_port.to_string());

    // The manifest asks for BOTH hosts...
    let pkg = make_package(
        "wbk",
        "withholder",
        "1.0.0",
        &format!(
            r#", "permissions": {{ "net": ["127.0.0.1:{consented_port}", "127.0.0.1:{withheld_port}"] }}"#
        ),
        &src,
    );
    // ...but the user consented to only the first.
    let consent = PackagePermissions {
        net: vec![format!("127.0.0.1:{consented_port}")],
        ..Default::default()
    };
    let lines = collect_session_lines_with_consent(
        9407,
        "pi_consent_withhold",
        &[("smudgy://wbk/withholder", Some(consent))],
        factory_for(vec![pkg]),
    )
    .await;

    assert!(
        has_line(&lines, "CONSENTED_NET:200:ok"),
        "the consented host:port must be reachable; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "WITHHELD_NET:NO_ERROR"),
        "the un-consented host must NOT be reachable even though the manifest declared it and a \
         live server is listening; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "WITHHELD_NET_ERR:NotCapable") && has_line(&lines, "net access"),
        "the withheld host must be denied at the permission gate (NotCapable / net access), \
         proving consent — not the manifest — is the enforced source; transcript:\n{lines:#?}"
    );
}

/// a consent granting NO deno permissions denies
/// everything, even a permission the manifest explicitly declares. The package asks for `net` on a
/// live host but the consent grants none; the fetch is rejected at the gate (`NotCapable`). This is
/// the empty-union "deny" the "must consent" default produces — the *same* enforced empty
/// union a literally-`None` lock entry yields via the engine's `unwrap_or_default()`.
///
/// The consent records an explicit empty-deno grant whose only smudgy capability is the reporting
/// `echo` (the harness adds it), keeping the deno enforcement under test as empty net union ⇒ net
/// denied while letting the package echo the denial (a fully-`None` consent denies `echo` too).
#[tokio::test]
async fn unconsented_package_is_denied_everything() {
    let (port, _server) = spawn_http_200_server();
    prepare_server("pi_consent_none");

    let src = r#"
        import { echo } from "smudgy:core";
        try {
          const res = await fetch("http://127.0.0.1:__PORT__/");
          echo("NET:NO_ERROR:" + res.status);
        } catch (e) { echo("NET_ERR:" + (e?.name ?? String(e)) + ":" + (e?.message ?? "")); }
        echo("DONE");
    "#
    .replace("__PORT__", &port.to_string());

    // The manifest DECLARES the net grant, but the consent grants no deno permission → deny-all.
    let pkg = make_package(
        "wbk",
        "unconsented",
        "1.0.0",
        &format!(r#", "permissions": {{ "net": ["127.0.0.1:{port}"] }}"#),
        &src,
    );
    let lines = collect_session_lines_with_consent(
        9408,
        "pi_consent_none",
        &[("smudgy://wbk/unconsented", Some(PackagePermissions::default()))],
        factory_for(vec![pkg]),
    )
    .await;

    assert!(
        has_line(&lines, "DONE"),
        "the package must finish evaluating (the denial is caught, not thrown); transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "NET:NO_ERROR"),
        "a no-deno-permission consent must NOT reach the network even though its manifest declared \
         the grant; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "NET_ERR:NotCapable") && has_line(&lines, "net access"),
        "the fetch must reject with a permission error (empty union ⇒ deny); transcript:\n{lines:#?}"
    );
}

/// A sandboxed package that did NOT request the interop capability is denied handle
/// `.emit`/consumer `.on` at runtime (`PACKAGE-EVENTS.md`): the gate throws `NotCapable`
/// naming the missing capability (`interop:write`/`interop:read` — the caps the legacy
/// `events` manifest tokens alias onto), caught and reported. Consent is echo-only (so it
/// can report), no interop.
#[tokio::test]
async fn sandboxed_package_without_events_capability_is_denied_emit_and_subscribe() {
    prepare_server("pi_events_denied");

    let src = r#"
        import { createEvent, events, echo } from "smudgy:core";
        const x = createEvent("x");
        try { x.emit({ a: 1 }); echo("EMIT_OK"); } catch (e) { echo("EMIT_DENIED:" + (e?.message ?? String(e))); }
        try { events.lookup("user", "y").on(() => {}); echo("ON_OK"); } catch (e) { echo("ON_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    let pkg = make_package("wbk", "noevents", "1.0.0", "", src);
    let lines = collect_session_lines_with_consent(
        9421,
        "pi_events_denied",
        &[("smudgy://wbk/noevents", Some(PackagePermissions::default()))],
        factory_for(vec![pkg]),
    )
    .await;

    assert!(has_line(&lines, "DONE"), "the package must finish evaluating; transcript:\n{lines:#?}");
    assert!(
        !has_line(&lines, "EMIT_OK") && !has_line(&lines, "ON_OK"),
        "a package without the events capability must not emit or subscribe; transcript:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("EMIT_DENIED") && l.contains("interop:write")),
        "emit must throw NotCapable('interop:write'); transcript:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("ON_DENIED") && l.contains("interop:read")),
        "on must throw NotCapable('interop:read'); transcript:\n{lines:#?}"
    );
}

/// Cross-isolate delivery through the consumer scheme (`PACKAGE-EVENTS.md` +
/// interop.md §4): two packages each get their OWN sandboxed isolate. The emitter
/// declares an event handle and emits (deferred via `setTimeout` so the listener is already
/// subscribed) on its owner-stamped namespace; the listener — in a DIFFERENT isolate —
/// imports the consumer handle from `smudgy:events/o/emitter` (a declared dependency, JS
/// package, so name discovery reads the literal name argument) and receives it, proving the
/// scheme synthesizes without evaluating the producer and the host routes delivery across
/// the isolate boundary.
#[tokio::test]
async fn events_deliver_across_sandboxed_isolates() {
    prepare_server("pi_events_cross");

    let emitter = make_package(
        "o",
        "emitter",
        "1.0.0",
        "",
        r#"import { createEvent } from "smudgy:core"; const evt = createEvent("evt"); setTimeout(() => evt.emit({ n: 5 }), 0);"#,
    );
    let listener = make_package(
        "o",
        "listener",
        "1.0.0",
        r#", "dependencies": ["smudgy://o/emitter"]"#,
        r#"import { echo } from "smudgy:core";
           import { evt } from "smudgy:events/o/emitter";
           evt.on((p) => echo("CROSS_GOT:" + p.n));"#,
    );
    let lines = collect_session_lines(
        9422,
        "pi_events_cross",
        &["smudgy://o/emitter", "smudgy://o/listener"],
        factory_for(vec![emitter, listener]),
    )
    .await;

    assert!(
        has_line(&lines, "CROSS_GOT:5"),
        "an emit in one sandboxed isolate must deliver to a smudgy:events/… subscriber in another; transcript:\n{lines:#?}"
    );
}
