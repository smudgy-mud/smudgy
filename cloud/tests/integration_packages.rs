//! End-to-end test of [`PackageApiClient`] against a self-contained, contract-shaped
//! mock of the `/packages` routes. Exercises the full client wire path:
//! create namespace → publish a version → resolve → fetch a module body (with the
//! client's SHA-256 integrity check), plus the batched `check-updates` sweep (and its
//! absence on an old server).
//!
//! This is a focused, standalone mock (its own tiny Axum app) so it doesn't touch the
//! shared `tests/support` `MockState`. The canonical mock for the broader suite still
//! belongs in `tests/support/` (the contract-mirror discipline); this validates the
//! client half of the contract end-to-end.
#![allow(clippy::cast_possible_wrap, clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use smudgy_cloud::{
    CheckUpdatesEntry, CheckUpdatesHave, CloudError, Credential, CredentialSource,
    PackageApiClient, PublishDependency, PublishModule, UpdateCheckInstalled,
    highest_satisfying_version,
};

// --- mock state ------------------------------------------------------------

struct MockModule {
    subpath: String,
    content_hash: String,
    media_type: String,
    byte_size: i64,
    is_entry: bool,
}

struct MockVersion {
    version: String,
    manifest: Value,
    modules: Vec<MockModule>,
    /// The `dependencies` array sent at publish (the locked dep set), captured verbatim
    /// so tests can assert the publish wire carries the resolved versions.
    dependencies: Value,
    yanked: bool,
}

struct MockPackage {
    id: Uuid,
    owner_id: Uuid,
    name: String,
    description: String,
    is_public: bool,
    versions: Vec<MockVersion>,
    /// Numbers published then hard-deleted: permanently reserved, never reusable.
    retired: Vec<String>,
}

#[derive(Default)]
struct MockState {
    base_url: String,
    owner_nickname: String,
    packages: Vec<MockPackage>,
    blobs: HashMap<String, Vec<u8>>, // content_hash -> body (any bytes)
    /// When set, a `latest` whose walked closure exceeds this many distinct nodes
    /// answers with `closure: []` while status/installed/latest stay intact — the
    /// server's over-cap contract (no 400; only the request caps 400). Tests set it
    /// to exercise the client's cannot-evaluate handling of an elided closure.
    closure_node_cap: Option<usize>,
    /// Most recent package-protocol capability value observed on resolve.
    package_compatibility: Option<String>,
}

type Shared = Arc<Mutex<MockState>>;

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

fn envelope(status: u16, data: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({ "success": true, "data": data, "error": null })),
    )
        .into_response()
}

// --- mock handlers (mirror smudgy-api/src/packages) ------------------------

async fn create_package(State(state): State<Shared>, body: String) -> Response {
    let req: Value = serde_json::from_str(&body).unwrap();
    let name = req["name"].as_str().unwrap().to_string();
    let mut st = state.lock().unwrap();
    let owner_id = Uuid::new_v4();
    // create-or-get
    if let Some(existing) = st.packages.iter().find(|p| p.name == name) {
        return envelope(201, package_view(existing));
    }
    let pkg = MockPackage {
        id: Uuid::new_v4(),
        owner_id,
        name,
        description: req["description"].as_str().unwrap_or("").to_string(),
        is_public: false,
        versions: Vec::new(),
        retired: Vec::new(),
    };
    let view = package_view(&pkg);
    st.packages.push(pkg);
    envelope(201, view)
}

fn mock_bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "success": false, "data": null, "error": msg })),
    )
        .into_response()
}

fn mock_version_unavailable(version: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({ "success": false, "data": null, "error": format!("version_unavailable: {version}") })),
    )
        .into_response()
}

fn mock_too_large(msg: &str) -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({ "success": false, "data": null, "error": msg })),
    )
        .into_response()
}

/// Mirror the server's begin/finalize validation: size caps + duplicate-subpath. Returns an
/// error response to short-circuit, or `None` if valid. Keeps the mock a faithful fidelity
/// reference for the client's cap behavior.
fn mock_validate(modules: &[Value], manifest: &Value) -> Option<Response> {
    if modules.len() > 128 {
        return Some(mock_bad_request("too many modules"));
    }
    if serde_json::to_vec(manifest).map_or(usize::MAX, |v| v.len()) > 256 * 1024 {
        return Some(mock_too_large("manifest too large"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut total: i64 = 0;
    for m in modules {
        if !seen.insert(m["subpath"].as_str().unwrap_or_default()) {
            return Some(mock_bad_request("duplicate module subpath"));
        }
        let bs = m["byte_size"].as_i64().unwrap_or(0);
        if bs < 0 {
            return Some(mock_bad_request("negative byte_size"));
        }
        if bs > 10 * 1024 * 1024 {
            return Some(mock_too_large("module too large"));
        }
        total += bs;
    }
    if total > 100 * 1024 * 1024 {
        return Some(mock_too_large("version too large"));
    }
    None
}

/// `…/versions/begin` — validate (mirroring the server) and return a presigned PUT per body
/// not already stored (content-addressed dedup). Writes nothing.
async fn begin_version(
    State(state): State<Shared>,
    Path(id): Path<Uuid>,
    body: String,
) -> Response {
    let Ok(req) = serde_json::from_str::<Value>(&body) else {
        return mock_bad_request("invalid JSON body");
    };
    let Some(version) = req["version"].as_str().map(str::to_string) else {
        return mock_bad_request("missing version");
    };
    let modules = req["modules"].as_array().cloned().unwrap_or_default();
    if let Some(err) = mock_validate(&modules, &req["manifest"]) {
        return err;
    }

    let st = state.lock().unwrap();
    let Some(pkg) = st.packages.iter().position(|p| p.id == id) else {
        return envelope(404, Value::Null);
    };
    // Build metadata is precedence-noise and never stored — reject it (mirrors the server).
    if version.contains('+') {
        return mock_bad_request("build metadata not allowed");
    }
    // Fast duplicate/retired pre-check (the authoritative re-check is in finalize). A number
    // is permanently reserved once published: reject a live duplicate OR a retired number.
    let taken = st.packages[pkg]
        .versions
        .iter()
        .any(|v| v.version == version)
        || st.packages[pkg].retired.contains(&version);
    if taken {
        return mock_version_unavailable(&version);
    }
    // Presign a PUT only for each distinct body not already stored (content-addressed dedup).
    let mut uploads = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for m in &modules {
        let hash = m["content_hash"].as_str().unwrap_or_default().to_string();
        if !seen.insert(hash.clone()) || st.blobs.contains_key(&hash) {
            continue;
        }
        uploads.push(json!({
            "content_hash": hash,
            "url": format!("{}/packages/upload/{}", st.base_url, hash),
            "headers": { "x-amz-checksum-sha256": hash },
        }));
    }
    envelope(200, json!({ "uploads": uploads }))
}

/// `PUT /packages/upload/{hash}` — the mock's stand-in for the presigned S3 PUT. Mirrors S3's
/// `x-amz-checksum-sha256` verification: store the body ONLY if it hashes to the declared hash
/// (from the URL), else reject — so a forged body can't land under an honest hash.
async fn upload_blob(
    State(state): State<Shared>,
    Path(hash): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Package capability negotiation belongs on registry API calls, never on presigned or
    // package-chosen content URLs.
    if headers.contains_key("x-smudgy-package-compatibility") {
        return (StatusCode::BAD_REQUEST, "package header leaked to blob URL").into_response();
    }
    let actual = sha256_hex(&body);
    // Mirror S3's signed-checksum binding: the client MUST replay the `x-amz-checksum-sha256`
    // header from begin (the mock uses the hex hash as its value), and the body must match it
    // AND the URL key. This makes every upload test assert the client echoes the header.
    let header_ok = headers
        .get("x-amz-checksum-sha256")
        .and_then(|v| v.to_str().ok())
        == Some(actual.as_str());
    if actual != hash || !header_ok {
        return (StatusCode::BAD_REQUEST, "checksum mismatch").into_response();
    }
    state.lock().unwrap().blobs.insert(hash, body.to_vec());
    StatusCode::OK.into_response()
}

/// `…/versions/finalize` — `HeadObject` each declared blob (present + matching size), then
/// commit the version. The reservation/duplicate guard runs here too (authoritative).
async fn finalize_version(
    State(state): State<Shared>,
    Path(id): Path<Uuid>,
    body: String,
) -> Response {
    let Ok(req) = serde_json::from_str::<Value>(&body) else {
        return mock_bad_request("invalid JSON body");
    };
    let Some(version) = req["version"].as_str().map(str::to_string) else {
        return mock_bad_request("missing version");
    };
    let manifest = req["manifest"].clone();
    let dependencies = req["dependencies"].clone();
    let modules_in = req["modules"].as_array().cloned().unwrap_or_default();
    if let Some(err) = mock_validate(&modules_in, &manifest) {
        return err;
    }

    let mut st = state.lock().unwrap();
    let Some(pkg) = st.packages.iter().position(|p| p.id == id) else {
        return envelope(404, Value::Null);
    };
    if version.contains('+') {
        return mock_bad_request("build metadata not allowed");
    }
    let taken = st.packages[pkg]
        .versions
        .iter()
        .any(|v| v.version == version)
        || st.packages[pkg].retired.contains(&version);
    if taken {
        return mock_version_unavailable(&version);
    }
    // HeadObject each declared blob: it must be present with exactly the declared size.
    let mut modules = Vec::new();
    let mut module_meta = Vec::new();
    for m in &modules_in {
        let subpath = m["subpath"].as_str().unwrap_or_default().to_string();
        let hash = m["content_hash"].as_str().unwrap_or_default().to_string();
        let byte_size = m["byte_size"].as_i64().unwrap_or(0);
        let media_type = m["media_type"].as_str().unwrap_or("text/plain").to_string();
        let is_entry = m["is_entry"].as_bool().unwrap_or(false);
        match st.blobs.get(&hash) {
            Some(bytes) if bytes.len() as i64 == byte_size => {}
            Some(_) => return mock_bad_request("module size mismatch"),
            None => return mock_bad_request("module blob not uploaded"),
        }
        module_meta.push(json!({
            "subpath": subpath, "content_hash": hash, "media_type": media_type,
            "byte_size": byte_size, "is_entry": is_entry,
        }));
        modules.push(MockModule {
            subpath,
            content_hash: hash,
            media_type,
            byte_size,
            is_entry,
        });
    }
    let version_id = Uuid::new_v4();
    st.packages[pkg].versions.push(MockVersion {
        version: version.clone(),
        manifest: manifest.clone(),
        modules,
        dependencies,
        yanked: false,
    });
    envelope(
        201,
        json!({
            "id": version_id, "package_id": id, "version": version,
            "manifest": manifest, "modules": module_meta,
            "published_at": "2026-06-20T00:00:00Z",
        }),
    )
}

/// Live (newest-first) + retired entries, mirroring the real `list_versions`: yanked
/// versions carry `yanked: true`; hard-deleted numbers carry `deleted: true`.
fn version_list_json(pkg: &MockPackage) -> Vec<Value> {
    // Combine live + retired and sort newest-first by true semver precedence, mirroring the
    // server's list_versions (which interleaves deleted numbers by version, NOT by insertion
    // order). Reversing insertion order + appending retired last would drift from the server.
    let mut combined: Vec<(String, bool, bool)> = pkg
        .versions
        .iter()
        .map(|v| (v.version.clone(), v.yanked, false))
        .chain(pkg.retired.iter().map(|v| (v.clone(), false, true)))
        .collect();
    combined.sort_by(
        |a, b| match (semver::Version::parse(&a.0), semver::Version::parse(&b.0)) {
            (Ok(va), Ok(vb)) => vb.cmp(&va),
            _ => b.0.cmp(&a.0),
        },
    );
    combined
        .into_iter()
        .map(|(version, yanked, deleted)| {
            json!({ "version": version, "yanked": yanked, "deleted": deleted, "published_at": "2026-06-20T00:00:00Z" })
        })
        .collect()
}

async fn list_versions(State(state): State<Shared>, Path(id): Path<Uuid>) -> Response {
    let st = state.lock().unwrap();
    let Some(pkg) = st.packages.iter().find(|p| p.id == id) else {
        return envelope(404, Value::Null);
    };
    envelope(200, json!(version_list_json(pkg)))
}

async fn set_version_yanked(
    State(state): State<Shared>,
    Path((id, version)): Path<(Uuid, String)>,
    body: String,
) -> Response {
    let req: Value = serde_json::from_str(&body).unwrap();
    let yanked = req["yanked"].as_bool().unwrap_or(false);
    let mut st = state.lock().unwrap();
    let Some(pkg) = st.packages.iter_mut().find(|p| p.id == id) else {
        return envelope(404, Value::Null);
    };
    let Some(v) = pkg.versions.iter_mut().find(|v| v.version == version) else {
        return envelope(404, Value::Null);
    };
    v.yanked = yanked;
    // Mirror patch_version: return the updated version list.
    envelope(200, json!(version_list_json(pkg)))
}

async fn delete_version(
    State(state): State<Shared>,
    Path((id, version)): Path<(Uuid, String)>,
) -> Response {
    let mut st = state.lock().unwrap();
    let Some(pkg) = st.packages.iter_mut().find(|p| p.id == id) else {
        return envelope(404, Value::Null);
    };
    let Some(idx) = pkg.versions.iter().position(|v| v.version == version) else {
        return envelope(404, Value::Null);
    };
    // Heavy, two-step: a version must be yanked before it can be deleted.
    if !pkg.versions[idx].yanked {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "data": null, "error": "version_not_yanked" })),
        )
            .into_response();
    }
    pkg.versions.remove(idx);
    pkg.retired.push(version); // number stays permanently reserved
    StatusCode::OK.into_response()
}

async fn resolve(
    State(state): State<Shared>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let name = params.get("name").cloned().unwrap_or_default();
    let range = params
        .get("version")
        .cloned()
        .unwrap_or_else(|| "latest".to_string());
    let mut st = state.lock().unwrap();
    st.package_compatibility = headers
        .get("x-smudgy-package-compatibility")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let Some(pkg) = st.packages.iter().find(|p| p.name == name) else {
        return envelope(404, Value::Null);
    };
    let version = if range == "latest" {
        pkg.versions.last()
    } else {
        pkg.versions.iter().find(|v| v.version == range)
    };
    let Some(version) = version else {
        return envelope(404, Value::Null);
    };
    let modules: Vec<Value> = version
        .modules
        .iter()
        .map(|m| {
            json!({
                "subpath": m.subpath, "content_hash": m.content_hash, "media_type": m.media_type,
                "byte_size": m.byte_size, "is_entry": m.is_entry,
                "content_url": format!("{}/packages/blob/{}", st.base_url, m.content_hash),
            })
        })
        .collect();
    // Mirror the server: surface the locked relations in resolve-shape. This mock's
    // publish endpoint accepts code dependencies only, so each recorded edge is a
    // `dependency`; dedicated wire tests cover `requires` deserialization.
    let dependencies: Vec<Value> = version
        .dependencies
        .as_array()
        .map(|deps| {
            deps.iter()
                .map(|d| {
                    json!({
                        "owner_nickname": d["owner_nickname"],
                        "name": d["name"],
                        "range": d["range"],
                        "resolved_version": d["resolved_version"],
                        "kind": "dependency",
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    envelope(
        200,
        json!({
            "package_id": pkg.id, "owner_nickname": st.owner_nickname, "name": pkg.name,
            "version": version.version, "manifest": version.manifest, "is_public": pkg.is_public,
            "aligned_hosts": [], "modules": modules, "dependencies": dependencies,
        }),
    )
}

/// Publish-shaped locked deps → the check-updates dependency shape: the owner field is
/// named `owner` (not `owner_nickname`) and each edge carries its relation `kind`. The
/// mock's publish wire records dependency edges only, so every row is `"dependency"`
/// (the real server also surfaces `"requires"` rows the same way).
fn check_deps_json(deps: &Value) -> Vec<Value> {
    deps.as_array()
        .map(|deps| {
            deps.iter()
                .map(|d| {
                    json!({
                        "owner": d["owner_nickname"], "name": d["name"], "range": d["range"],
                        "resolved_version": d["resolved_version"], "kind": "dependency",
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `(owner, name, resolved_version)` targets of a publish-shaped dep list — the
/// edges the closure walk follows (`kind = "dependency"` only, which is all the mock
/// publishes).
fn dep_targets(deps: &Value) -> Vec<(String, String, String)> {
    deps.as_array()
        .map(|deps| {
            deps.iter()
                .map(|d| {
                    (
                        d["owner_nickname"].as_str().unwrap_or_default().to_string(),
                        d["name"].as_str().unwrap_or_default().to_string(),
                        d["resolved_version"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The server's `latest` ordering, mirrored: `ORDER BY major DESC, minor DESC,
/// patch DESC, (prerelease IS NULL) DESC, prerelease DESC` — the numeric triple,
/// then a release above any prerelease of the same triple, then the prerelease tag
/// compared LEXICOGRAPHICALLY as text. That last leg is deliberately not semver
/// precedence: on the server `1.0.0-beta.2` outranks `1.0.0-beta.11` ("beta.2" >
/// "beta.11" as strings), and the mirror reproduces the wart rather than "fixing"
/// it — the client must see the same latest the server would pick. (The server
/// parses every published version into those columns, so unparseable strings can't
/// exist there; the lexical fallback is mock robustness only.)
fn server_latest_order(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(va), Ok(vb)) => (va.major, va.minor, va.patch)
            .cmp(&(vb.major, vb.minor, vb.patch))
            .then_with(|| match (va.pre.is_empty(), vb.pre.is_empty()) {
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                _ => va.pre.as_str().cmp(vb.pre.as_str()),
            }),
        (Ok(_), Err(_)) => Ordering::Greater,
        (Err(_), Ok(_)) => Ordering::Less,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// `POST /packages/check-updates` — the batched lockfile sweep, mirroring the server's
/// semantics the client exercises: results in request order; the uniform `not_found`
/// for an unknown package; the installed version's yanked/deleted status; `latest` =
/// the highest live non-yanked version (see [`server_latest_order`]) with modules +
/// kind-bearing dependencies and NO content URLs; the dependency-kind closure at
/// locked versions minus the request's `have` list (matched case-folded), emptied
/// wholesale when it exceeds the closure-node cap; entry/have caps → 400.
#[allow(clippy::too_many_lines)]
async fn check_updates(State(state): State<Shared>, body: String) -> Response {
    let Ok(req) = serde_json::from_str::<Value>(&body) else {
        return mock_bad_request("invalid JSON body");
    };
    let entries = req["entries"].as_array().cloned().unwrap_or_default();
    let have_in = req["have"].as_array().cloned().unwrap_or_default();
    if entries.len() > 64 {
        return mock_bad_request("too many entries");
    }
    if have_in.len() > 512 {
        return mock_bad_request("too many have entries");
    }
    // Owner nicknames and package names are case-insensitive identities, so the
    // server folds them when matching `have` entries; the version is exact.
    let have: std::collections::HashSet<(String, String, String)> = have_in
        .iter()
        .map(|h| {
            (
                h["owner"].as_str().unwrap_or_default().to_ascii_lowercase(),
                h["name"].as_str().unwrap_or_default().to_ascii_lowercase(),
                h["version"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();

    let st = state.lock().unwrap();
    let mut results = Vec::new();
    for entry in &entries {
        let owner = entry["owner"].as_str().unwrap_or_default();
        let name = entry["name"].as_str().unwrap_or_default();
        let pkg = (owner == st.owner_nickname)
            .then(|| st.packages.iter().find(|p| p.name == name))
            .flatten();
        let Some(pkg) = pkg else {
            results.push(json!({
                "owner": owner, "name": name, "status": "not_found",
                "installed": Value::Null, "latest": Value::Null, "closure": [],
            }));
            continue;
        };
        // Installed status: yanked for a live version, deleted for a retired number,
        // null for a version the server has never seen (or none sent).
        let installed = entry["installed"].as_str().map_or(Value::Null, |version| {
            if let Some(v) = pkg.versions.iter().find(|v| v.version == version) {
                json!({ "yanked": v.yanked, "deleted": false })
            } else if pkg.retired.iter().any(|r| r == version) {
                json!({ "yanked": false, "deleted": true })
            } else {
                Value::Null
            }
        });
        // Latest = the highest live non-yanked version by the SERVER'S ordering.
        let latest = pkg
            .versions
            .iter()
            .filter(|v| !v.yanked)
            .max_by(|a, b| server_latest_order(&a.version, &b.version));
        let (latest_json, closure_json) = match latest {
            None => (Value::Null, json!([])),
            Some(v) => {
                let modules: Vec<Value> = v
                    .modules
                    .iter()
                    .map(|m| {
                        json!({
                            "subpath": m.subpath, "content_hash": m.content_hash,
                            "media_type": m.media_type, "byte_size": m.byte_size,
                            "is_entry": m.is_entry,
                        })
                    })
                    .collect();
                // Closure: the distinct transitive dependency-kind nodes at their
                // locked versions, minus anything the caller's `have` covers (the
                // node is still WALKED through so its own deps join the closure).
                let mut closure = Vec::new();
                let mut queue = dep_targets(&v.dependencies);
                let mut seen = std::collections::HashSet::new();
                while let Some((dep_owner, dep_name, dep_version)) = queue.pop() {
                    if !seen.insert((dep_owner.clone(), dep_name.clone(), dep_version.clone())) {
                        continue;
                    }
                    let Some(dep_pkg) = (dep_owner == st.owner_nickname)
                        .then(|| st.packages.iter().find(|p| p.name == dep_name))
                        .flatten()
                    else {
                        continue;
                    };
                    let Some(dep_v) = dep_pkg.versions.iter().find(|dv| dv.version == dep_version)
                    else {
                        continue;
                    };
                    queue.extend(dep_targets(&dep_v.dependencies));
                    if have.contains(&(
                        dep_owner.to_ascii_lowercase(),
                        dep_name.to_ascii_lowercase(),
                        dep_version.clone(),
                    )) {
                        continue;
                    }
                    closure.push(json!({
                        "owner": dep_owner, "name": dep_name, "version": dep_version,
                        "manifest": dep_v.manifest,
                        "dependencies": check_deps_json(&dep_v.dependencies),
                    }));
                }
                // Over-cap: the whole closure is withheld — never a 400 — leaving
                // status/installed/latest intact; the client sees an uncoverable
                // closure and classifies the entry as cannot-evaluate.
                if st.closure_node_cap.is_some_and(|cap| seen.len() > cap) {
                    closure.clear();
                }
                (
                    json!({
                        "version": v.version, "published_at": "2026-06-20T00:00:00Z",
                        "manifest": v.manifest, "modules": modules,
                        "dependencies": check_deps_json(&v.dependencies),
                    }),
                    Value::Array(closure),
                )
            }
        };
        results.push(json!({
            "owner": owner, "name": name, "status": "ok",
            "installed": installed, "latest": latest_json, "closure": closure_json,
        }));
    }
    envelope(200, json!({ "results": results }))
}

async fn get_blob(
    State(state): State<Shared>,
    Path(hash): Path<String>,
    headers: HeaderMap,
) -> Response {
    if headers.contains_key("x-smudgy-package-compatibility") {
        return (StatusCode::BAD_REQUEST, "package header leaked to blob URL").into_response();
    }
    let st = state.lock().unwrap();
    match st.blobs.get(&hash) {
        Some(body) => (StatusCode::OK, body.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn package_view(pkg: &MockPackage) -> Value {
    json!({
        "id": pkg.id, "owner_id": pkg.owner_id, "name": pkg.name, "description": pkg.description,
        "is_public": pkg.is_public,
        "created_at": "2026-06-20T00:00:00Z", "updated_at": "2026-06-20T00:00:00Z",
    })
}

async fn spawn_mock() -> (String, Shared) {
    spawn_mock_router(true).await
}

/// The mock WITHOUT `POST /packages/check-updates` — a server predating the route.
/// Axum answers the unknown path with a bare 404, which the client must surface as the
/// route-missing condition its legacy per-package probe fallback keys on.
async fn spawn_mock_without_check_updates() -> (String, Shared) {
    spawn_mock_router(false).await
}

async fn spawn_mock_router(with_check_updates: bool) -> (String, Shared) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");
    let state: Shared = Arc::new(Mutex::new(MockState {
        base_url: base_url.clone(),
        owner_nickname: "wbk".to_string(),
        ..MockState::default()
    }));
    let mut app = Router::new()
        .route("/packages", post(create_package))
        .route("/packages/:id/versions", get(list_versions))
        .route("/packages/:id/versions/begin", post(begin_version))
        .route("/packages/:id/versions/finalize", post(finalize_version))
        .route(
            "/packages/:id/versions/:version",
            patch(set_version_yanked).delete(delete_version),
        )
        .route("/packages/resolve", get(resolve))
        .route("/packages/blob/:hash", get(get_blob))
        .route("/packages/upload/:hash", put(upload_blob));
    if with_check_updates {
        app = app.route("/packages/check-updates", post(check_updates));
    }
    let app = app.with_state(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base_url, state)
}

fn client(base_url: &str) -> PackageApiClient {
    PackageApiClient::new(
        base_url,
        CredentialSource::new(Some(Credential::ApiKey("smudgy_test".to_string()))),
    )
}

// --- the end-to-end test ---------------------------------------------------

#[tokio::test]
async fn create_publish_resolve_fetch_round_trip() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);

    // Create the namespace.
    let pkg = api
        .create_package("mapper", "A mapper")
        .await
        .expect("create package");
    assert_eq!(pkg.name, "mapper");

    // Publish a version with two modules.
    let modules = vec![
        PublishModule {
            subpath: "index.ts".to_string(),
            content: "export const x = 1;".to_string().into_bytes(),
            media_type: "application/typescript".to_string(),
            is_entry: true,
        },
        PublishModule {
            subpath: "util.ts".to_string(),
            content: "export const u = 2;".to_string().into_bytes(),
            media_type: "application/typescript".to_string(),
            is_entry: false,
        },
    ];
    let manifest = json!({ "name": "mapper", "version": "1.0.0" });
    let published = api
        .publish_version(pkg.id, "1.0.0", &manifest, &modules, &[], None)
        .await
        .expect("publish version");
    assert_eq!(published.version, "1.0.0");
    assert_eq!(published.modules.len(), 2);

    // Resolve and fetch each body with the client's integrity check.
    let resolved = api
        .resolve_package("wbk", "mapper", None)
        .await
        .expect("resolve");
    assert_eq!(resolved.version, "1.0.0");
    assert_eq!(resolved.owner_nickname, "wbk");
    assert_eq!(resolved.modules.len(), 2);

    let entry = resolved
        .modules
        .iter()
        .find(|m| m.is_entry)
        .expect("entry module");
    let body = api
        .fetch_module_body(&entry.content_url, &entry.content_hash)
        .await
        .expect("fetch + verify body");
    assert_eq!(body, "export const x = 1;");
}

#[tokio::test]
async fn pre_finalize_check_stops_the_immutable_version_commit() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let package = api
        .create_package("snapshot-check", "Snapshot check")
        .await
        .unwrap();
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: b"export {};".to_vec(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    let manifest = json!({ "version": "1.0.0", "entry": "index.ts" });

    let error = api
        .publish_version_checked(package.id, "1.0.0", &manifest, &modules, &[], None, || {
            Err(CloudError::InvalidInput(
                "local snapshot changed".to_string(),
            ))
        })
        .await
        .expect_err("the final precondition must stop finalize");
    assert!(matches!(error, CloudError::InvalidInput(_)));
    assert!(
        api.list_versions(package.id).await.unwrap().is_empty(),
        "uploaded blobs do not make the immutable version visible without finalize"
    );
}

/// A logged-out client (no credential) resolves + fetches a public package: the
/// public read surface omits the auth header rather than short-circuiting with
/// `Unauthorized`, so cloud-averse users can install and run public packages.
/// Mirrors the server's "no credential ⇒ anonymous, public-only viewer" rule.
/// Write endpoints stay credential-gated, proving the gate lowered only for the
/// public *read* surface.
#[tokio::test]
async fn logged_out_client_resolves_public_package_but_not_writes() {
    let (base_url, _state) = spawn_mock().await;

    // A signed-in author publishes a version.
    let author = client(&base_url);
    let pkg = author
        .create_package("mapper", "A mapper")
        .await
        .expect("create");
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: "export const x = 1;".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    let manifest = json!({ "name": "mapper", "version": "1.0.0" });
    author
        .publish_version(pkg.id, "1.0.0", &manifest, &modules, &[], None)
        .await
        .expect("publish");

    // A client with NO credential resolves and fetches it end-to-end.
    let anon = PackageApiClient::new(&base_url, CredentialSource::new(None));
    let resolved = anon
        .resolve_package("wbk", "mapper", None)
        .await
        .expect("anonymous resolve of a public package");
    assert_eq!(resolved.version, "1.0.0");
    let entry = resolved
        .modules
        .iter()
        .find(|m| m.is_entry)
        .expect("entry module");
    let body = anon
        .fetch_module_body(&entry.content_url, &entry.content_hash)
        .await
        .expect("anonymous fetch + verify");
    assert_eq!(body, "export const x = 1;");

    // A write endpoint still requires a credential — it short-circuits client
    // side before any request leaves the machine.
    let err = anon
        .create_package("private", "x")
        .await
        .expect_err("write needs auth");
    assert!(matches!(err, CloudError::Unauthorized(_)));
}

#[tokio::test]
async fn fetch_with_wrong_hash_is_integrity_error() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("mapper", "").await.unwrap();
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: "export const x = 1;".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    api.publish_version(pkg.id, "1.0.0", &json!({}), &modules, &[], None)
        .await
        .unwrap();
    let resolved = api.resolve_package("wbk", "mapper", None).await.unwrap();
    let url = &resolved.modules[0].content_url;

    // A tampered/expected hash must be rejected before the bytes are trusted.
    let result = api.fetch_module_body(url, "deadbeefdeadbeef").await;
    assert!(result.is_err(), "integrity mismatch must error");
}

#[tokio::test]
async fn binary_module_round_trips() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("fx", "").await.unwrap();

    let bytes: Vec<u8> = vec![0, 159, 146, 150, 255]; // invalid UTF-8
    let modules = vec![PublishModule {
        subpath: "fire.bin".to_string(),
        content: bytes.clone(),
        media_type: "application/octet-stream".to_string(),
        is_entry: true,
    }];
    api.publish_version(pkg.id, "1.0.0", &json!({}), &modules, &[], None)
        .await
        .unwrap();

    let resolved = api.resolve_package("wbk", "fx", None).await.unwrap();
    let m = &resolved.modules[0];
    assert_eq!(m.media_type, "application/octet-stream");
    // The bytes round-trip exactly; a String fetch rejects the non-UTF-8 body.
    let fetched = api
        .fetch_module_bytes(&m.content_url, &m.content_hash)
        .await
        .unwrap();
    assert_eq!(fetched, bytes);
    assert!(
        api.fetch_module_body(&m.content_url, &m.content_hash)
            .await
            .is_err(),
        "a non-UTF-8 body is not fetchable as text"
    );
}

#[tokio::test]
async fn publish_dedups_shared_blobs() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("mapper", "").await.unwrap();

    let shared = PublishModule {
        subpath: "a.ts".to_string(),
        content: "shared".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    };
    api.publish_version(
        pkg.id,
        "1.0.0",
        &json!({}),
        std::slice::from_ref(&shared),
        &[],
        None,
    )
    .await
    .unwrap();

    // v2 reuses the same body + adds a new one — only the new body is uploaded.
    let extra = PublishModule {
        subpath: "b.ts".to_string(),
        content: "extra".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: false,
    };
    api.publish_version(pkg.id, "1.1.0", &json!({}), &[shared, extra], &[], None)
        .await
        .unwrap();

    // Two distinct bodies stored ("shared", "extra"); the reused body was not re-uploaded.
    assert_eq!(
        state.lock().unwrap().blobs.len(),
        2,
        "shared blob deduped across versions"
    );
}

#[tokio::test]
async fn over_cap_publish_is_rejected() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("mapper", "").await.unwrap();
    // 129 modules > the 128 cap — begin rejects it before any upload.
    let modules: Vec<PublishModule> = (0..129)
        .map(|i| PublishModule {
            subpath: format!("m{i}.ts"),
            content: format!("// {i}").into_bytes(),
            media_type: "application/typescript".to_string(),
            is_entry: i == 0,
        })
        .collect();
    let result = api
        .publish_version(pkg.id, "1.0.0", &json!({}), &modules, &[], None)
        .await;
    assert!(result.is_err(), "an over-cap publish is rejected at begin");
}

#[tokio::test]
async fn publish_locks_dependency_to_highest_satisfying_version() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);

    // A dependency package with three published 1.x versions.
    let util = api.create_package("util", "").await.unwrap();
    for v in ["1.2.0", "1.3.0", "1.4.0"] {
        let modules = vec![PublishModule {
            subpath: "index.ts".to_string(),
            content: format!("export const v = \"{v}\";").into_bytes(),
            media_type: "application/typescript".to_string(),
            is_entry: true,
        }];
        let manifest = json!({ "name": "util", "version": v });
        api.publish_version(util.id, v, &manifest, &modules, &[], None)
            .await
            .expect("publish util version");
    }

    // Lock a declared `^1.2` range against the published versions (the publish path's
    // resolve -> list_versions -> pick orchestration).
    let versions = api.list_versions(util.id).await.expect("list versions");
    let resolved = highest_satisfying_version(&versions, Some("^1.2"))
        .expect("valid range")
        .expect("a satisfying version");
    assert_eq!(
        resolved, "1.4.0",
        "^1.2 collapses to the highest published 1.x"
    );

    // Publish a dependent carrying the locked dependency on the wire.
    let app = api.create_package("app", "").await.unwrap();
    let dep = PublishDependency {
        owner_nickname: "wbk".to_string(),
        name: "util".to_string(),
        range: "^1.2".to_string(),
        resolved_version: resolved,
    };
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: "import \"smudgy://wbk/util\";".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    let manifest = json!({ "name": "app", "version": "1.0.0" });
    api.publish_version(app.id, "1.0.0", &manifest, &modules, &[dep], None)
        .await
        .expect("publish app version");

    // The publish wire carried the locked dependency verbatim.
    let st = state.lock().unwrap();
    let app_pkg = st.packages.iter().find(|p| p.name == "app").unwrap();
    let recorded = &app_pkg.versions.last().unwrap().dependencies;
    assert_eq!(recorded[0]["name"], "util");
    assert_eq!(recorded[0]["range"], "^1.2");
    assert_eq!(recorded[0]["resolved_version"], "1.4.0");
}

#[tokio::test]
async fn resolve_carries_locked_dependencies() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);

    // Publish "app" carrying a dependency locked to util@1.4.0.
    let app = api.create_package("app", "").await.unwrap();
    let dep = PublishDependency {
        owner_nickname: "wbk".to_string(),
        name: "util".to_string(),
        range: "^1.2".to_string(),
        resolved_version: "1.4.0".to_string(),
    };
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: "import \"smudgy://wbk/util\";".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    let manifest = json!({ "name": "app", "version": "1.0.0" });
    api.publish_version(app.id, "1.0.0", &manifest, &modules, &[dep], None)
        .await
        .expect("publish app");

    // Resolve surfaces the locked dep (the referrer-aware version-selection input).
    let resolved = api
        .resolve_package("wbk", "app", None)
        .await
        .expect("resolve");
    assert_eq!(resolved.dependencies.len(), 1);
    assert_eq!(resolved.dependencies[0].owner_nickname, "wbk");
    assert_eq!(resolved.dependencies[0].name, "util");
    assert_eq!(resolved.dependencies[0].range, "^1.2");
    assert_eq!(resolved.dependencies[0].resolved_version, "1.4.0");
    assert_eq!(
        resolved.dependencies[0].kind,
        smudgy_cloud::DependencyKind::Dependency
    );
}

#[tokio::test]
async fn resolve_missing_package_is_not_found() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let result = api.resolve_package("wbk", "ghost", None).await;
    assert!(
        result.is_err(),
        "unknown package resolves to an error (404)"
    );
    assert_eq!(
        state.lock().unwrap().package_compatibility.as_deref(),
        Some("1"),
        "package requests advertise advisory support explicitly"
    );
}

#[tokio::test]
async fn delete_is_two_step_and_reserves_the_number() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("mapper", "").await.unwrap();
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: "export const x = 1;".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    api.publish_version(pkg.id, "1.0.0", &json!({}), &modules, &[], None)
        .await
        .unwrap();

    // Delete is the heavy, two-step action: a live version can't be deleted until yanked.
    match api.delete_version(pkg.id, "1.0.0").await {
        Err(CloudError::VersionNotYanked) => {}
        other => panic!("expected VersionNotYanked, got {other:?}"),
    }
    api.set_version_yanked(pkg.id, "1.0.0", true).await.unwrap();
    api.delete_version(pkg.id, "1.0.0").await.unwrap();

    // The number is permanently reserved: re-publishing it (even with altered content) is
    // rejected — the publish/delete/alter/re-publish loop is closed end-to-end.
    let altered = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: "export const x = 999;".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    match api
        .publish_version(pkg.id, "1.0.0", &json!({}), &altered, &[], None)
        .await
    {
        Err(CloudError::VersionUnavailable(v)) => assert_eq!(v, "1.0.0"),
        other => panic!("expected VersionUnavailable, got {other:?}"),
    }

    // The deleted number surfaces in the list flagged deleted (so the owner UI can show it).
    let versions = api.list_versions(pkg.id).await.unwrap();
    let deleted: Vec<&str> = versions
        .iter()
        .filter(|v| v.deleted)
        .map(|v| v.version.as_str())
        .collect();
    assert_eq!(deleted, ["1.0.0"]);
}

#[tokio::test]
async fn list_versions_is_semver_ordered_with_deleted_interleaved() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("mapper", "").await.unwrap();
    let module = |c: &str| {
        vec![PublishModule {
            subpath: "i.ts".to_string(),
            content: c.to_string().into_bytes(),
            media_type: "text/plain".to_string(),
            is_entry: true,
        }]
    };
    // Publish out of order, including an infill below the current max.
    api.publish_version(pkg.id, "1.0.0", &json!({}), &module("a"), &[], None)
        .await
        .unwrap();
    api.publish_version(pkg.id, "2.0.0", &json!({}), &module("b"), &[], None)
        .await
        .unwrap();
    api.publish_version(pkg.id, "1.5.0", &json!({}), &module("c"), &[], None)
        .await
        .unwrap();
    // Yank + delete the highest (2.0.0): it becomes a deleted entry that must STILL sort
    // first by semver, not be buried last — this is where insertion-order mocks drift.
    api.set_version_yanked(pkg.id, "2.0.0", true).await.unwrap();
    api.delete_version(pkg.id, "2.0.0").await.unwrap();

    let order: Vec<String> = api
        .list_versions(pkg.id)
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.version)
        .collect();
    assert_eq!(
        order,
        ["2.0.0", "1.5.0", "1.0.0"],
        "newest-first by semver, deleted interleaved"
    );
}

/// Publish one single-module version of `name` (creating the namespace when absent),
/// returning the package id — the check-updates tests' fixture press.
async fn publish_simple(
    api: &PackageApiClient,
    name: &str,
    version: &str,
    deps: &[PublishDependency],
) -> Uuid {
    let pkg = api.create_package(name, "").await.expect("create");
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: format!("export const v = \"{name}@{version}\";").into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    let manifest = json!({ "name": name, "version": version });
    api.publish_version(pkg.id, version, &manifest, &modules, deps, None)
        .await
        .expect("publish");
    pkg.id
}

fn entry(name: &str, installed: Option<&str>) -> CheckUpdatesEntry {
    CheckUpdatesEntry {
        owner: "wbk".to_string(),
        name: name.to_string(),
        installed: installed.map(str::to_string),
    }
}

#[tokio::test]
async fn check_updates_round_trips_the_batched_shape() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);

    // util has two versions; app@1.0.0 locks util@1.4.0.
    publish_simple(&api, "util", "1.2.0", &[]).await;
    publish_simple(&api, "util", "1.4.0", &[]).await;
    let dep = PublishDependency {
        owner_nickname: "wbk".to_string(),
        name: "util".to_string(),
        range: "^1.2".to_string(),
        resolved_version: "1.4.0".to_string(),
    };
    publish_simple(&api, "app", "1.0.0", std::slice::from_ref(&dep)).await;

    let response = api
        .check_updates(&[entry("app", Some("1.0.0")), entry("ghost", None)], &[])
        .await
        .expect("check-updates round-trips");

    assert_eq!(response.results.len(), 2, "one result per entry");
    let app = &response.results[0];
    assert_eq!(
        (app.name.as_str(), app.status.as_str()),
        ("app", "ok"),
        "results come back in request order"
    );
    assert_eq!(
        app.installed,
        Some(UpdateCheckInstalled {
            yanked: false,
            deleted: false
        })
    );
    let latest = app.latest.as_ref().expect("a live latest");
    assert_eq!(latest.version, "1.0.0");
    assert_eq!(
        latest.manifest["name"], "app",
        "the manifest rides verbatim"
    );
    assert_eq!(latest.modules.len(), 1);
    assert!(latest.modules[0].is_entry);
    assert!(
        !latest.modules[0].content_hash.is_empty(),
        "modules carry hashes (never content URLs)"
    );
    assert_eq!(latest.dependencies.len(), 1);
    assert_eq!(latest.dependencies[0].owner, "wbk");
    assert_eq!(latest.dependencies[0].name, "util");
    assert_eq!(latest.dependencies[0].resolved_version, "1.4.0");
    assert_eq!(latest.dependencies[0].kind, "dependency");
    assert_eq!(app.closure.len(), 1, "the locked dep joins the closure");
    assert_eq!(app.closure[0].name, "util");
    assert_eq!(app.closure[0].version, "1.4.0");
    assert_eq!(app.closure[0].manifest["name"], "util");

    // The unknown package is the uniform miss: no installed status, no latest, an
    // empty closure — indistinguishable from "not visible", by design.
    let ghost = &response.results[1];
    assert_eq!(
        (ghost.name.as_str(), ghost.status.as_str()),
        ("ghost", "not_found")
    );
    assert_eq!(ghost.installed, None);
    assert!(ghost.latest.is_none());
    assert!(ghost.closure.is_empty());
}

#[tokio::test]
async fn check_updates_honors_the_have_elision() {
    let (base_url, _state) = spawn_mock().await;
    let author = client(&base_url);
    publish_simple(&author, "util", "1.4.0", &[]).await;
    let dep = PublishDependency {
        owner_nickname: "wbk".to_string(),
        name: "util".to_string(),
        range: "^1.4".to_string(),
        resolved_version: "1.4.0".to_string(),
    };
    publish_simple(&author, "app", "1.0.0", std::slice::from_ref(&dep)).await;

    // An ANONYMOUS caller — check-updates rides the public read surface, so no
    // credential must not short-circuit — declaring it already holds util@1.4.0.
    // The have entry arrives oddly cased: owner/name are case-insensitive
    // identities, so the server folds them when matching (the version is exact).
    let anon = PackageApiClient::new(&base_url, CredentialSource::new(None));
    let response = anon
        .check_updates(
            &[entry("app", Some("1.0.0"))],
            &[CheckUpdatesHave {
                owner: "WBK".to_string(),
                name: "Util".to_string(),
                version: "1.4.0".to_string(),
            }],
        )
        .await
        .expect("anonymous check-updates");

    let app = &response.results[0];
    assert!(
        app.closure.is_empty(),
        "a have-covered node is elided from the closure, case-folded"
    );
    assert_eq!(
        app.latest.as_ref().expect("latest").dependencies.len(),
        1,
        "the dependency edge itself still rides — coverage is judged against have + cache"
    );
}

#[tokio::test]
async fn check_updates_latest_uses_the_servers_lexicographic_prerelease_order() {
    // Among prereleases of one numeric triple the server picks latest by RAW TEXT
    // comparison of the prerelease tag (`prerelease DESC` in SQL), not semver
    // precedence: "beta.2" > "beta.11". The mirror must reproduce that wart so the
    // client sees the same latest the server would serve.
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    publish_simple(&api, "edge", "1.0.0-beta.11", &[]).await;
    publish_simple(&api, "edge", "1.0.0-beta.2", &[]).await;

    let response = api
        .check_updates(&[entry("edge", Some("1.0.0-beta.11"))], &[])
        .await
        .expect("check-updates round-trips");
    assert_eq!(
        response.results[0]
            .latest
            .as_ref()
            .map(|l| l.version.as_str()),
        Some("1.0.0-beta.2"),
        "lexicographic prerelease order — semver precedence would say beta.11"
    );

    // A release of the same triple still outranks every prerelease
    // (`(prerelease IS NULL) DESC`).
    publish_simple(&api, "edge", "1.0.0", &[]).await;
    let response = api
        .check_updates(&[entry("edge", Some("1.0.0-beta.11"))], &[])
        .await
        .expect("check-updates round-trips");
    assert_eq!(
        response.results[0]
            .latest
            .as_ref()
            .map(|l| l.version.as_str()),
        Some("1.0.0")
    );
}

#[tokio::test]
async fn check_updates_over_cap_closure_is_withheld_not_an_error() {
    // The over-cap contract: a closure past the node cap comes back EMPTY with
    // status/installed/latest intact — never a 400 (only the request caps 400) —
    // and the rest of the batch is answered normally. The client side of this
    // shape (dependencies without coverage => cannot evaluate, no offer, no
    // staging) is pinned by the checker's uncoverable-closure test.
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    publish_simple(&api, "util", "1.4.0", &[]).await;
    let dep = PublishDependency {
        owner_nickname: "wbk".to_string(),
        name: "util".to_string(),
        range: "^1.4".to_string(),
        resolved_version: "1.4.0".to_string(),
    };
    publish_simple(&api, "app", "1.0.0", std::slice::from_ref(&dep)).await;
    state.lock().unwrap().closure_node_cap = Some(0);

    let response = api
        .check_updates(
            &[entry("app", Some("1.0.0")), entry("util", Some("1.4.0"))],
            &[],
        )
        .await
        .expect("an over-cap closure is not an HTTP error");

    let app = &response.results[0];
    assert_eq!(app.status, "ok");
    let latest = app.latest.as_ref().expect("latest rides intact");
    assert_eq!(latest.version, "1.0.0");
    assert_eq!(
        latest.dependencies.len(),
        1,
        "the dependency edges still ride — only the closure nodes are withheld"
    );
    assert!(app.closure.is_empty(), "the over-cap closure is withheld");
    assert_eq!(
        app.installed,
        Some(UpdateCheckInstalled {
            yanked: false,
            deleted: false
        })
    );
    // The other entry is unaffected: entries are answered independently.
    let util = &response.results[1];
    assert_eq!(util.status, "ok");
    assert_eq!(
        util.latest.as_ref().map(|l| l.version.as_str()),
        Some("1.4.0")
    );
}

#[tokio::test]
async fn check_updates_reports_installed_yank_and_delete() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let id = publish_simple(&api, "mapper", "1.0.0", &[]).await;
    publish_simple(&api, "mapper", "2.0.0", &[]).await;

    // Yank the installed 2.0.0: the entry reports yanked, and latest steps back to
    // the highest live version.
    api.set_version_yanked(id, "2.0.0", true).await.unwrap();
    let response = api
        .check_updates(&[entry("mapper", Some("2.0.0"))], &[])
        .await
        .unwrap();
    let result = &response.results[0];
    assert_eq!(
        result.installed,
        Some(UpdateCheckInstalled {
            yanked: true,
            deleted: false
        })
    );
    assert_eq!(
        result.latest.as_ref().map(|l| l.version.as_str()),
        Some("1.0.0"),
        "latest skips the yanked version"
    );

    // Hard-delete the installed 1.0.0 (yank first — the two-step rule): the entry
    // reports deleted, and with only a yanked version left there is NO latest — the
    // combination that forms the definitive deletion signal.
    api.set_version_yanked(id, "1.0.0", true).await.unwrap();
    api.delete_version(id, "1.0.0").await.unwrap();
    let response = api
        .check_updates(&[entry("mapper", Some("1.0.0"))], &[])
        .await
        .unwrap();
    let result = &response.results[0];
    assert_eq!(
        result.installed,
        Some(UpdateCheckInstalled {
            yanked: false,
            deleted: true
        })
    );
    assert!(
        result.latest.is_none(),
        "no live versions remain — deleted + no latest is the definitive signal"
    );
    assert!(result.closure.is_empty());
}

#[tokio::test]
async fn check_updates_caps_are_a_400() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);

    let too_many_entries: Vec<CheckUpdatesEntry> =
        (0..65).map(|i| entry(&format!("p{i}"), None)).collect();
    match api.check_updates(&too_many_entries, &[]).await {
        Err(CloudError::InvalidInput(_)) => {}
        other => panic!("65 entries must be a 400, got {other:?}"),
    }

    let too_much_have: Vec<CheckUpdatesHave> = (0..513)
        .map(|i| CheckUpdatesHave {
            owner: "wbk".to_string(),
            name: format!("p{i}"),
            version: "1.0.0".to_string(),
        })
        .collect();
    match api
        .check_updates(&[entry("app", None)], &too_much_have)
        .await
    {
        Err(CloudError::InvalidInput(_)) => {}
        other => panic!("513 have entries must be a 400, got {other:?}"),
    }
}

#[tokio::test]
async fn a_missing_check_updates_route_reports_route_missing() {
    // A server predating the endpoint 404s the path itself. Per-entry misses are
    // body-level not_found results, so this top-level 404 is unambiguous — the
    // condition the checker's legacy per-package probe fallback keys on.
    let (base_url, _state) = spawn_mock_without_check_updates().await;
    let api = client(&base_url);
    match api.check_updates(&[entry("app", Some("1.0.0"))], &[]).await {
        Err(CloudError::NotFoundOrNoAccess) => {}
        other => panic!("an absent route must surface as NotFoundOrNoAccess, got {other:?}"),
    }
}
