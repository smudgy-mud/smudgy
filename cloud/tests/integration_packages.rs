//! End-to-end test of [`PackageApiClient`] against a self-contained, contract-shaped
//! mock of the `/packages` routes and the signed bundle URLs. Exercises the full client
//! wire path: create namespace → publish a version (one zstd bundle up) → resolve →
//! fetch bodies (one bundle down, with the client's frame, size and SHA-256 checks), plus
//! the batched `check-updates` sweep (and its absence on an old server).
//!
//! This is a focused, standalone mock (its own tiny Axum app) so it doesn't touch the
//! shared `tests/support` `MockState`. Its fidelity reference is the registry Worker: the
//! bundle frame rules, the `want` bitmap, the always-upload rule, the 400s and 413s, and
//! signed, expiring upload and download URLs all follow the package-bundle contract. So do
//! names reserved forever on first publication (409 `package_name_unavailable`), clan-owned packages,
//! addresses whose optional owner constrains legacy resolution (resolve, check-updates,
//! publish edges), and the upload refused while garbage collection deletes one of its
//! bodies (409 `body_being_collected`, or the older 500).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::needless_pass_by_value
)]

use std::collections::{BTreeSet, HashMap};
use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zstd::zstd_safe;

use smudgy_cloud::{
    CheckUpdatesEntry, CheckUpdatesHave, CloudError, Credential, CredentialSource,
    PackageApiClient, PublishDependency, PublishModule, ResolvedPackageWire, UpdateCheckInstalled,
    highest_satisfying_version,
};

// --- mock state ------------------------------------------------------------

/// The registry's bundle size cap: a bundle past it is refused at `begin` with 413.
const BUNDLE_CAP: u64 = 95_000_000;
/// The lifetime of every signed URL the registry issues.
const SIGNED_URL_TTL_SECS: i64 = 15 * 60;
/// The key the mock signs its upload and bundle URLs with.
const URL_SIGNING_KEY: &[u8] = b"mock-package-url-key";

struct MockModule {
    subpath: String,
    content_hash: String,
    media_type: String,
    byte_size: i64,
    is_entry: bool,
}

struct MockVersion {
    id: Uuid,
    version: String,
    manifest: Value,
    modules: Vec<MockModule>,
    /// The version's bodies in canonical order: distinct content hashes by first appearance
    /// in the published module list.
    bodies: Vec<String>,
    /// The `dependencies` array sent at publish (the locked dep set), captured verbatim
    /// so tests can assert the publish wire carries the resolved versions.
    dependencies: Value,
    yanked: bool,
}

/// One body a pending publish declared at `begin`, in canonical order.
struct PendingBody {
    content_hash: String,
    byte_size: u64,
    compressed_size: u64,
    /// The first module carrying this body, which a refusal names.
    subpath: String,
}

/// A `begin` awaiting its bundle and `finalize`. A newer `begin` of the same number replaces it.
struct PendingPublish {
    id: Uuid,
    package_id: Uuid,
    version: String,
    size: u64,
    bodies: Vec<PendingBody>,
    /// Every body was uploaded and verified for this publish.
    recorded: bool,
}

/// Who owns a mock package.
#[derive(Clone, PartialEq, Eq)]
enum MockOwner {
    /// A user, by nickname.
    User(String),
    /// A clan, by ID: it has no nickname, so its packages are addressed by name alone.
    Clan(Uuid),
}

impl MockOwner {
    /// The owner's nickname, when it has one.
    fn nickname(&self) -> Option<&str> {
        match self {
            Self::User(nickname) => Some(nickname),
            Self::Clan(_) => None,
        }
    }
}

/// How the next uploads are refused while garbage collection deletes one of their bodies.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum CollectionRefusal {
    /// 409 `body_being_collected`, with `details` of `{content_hash}`.
    #[default]
    Conflict,
    /// The older 500 `body … is being collected; retry the upload`.
    Internal,
}

struct MockPackage {
    id: Uuid,
    owner_id: Uuid,
    owner: MockOwner,
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
    /// The signed-in caller's nickname; tests switch it to act as another user.
    caller: String,
    packages: Vec<MockPackage>,
    /// Every name ever claimed, lowercased, with the package that claimed it. A claim
    /// outlives its package: names are reserved forever.
    claims: Vec<(String, Uuid)>,
    /// Clans by ID: each member's nickname with the clan-wide `package.*` actions they hold.
    /// An owner holds every one; a member may hold none.
    clans: HashMap<Uuid, HashMap<String, BTreeSet<String>>>,
    /// How many upcoming uploads garbage collection refuses, and how.
    collection_refusals: usize,
    collection_refusal: CollectionRefusal,
    /// Every `begin` accepted.
    begins: usize,
    /// `bodies/zstd/<content_hash>`: each body's frame exactly as first uploaded. A later
    /// upload of the same content leaves it as is. Tests may replace one to model a
    /// misbehaving store.
    frames: HashMap<String, Vec<u8>>,
    publishes: Vec<PendingPublish>,
    /// The bundle cap `begin` enforces ([`BUNDLE_CAP`]); tests lower it.
    bundle_cap: u64,
    /// The lifetime of newly issued signed URLs ([`SIGNED_URL_TTL_SECS`]); a negative value
    /// issues URLs that have already expired.
    url_ttl_secs: i64,
    /// Every accepted bundle upload: the content hashes it carried, in order.
    bundle_uploads: Vec<Vec<String>>,
    /// Every bundle download answered: the `want` it carried (`None` = every body).
    bundle_wants: Vec<Option<String>>,
    /// Bytes appended after the selected frames of every bundle download, modelling a
    /// response longer than its frames.
    bundle_trailer: Vec<u8>,
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

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn sign(message: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(URL_SIGNING_KEY).unwrap();
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Whether `params` carry an unexpired signature over `scope` (everything the URL binds
/// except its expiry).
fn signature_holds(params: &HashMap<String, String>, scope: &str) -> bool {
    let (Some(expires), Some(sig)) = (params.get("expires"), params.get("sig")) else {
        return false;
    };
    let Ok(expires_at) = expires.parse::<i64>() else {
        return false;
    };
    expires_at >= now_secs() && *sig == sign(&format!("{scope}:{expires}"))
}

/// `?expires=…&sig=…` for a URL binding `scope`, issued now with the state's URL lifetime.
fn signed_query(st: &MockState, scope: &str) -> String {
    let expires = now_secs() + st.url_ttl_secs;
    format!(
        "expires={expires}&sig={}",
        sign(&format!("{scope}:{expires}"))
    )
}

/// libzstd's `ZSTD_COMPRESSBOUND`: no frame of `n` bytes of content is larger.
fn compress_bound(n: u64) -> u64 {
    n + (n >> 8) + if n < 131_072 { (131_072 - n) >> 11 } else { 0 }
}

/// The Worker's frame rules for one bundle segment: exactly one standard zstd frame,
/// `Frame_Content_Size` equal to `byte_size`, no dictionary, a window of at most 16 MiB;
/// decoded without passing `byte_size`, then length and SHA-256 checked.
fn check_frame(segment: &[u8], byte_size: u64, content_hash: &str) -> Result<(), String> {
    if !segment.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Err("not a standard zstd frame".into());
    }
    match zstd_safe::find_frame_compressed_size(segment) {
        Ok(length) if length == segment.len() => {}
        Ok(_) => return Err("the segment is not exactly one frame".into()),
        Err(_) => return Err("corrupt frame".into()),
    }
    match zstd_safe::get_frame_content_size(segment) {
        Ok(Some(size)) if size == byte_size => {}
        _ => return Err("Frame_Content_Size is missing or differs from byte_size".into()),
    }
    if zstd_safe::get_dict_id_from_frame(segment).is_some() {
        return Err("the frame names a dictionary".into());
    }
    let mut decoder = zstd::stream::read::Decoder::with_buffer(segment)
        .map_err(|error| error.to_string())?
        .single_frame();
    decoder
        .window_log_max(24)
        .map_err(|error| error.to_string())?;
    let mut output = Vec::new();
    decoder
        .take(byte_size + 1)
        .read_to_end(&mut output)
        .map_err(|error| format!("decode failed: {error}"))?;
    if output.len() as u64 != byte_size {
        return Err(format!(
            "decodes to {} bytes, not {byte_size}",
            output.len()
        ));
    }
    if sha256_hex(&output) != content_hash {
        return Err("content does not match its hash".into());
    }
    Ok(())
}

/// A `want` bitmap over `count` bodies: lowercase hex, byte 0 holding bits 0–7 least
/// significant bit first. Malformed hex, a set bit beyond the bodies, or an empty
/// selection is refused.
fn parse_want(raw: &str, count: usize) -> Result<Vec<bool>, &'static str> {
    if raw.is_empty()
        || !raw.len().is_multiple_of(2)
        || !raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("malformed want");
    }
    let mut selected = vec![false; count];
    for (byte_index, byte) in hex::decode(raw).unwrap().into_iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) == 0 {
                continue;
            }
            let index = byte_index * 8 + bit;
            if index >= count {
                return Err("want selects a body the version does not have");
            }
            selected[index] = true;
        }
    }
    if !selected.contains(&true) {
        return Err("want selects no bodies");
    }
    Ok(selected)
}

/// A nickname's form: 3–24 ASCII letters, digits, `_` and `-`.
fn valid_nickname(owner: &str) -> bool {
    (3..=24).contains(&owner.len())
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// An address's owner segment: absent or empty, or a well-formed nickname.
fn valid_address_owner(owner: Option<&str>) -> bool {
    owner.is_none_or(|owner| owner.trim().is_empty() || valid_nickname(owner.trim()))
}

/// A package name's form: 1–64 ASCII letters, digits, `_`, `.` and `-`, starting with a
/// letter or digit.
fn valid_package_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Every clan-wide package action (clans.md §1.1); a clan's owners hold them all.
const PACKAGE_ACTIONS: [&str; 6] = [
    "package.create",
    "package.edit_metadata",
    "package.manage_availability",
    "package.publish",
    "package.retire",
    "package.delete",
];

impl MockState {
    /// The package an address names: by name, ignoring ASCII case, whoever owns it.
    fn named(&self, name: &str) -> Option<&MockPackage> {
        let (_, holder) = self
            .claims
            .iter()
            .find(|(claimed, _)| claimed.eq_ignore_ascii_case(name))?;
        self.packages.iter().find(|p| p.id == *holder)
    }

    fn addressed(&self, owner: Option<&str>, name: &str) -> Option<&MockPackage> {
        self.named(name).filter(|p| {
            owner.is_none_or(|owner| {
                owner.trim().is_empty()
                    || p.owner
                        .nickname()
                        .is_some_and(|actual| actual.eq_ignore_ascii_case(owner.trim()))
            })
        })
    }

    /// Whether the caller holds `action` in `clan`.
    fn clan_allows(&self, clan: Uuid, action: &str) -> bool {
        self.clans
            .get(&clan)
            .and_then(|members| members.get(&self.caller))
            .is_some_and(|actions| actions.contains(action))
    }

    /// Whether the caller owns `pkg`: its user, never anyone for a clan's (packages.md §1).
    fn owns(&self, pkg: &MockPackage) -> bool {
        matches!(&pkg.owner, MockOwner::User(nickname) if *nickname == self.caller)
    }

    /// Whether the caller may do what `action` allows on `pkg`: its owner on a user's
    /// package, a member holding the clan-wide action on a clan's.
    fn may(&self, pkg: &MockPackage, action: &str) -> bool {
        match &pkg.owner {
            MockOwner::User(nickname) => *nickname == self.caller,
            MockOwner::Clan(clan) => self.clan_allows(*clan, action),
        }
    }

    /// Whether the caller sees `pkg`: public, their own, or their clan's (grants are not
    /// modelled here).
    fn sees(&self, pkg: &MockPackage) -> bool {
        pkg.is_public
            || self.owns(pkg)
            || matches!(&pkg.owner, MockOwner::Clan(clan) if self
                .clans
                .get(clan)
                .is_some_and(|members| members.contains_key(&self.caller)))
    }
}

/// `owner/name` as sent, or the bare name when the address had no owner.
fn address_of(owner: Option<&str>, name: &str) -> String {
    match owner.filter(|owner| !owner.is_empty()) {
        Some(owner) => format!("{owner}/{name}"),
        None => name.to_string(),
    }
}

fn envelope(status: u16, data: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({ "success": true, "data": data, "error": null })),
    )
        .into_response()
}

// --- mock handlers (mirror smudgy-api/src/packages) ------------------------

/// `POST /packages` — create, or get the caller's package of that name. A `clan_id` makes
/// the package the clan's, for a member holding `package.create` (404 otherwise).
/// Draft names are owner-scoped; the first publication reserves the global name.
async fn create_package(State(state): State<Shared>, body: String) -> Response {
    let req: Value = serde_json::from_str(&body).unwrap();
    let name = req["name"].as_str().unwrap().to_string();
    let mut st = state.lock().unwrap();
    let caller = st.caller.clone();
    let owner = match req.get("clan_id").and_then(Value::as_str) {
        Some(clan) => {
            let Ok(clan) = Uuid::parse_str(clan) else {
                return envelope(404, Value::Null);
            };
            if !st.clan_allows(clan, "package.create") {
                return envelope(404, Value::Null);
            }
            MockOwner::Clan(clan)
        }
        None => MockOwner::User(caller),
    };
    if !valid_package_name(&name) {
        return mock_bad_request(
            "invalid package name (alphanumeric, _.- , <=64, must start alphanumeric)",
        );
    }
    if let Some(existing) = st
        .packages
        .iter()
        .find(|p| p.owner == owner && p.name.eq_ignore_ascii_case(&name))
    {
        return envelope(201, package_view(existing));
    }
    let owner_id = match &owner {
        MockOwner::Clan(clan) => *clan,
        MockOwner::User(_) => Uuid::new_v4(),
    };
    let pkg = MockPackage {
        id: Uuid::new_v4(),
        owner_id,
        owner,
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

fn mock_name_conflict(st: &MockState, pkg: &MockPackage) -> Option<Response> {
    st.claims.iter().any(|(name, holder)| name.eq_ignore_ascii_case(&pkg.name) && *holder != pkg.id).then(|| (
        StatusCode::CONFLICT,
        Json(json!({ "success": false, "data": null, "error": format!("package_name_unavailable: {}", pkg.name) })),
    ).into_response())
}

/// The server's edge validation, `dependencies` then `requires`: an owner, when given, is a
/// well-formed nickname; the target is a known package (by name) other than this one.
fn mock_validate_edges(st: &MockState, package_id: Uuid, req: &Value) -> Option<Response> {
    for (list, kind) in [("dependencies", "dependency"), ("requires", "requires")] {
        for edge in req[list].as_array().into_iter().flatten() {
            let owner = edge["owner_nickname"].as_str();
            let name = edge["name"].as_str().unwrap_or_default();
            if !valid_address_owner(owner) {
                return Some(mock_bad_request(&format!(
                    "invalid {kind} handle: {}",
                    owner.unwrap_or_default()
                )));
            }
            let address = address_of(owner.map(str::trim), name);
            let Some(target) = st.addressed(owner, name) else {
                return Some(mock_bad_request(&format!("unknown {kind}: {address}")));
            };
            if target.id == package_id {
                return Some(mock_bad_request(if kind == "requires" {
                    "a package cannot require itself"
                } else {
                    "a package cannot depend on itself"
                }));
            }
        }
    }
    None
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

/// `…/versions/begin` — validate (mirroring the server), check every module's
/// `compressed_size`, and grant one signed bundle upload covering every body of the
/// version, stored or not (the always-upload rule). Replaces any earlier pending publish of
/// the same number; records no body.
#[allow(clippy::too_many_lines)]
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

    let mut st = state.lock().unwrap();
    let Some(pkg) = st.packages.iter().position(|p| p.id == id) else {
        return envelope(404, Value::Null);
    };
    // Publishing is the owner's, or on a clan's package a member's holding `package.publish`;
    // anyone else meets the uniform 404.
    if !st.may(&st.packages[pkg], "package.publish") {
        return envelope(404, Value::Null);
    }
    // Build metadata is precedence-noise and never stored — reject it (mirrors the server).
    if let Some(error) = mock_name_conflict(&st, &st.packages[pkg]) {
        return error;
    }
    if version.contains('+') {
        return mock_bad_request("build metadata not allowed");
    }
    if let Some(err) = mock_validate_edges(&st, id, &req) {
        return err;
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
    // The version's bodies in canonical order, each with one agreed frame size.
    let mut bodies: Vec<PendingBody> = Vec::new();
    for m in &modules {
        let subpath = m["subpath"].as_str().unwrap_or_default();
        let hash = m["content_hash"].as_str().unwrap_or_default();
        let byte_size = m["byte_size"].as_u64().unwrap_or(0);
        let Some(compressed_size) = m["compressed_size"].as_u64().filter(|&size| size > 0) else {
            return mock_bad_request(&format!(
                "module {subpath}: compressed_size must be an integer > 0"
            ));
        };
        if compressed_size > compress_bound(byte_size) {
            return mock_bad_request(&format!(
                "module {subpath}: compressed_size {compressed_size} exceeds the bound for {byte_size} bytes"
            ));
        }
        match bodies.iter().find(|body| body.content_hash == hash) {
            Some(body) if body.compressed_size != compressed_size => {
                return mock_bad_request(&format!(
                    "module {subpath}: compressed_size differs from another module with the same content_hash"
                ));
            }
            Some(_) => {}
            None => bodies.push(PendingBody {
                content_hash: hash.to_string(),
                byte_size,
                compressed_size,
                subpath: subpath.to_string(),
            }),
        }
    }
    let size: u64 = bodies.iter().map(|body| body.compressed_size).sum();
    if size > st.bundle_cap {
        return mock_too_large(&format!(
            "bundle too large: {size} bytes (max {})",
            st.bundle_cap
        ));
    }

    st.begins += 1;
    let publish = Uuid::new_v4();
    let url = format!(
        "{}/package-uploads/{publish}?size={size}&{}",
        st.base_url,
        signed_query(&st, &format!("upload:{publish}:{size}"))
    );
    st.publishes
        .retain(|pending| !(pending.package_id == id && pending.version == version));
    st.publishes.push(PendingPublish {
        id: publish,
        package_id: id,
        version,
        size,
        bodies,
        recorded: false,
    });
    envelope(
        200,
        json!({
            "bundle": {
                "url": url,
                "headers": { "content-type": "application/zstd" },
                "size": size,
            }
        }),
    )
}

/// `PUT /package-uploads/{publish}` — the signed bundle upload. Reads the body segment by
/// segment by the declared frame sizes and checks each against the frame rules; any refusal
/// is a 400 naming the module and records nothing. On success each frame is stored as
/// uploaded (an existing frame of the same content stays) and every body is recorded
/// against the pending publish.
async fn upload_bundle(
    State(state): State<Shared>,
    Path(publish): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Registry headers and credentials belong on API calls, never on signed URLs.
    if headers.contains_key("x-smudgy-package-compatibility")
        || headers.contains_key(header::AUTHORIZATION)
    {
        return mock_bad_request("registry header leaked to a signed URL");
    }
    if !headers.contains_key("x-smudgy-client-version") {
        return mock_bad_request("missing x-smudgy-client-version");
    }
    let size = params.get("size").and_then(|size| size.parse::<u64>().ok());
    let (Some(size), Ok(publish)) = (size, Uuid::parse_str(&publish)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !signature_holds(&params, &format!("upload:{publish}:{size}")) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut st = state.lock().unwrap();
    let Some(pending) = st
        .publishes
        .iter()
        .position(|pending| pending.id == publish && pending.size == size)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // `begin` named the upload's headers; the client must send exactly those.
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        != Some("application/zstd")
    {
        return mock_bad_request("the upload does not carry begin's headers");
    }
    if body.len() as u64 != size {
        return mock_bad_request("the bundle is not the size begin declared");
    }
    // Garbage collection deleting one of the bodies refuses the upload before anything is
    // stored or recorded.
    if st.collection_refusals > 0 {
        st.collection_refusals -= 1;
        let hash = st.publishes[pending]
            .bodies
            .first()
            .map(|body| body.content_hash.clone())
            .unwrap_or_default();
        let (status, body) = match st.collection_refusal {
            CollectionRefusal::Conflict => (
                StatusCode::CONFLICT,
                json!({
                    "success": false, "data": null, "error": "body_being_collected",
                    "details": { "content_hash": hash },
                }),
            ),
            CollectionRefusal::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "success": false, "data": null,
                    "error": format!("body {hash} is being collected; retry the upload"),
                }),
            ),
        };
        return (status, Json(body)).into_response();
    }

    let mut offset = 0usize;
    let mut segments = Vec::new();
    for declared in &st.publishes[pending].bodies {
        let segment = &body[offset..offset + declared.compressed_size as usize];
        offset += declared.compressed_size as usize;
        if let Err(reason) = check_frame(segment, declared.byte_size, &declared.content_hash) {
            return mock_bad_request(&format!("module {}: {reason}", declared.subpath));
        }
        segments.push((declared.content_hash.clone(), segment.to_vec()));
    }
    let hashes: Vec<String> = segments.iter().map(|(hash, _)| hash.clone()).collect();
    for (hash, frame) in segments {
        st.frames.entry(hash).or_insert(frame);
    }
    st.publishes[pending].recorded = true;
    st.bundle_uploads.push(hashes);
    envelope(200, Value::Null)
}

/// `…/versions/finalize` — every module's body must be recorded for the caller's live
/// pending publish of this number, then the version commits and the publish clears. The
/// reservation/duplicate guard runs here too (authoritative).
#[allow(clippy::too_many_lines)]
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
    if !st.may(&st.packages[pkg], "package.publish") {
        return envelope(404, Value::Null);
    }
    if version.contains('+') {
        return mock_bad_request("build metadata not allowed");
    }
    if let Some(err) = mock_validate_edges(&st, id, &req) {
        return err;
    }
    let taken = st.packages[pkg]
        .versions
        .iter()
        .any(|v| v.version == version)
        || st.packages[pkg].retired.contains(&version);
    if taken {
        return mock_version_unavailable(&version);
    }
    let pending = st.publishes.iter().position(|pending| {
        pending.package_id == id && pending.version == version && pending.recorded
    });
    // Every module's body must be recorded, at its declared size, for the live publish.
    let mut modules = Vec::new();
    let mut module_meta = Vec::new();
    let mut bodies: Vec<String> = Vec::new();
    for m in &modules_in {
        let subpath = m["subpath"].as_str().unwrap_or_default().to_string();
        let hash = m["content_hash"].as_str().unwrap_or_default().to_string();
        let byte_size = m["byte_size"].as_i64().unwrap_or(0);
        let media_type = m["media_type"].as_str().unwrap_or("text/plain").to_string();
        let is_entry = m["is_entry"].as_bool().unwrap_or(false);
        let recorded = pending.and_then(|pending| {
            st.publishes[pending]
                .bodies
                .iter()
                .find(|body| body.content_hash == hash)
        });
        match recorded {
            Some(body) if body.byte_size as i64 == byte_size => {}
            Some(_) => return mock_bad_request(&format!("module {subpath} size mismatch")),
            None => {
                return mock_bad_request(&format!(
                    "module {subpath} was not uploaded (blob {hash} missing)"
                ));
            }
        }
        if !bodies.contains(&hash) {
            bodies.push(hash.clone());
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
    if let Some(error) = mock_name_conflict(&st, &st.packages[pkg]) {
        return error;
    }
    let name = st.packages[pkg].name.to_ascii_lowercase();
    if !st.claims.iter().any(|(_, holder)| *holder == id) {
        st.claims.push((name, id));
    }
    if let Some(pending) = pending {
        st.publishes.remove(pending);
    }
    st.packages[pkg].versions.push(MockVersion {
        id: version_id,
        version: version.clone(),
        manifest: manifest.clone(),
        modules,
        bodies,
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
/// versions carry `yanked: true`; hard-deleted numbers carry `deleted: true`. Only the owner
/// (`owner_view`) sees retired numbers; nobody has the owner's view of a clan's package.
fn version_list_json(pkg: &MockPackage, owner_view: bool) -> Vec<Value> {
    // Combine live + retired and sort newest-first by true semver precedence, mirroring the
    // server's list_versions (which interleaves deleted numbers by version, NOT by insertion
    // order). Reversing insertion order + appending retired last would drift from the server.
    let mut combined: Vec<(String, bool, bool)> = pkg
        .versions
        .iter()
        .map(|v| (v.version.clone(), v.yanked, false))
        .chain(
            pkg.retired
                .iter()
                .filter(|_| owner_view)
                .map(|v| (v.clone(), false, true)),
        )
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
    envelope(200, json!(version_list_json(pkg, st.owns(pkg))))
}

/// A package's detail as the caller sees it (`GET /packages/{id}`, `/mine`): no owner
/// nickname for its owner or a clan, and `viewer_can_admin` only for a user's own package.
fn package_detail(st: &MockState, pkg: &MockPackage) -> Value {
    let own = st.owns(pkg);
    let mut view = package_view(pkg);
    if !own && let Some(nickname) = pkg.owner.nickname() {
        view["owner_nickname"] = json!(nickname);
    }
    view["latest_version"] = json!(
        pkg.versions
            .iter()
            .rev()
            .find(|v| !v.yanked)
            .map(|v| v.version.clone())
    );
    view["version_count"] = json!(pkg.versions.len());
    view["aligned_hosts"] = json!([]);
    view["avg_rating"] = Value::Null;
    view["rating_count"] = json!(0);
    view["install_count"] = json!(0);
    view["viewer_can_admin"] = json!(own);
    view
}

/// `GET /packages/{id}`: for anyone who sees it; everyone else meets the uniform 404.
async fn get_package(State(state): State<Shared>, Path(id): Path<Uuid>) -> Response {
    let st = state.lock().unwrap();
    match st.packages.iter().find(|p| p.id == id && st.sees(p)) {
        Some(pkg) => envelope(200, package_detail(&st, pkg)),
        None => envelope(404, Value::Null),
    }
}

/// `GET /packages/mine`: the caller's own packages; a clan's are never among them.
async fn list_mine(State(state): State<Shared>) -> Response {
    let st = state.lock().unwrap();
    let mine: Vec<Value> = st
        .packages
        .iter()
        .filter(|pkg| st.owns(pkg))
        .map(|pkg| package_detail(&st, pkg))
        .collect();
    envelope(200, json!(mine))
}

/// `PATCH /packages/{id}`: each field sent needs its action, `description`
/// `package.edit_metadata` and `is_public` `package.manage_availability` on a clan's
/// package; a missing one is the uniform 404.
async fn patch_package(
    State(state): State<Shared>,
    Path(id): Path<Uuid>,
    body: String,
) -> Response {
    let Ok(req) = serde_json::from_str::<Value>(&body) else {
        return mock_bad_request("invalid JSON body");
    };
    let mut st = state.lock().unwrap();
    let Some(index) = st.packages.iter().position(|p| p.id == id && st.sees(p)) else {
        return envelope(404, Value::Null);
    };
    let description = req.get("description").and_then(Value::as_str);
    let is_public = req.get("is_public").and_then(Value::as_bool);
    let pkg = &st.packages[index];
    if (description.is_some() && !st.may(pkg, "package.edit_metadata"))
        || (is_public.is_some() && !st.may(pkg, "package.manage_availability"))
    {
        return envelope(404, Value::Null);
    }
    let pkg = &mut st.packages[index];
    if let Some(description) = description {
        pkg.description = description.to_string();
    }
    if let Some(is_public) = is_public {
        pkg.is_public = is_public;
    }
    envelope(200, package_view(pkg))
}

async fn set_version_yanked(
    State(state): State<Shared>,
    Path((id, version)): Path<(Uuid, String)>,
    body: String,
) -> Response {
    let req: Value = serde_json::from_str(&body).unwrap();
    let yanked = req["yanked"].as_bool().unwrap_or(false);
    let mut st = state.lock().unwrap();
    let Some(index) = st.packages.iter().position(|p| p.id == id) else {
        return envelope(404, Value::Null);
    };
    if !st.may(&st.packages[index], "package.retire") {
        return envelope(404, Value::Null);
    }
    let owner_view = st.owns(&st.packages[index]);
    let pkg = &mut st.packages[index];
    let Some(v) = pkg.versions.iter_mut().find(|v| v.version == version) else {
        return envelope(404, Value::Null);
    };
    v.yanked = yanked;
    // Mirror patch_version: return the updated version list.
    envelope(200, json!(version_list_json(pkg, owner_view)))
}

async fn delete_version(
    State(state): State<Shared>,
    Path((id, version)): Path<(Uuid, String)>,
) -> Response {
    let mut st = state.lock().unwrap();
    let Some(index) = st.packages.iter().position(|p| p.id == id) else {
        return envelope(404, Value::Null);
    };
    if !st.may(&st.packages[index], "package.retire") {
        return envelope(404, Value::Null);
    }
    let pkg = &mut st.packages[index];
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
    // A legacy owner constrains the globally claimed name.
    if name.is_empty() || !valid_address_owner(params.get("owner").map(String::as_str)) {
        return envelope(404, Value::Null);
    }
    let Some(pkg) = st.addressed(params.get("owner").map(String::as_str), &name) else {
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
    // Modules in subpath order (as the registry lists them); `bodies` keep the canonical
    // order, so a client must index the bundle by `bodies`, never by `modules`.
    let mut listed: Vec<&MockModule> = version.modules.iter().collect();
    listed.sort_by(|a, b| a.subpath.cmp(&b.subpath));
    let modules: Vec<Value> = listed
        .into_iter()
        .map(|m| {
            json!({
                "subpath": m.subpath, "content_hash": m.content_hash, "media_type": m.media_type,
                "byte_size": m.byte_size, "is_entry": m.is_entry,
            })
        })
        .collect();
    let bodies: Vec<Value> = version
        .bodies
        .iter()
        .map(|hash| {
            let byte_size = version
                .modules
                .iter()
                .find(|m| m.content_hash == *hash)
                .map_or(0, |m| m.byte_size);
            json!({
                "content_hash": hash, "byte_size": byte_size,
                "compressed_size": st.frames.get(hash).map_or(0, Vec::len),
            })
        })
        .collect();
    let bundle_url = format!(
        "{}/package-bundles/{}?{}",
        st.base_url,
        version.id,
        signed_query(&st, &format!("bundle:{}", version.id))
    );
    // Mirror the server: surface the locked relations in resolve-shape, each naming its
    // target's current owner, omitted when that owner has no nickname. This mock's publish
    // endpoint accepts code dependencies only, so each recorded edge is a `dependency`;
    // dedicated wire tests cover `requires` deserialization.
    let dependencies: Vec<Value> = version
        .dependencies
        .as_array()
        .map(|deps| {
            deps.iter()
                .map(|d| {
                    let target = st.named(d["name"].as_str().unwrap_or_default());
                    let mut edge = json!({
                        "name": d["name"],
                        "range": d["range"],
                        "resolved_version": d["resolved_version"],
                        "kind": "dependency",
                    });
                    if let Some(target) = target {
                        edge["name"] = json!(target.name);
                        if let Some(nickname) = target.owner.nickname() {
                            edge["owner_nickname"] = json!(nickname);
                        }
                    }
                    edge
                })
                .collect()
        })
        .unwrap_or_default();
    envelope(
        200,
        json!({
            "package_id": pkg.id, "owner_nickname": pkg.owner.nickname(), "name": pkg.name,
            "version": version.version, "manifest": version.manifest, "is_public": pkg.is_public,
            "aligned_hosts": [], "modules": modules, "bodies": bodies, "bundle_url": bundle_url,
            "dependencies": dependencies,
        }),
    )
}

/// Publish-shaped locked deps → the check-updates dependency shape: the owner field is
/// named `owner` (not `owner_nickname`), names the target's current owner and is omitted
/// when that owner has no nickname, and each edge carries its relation `kind`. The mock's
/// publish wire records dependency edges only, so every row is `"dependency"` (the real
/// server also surfaces `"requires"` rows the same way).
fn check_deps_json(st: &MockState, deps: &Value) -> Vec<Value> {
    deps.as_array()
        .map(|deps| {
            deps.iter()
                .map(|d| {
                    let mut edge = json!({
                        "name": d["name"], "range": d["range"],
                        "resolved_version": d["resolved_version"], "kind": "dependency",
                    });
                    if let Some(target) = st.named(d["name"].as_str().unwrap_or_default()) {
                        edge["name"] = json!(target.name);
                        if let Some(nickname) = target.owner.nickname() {
                            edge["owner"] = json!(nickname);
                        }
                    }
                    edge
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `(name, resolved_version)` targets of a publish-shaped dep list — the edges the
/// closure walk follows (`kind = "dependency"` only, which is all the mock publishes).
fn dep_targets(deps: &Value) -> Vec<(String, String)> {
    deps.as_array()
        .map(|deps| {
            deps.iter()
                .map(|d| {
                    (
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

/// The server's `latest` ordering, mirrored: semver precedence. The numeric triple first,
/// then a release above any prerelease of the same triple, then prerelease identifiers
/// compared field by field — numeric fields as numbers (`beta.11` outranks `beta.2`).
/// Build metadata never reaches the server (publish refuses it). The server parses every
/// published version, so unparseable strings can't exist there; the lexical fallback is
/// mock robustness only.
fn server_latest_order(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(va), Ok(vb)) => va.cmp_precedence(&vb),
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
    // A legacy `have` row matches only the named owner; modern rows match by name.
    let have: std::collections::HashSet<(String, String, String)> = have_in
        .iter()
        .filter(|h| valid_address_owner(h["owner"].as_str()))
        .map(|h| {
            (
                h["owner"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase(),
                h["name"].as_str().unwrap_or_default().to_ascii_lowercase(),
                h["version"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();

    let st = state.lock().unwrap();
    let mut results = Vec::new();
    for entry in &entries {
        let owner = entry["owner"].as_str();
        let name = entry["name"].as_str().unwrap_or_default();
        // The entry's owner is echoed, and omitted when it had none.
        let echo = |mut result: Value| {
            if let Some(owner) = owner {
                result["owner"] = json!(owner);
            }
            result
        };
        let pkg = valid_address_owner(owner)
            .then(|| st.addressed(owner, name))
            .flatten();
        let Some(pkg) = pkg else {
            results.push(echo(json!({
                "name": name, "status": "not_found",
                "installed": Value::Null, "latest": Value::Null, "closure": [],
            })));
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
                while let Some((dep_name, dep_version)) = queue.pop() {
                    if !seen.insert((dep_name.to_ascii_lowercase(), dep_version.clone())) {
                        continue;
                    }
                    let Some(dep_pkg) = st.named(&dep_name) else {
                        continue;
                    };
                    let Some(dep_v) = dep_pkg.versions.iter().find(|dv| dv.version == dep_version)
                    else {
                        continue;
                    };
                    queue.extend(dep_targets(&dep_v.dependencies));
                    if have.contains(&(
                        String::new(),
                        dep_name.to_ascii_lowercase(),
                        dep_version.clone(),
                    )) || have.contains(&(
                        dep_pkg
                            .owner
                            .nickname()
                            .unwrap_or_default()
                            .to_ascii_lowercase(),
                        dep_name.to_ascii_lowercase(),
                        dep_version.clone(),
                    )) {
                        continue;
                    }
                    let mut node = json!({
                        "name": dep_pkg.name, "version": dep_version,
                        "manifest": dep_v.manifest,
                        "dependencies": check_deps_json(&st, &dep_v.dependencies),
                    });
                    if let Some(nickname) = dep_pkg.owner.nickname() {
                        node["owner"] = json!(nickname);
                    }
                    closure.push(node);
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
                        "dependencies": check_deps_json(&st, &v.dependencies),
                    }),
                    Value::Array(closure),
                )
            }
        };
        results.push(echo(json!({
            "name": name, "status": "ok",
            "installed": installed, "latest": latest_json, "closure": closure_json,
        })));
    }
    envelope(200, json!({ "results": results }))
}

/// `GET` a version's signed bundle URL, optionally narrowed by `want`: the selected frames
/// in canonical order, straight from the store. A bad, tampered or expired URL (or a version
/// since deleted) is the uniform bare 404; a malformed or empty `want` is a 400.
async fn get_bundle(
    State(state): State<Shared>,
    Path(version_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if headers.contains_key("x-smudgy-package-compatibility")
        || headers.contains_key(header::AUTHORIZATION)
    {
        return mock_bad_request("registry header leaked to a signed URL");
    }
    let Ok(version_id) = Uuid::parse_str(&version_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !signature_holds(&params, &format!("bundle:{version_id}")) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut st = state.lock().unwrap();
    let Some(version) = st
        .packages
        .iter()
        .flat_map(|pkg| &pkg.versions)
        .find(|version| version.id == version_id)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let want = params.get("want").cloned();
    let selected = match &want {
        None => vec![true; version.bodies.len()],
        Some(raw) => match parse_want(raw, version.bodies.len()) {
            Ok(selected) => selected,
            Err(reason) => return mock_bad_request(reason),
        },
    };
    let mut bundle: Vec<u8> = version
        .bodies
        .iter()
        .zip(&selected)
        .filter(|(_, chosen)| **chosen)
        .flat_map(|(hash, _)| st.frames[hash].iter().copied())
        .collect();
    bundle.extend_from_slice(&st.bundle_trailer);
    st.bundle_wants.push(want);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        bundle,
    )
        .into_response()
}

fn package_view(pkg: &MockPackage) -> Value {
    let mut view = json!({
        "id": pkg.id, "owner_id": pkg.owner_id, "name": pkg.name, "description": pkg.description,
        "is_public": pkg.is_public,
        "created_at": "2026-06-20T00:00:00Z", "updated_at": "2026-06-20T00:00:00Z",
    });
    if matches!(pkg.owner, MockOwner::Clan(_)) {
        view["owner_kind"] = json!("clan");
    }
    view
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
        caller: "wbk".to_string(),
        bundle_cap: BUNDLE_CAP,
        url_ttl_secs: SIGNED_URL_TTL_SECS,
        ..MockState::default()
    }));
    let mut app = Router::new()
        .route("/packages", post(create_package))
        .route("/packages/mine", get(list_mine))
        .route("/packages/:id", get(get_package).patch(patch_package))
        .route("/packages/:id/versions", get(list_versions))
        .route("/packages/:id/versions/begin", post(begin_version))
        .route("/packages/:id/versions/finalize", post(finalize_version))
        .route(
            "/packages/:id/versions/:version",
            patch(set_version_yanked).delete(delete_version),
        )
        .route("/packages/resolve", get(resolve))
        .route("/package-uploads/:publish", put(upload_bundle))
        .route("/package-bundles/:version", get(get_bundle));
    if with_check_updates {
        app = app.route("/packages/check-updates", post(check_updates));
    } else {
        // `/packages/{id}` would answer the absent route's POST with a 405; an older server
        // answers its bare 404.
        app = app.route(
            "/packages/check-updates",
            post(|| async { StatusCode::NOT_FOUND }),
        );
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
    let (base_url, state) = spawn_mock().await;
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

    // One bundle went up, carrying both bodies.
    assert_eq!(state.lock().unwrap().bundle_uploads.len(), 1);
    assert_eq!(state.lock().unwrap().bundle_uploads[0].len(), 2);

    // Resolve and fetch every body in one bundle, with the client's frame and integrity
    // checks.
    let resolved = api
        .resolve_package(Some("wbk"), "mapper", None)
        .await
        .expect("resolve");
    assert_eq!(resolved.version, "1.0.0");
    assert_eq!(resolved.owner_nickname.as_deref(), Some("wbk"));
    assert_eq!(resolved.modules.len(), 2);
    assert_eq!(resolved.bodies.len(), 2);

    let hashes: Vec<&str> = resolved
        .modules
        .iter()
        .map(|m| m.content_hash.as_str())
        .collect();
    let bodies = api
        .fetch_bodies(&resolved.bundle_url, &resolved.bodies, &hashes)
        .await
        .expect("fetch + verify bodies");
    let entry = resolved
        .modules
        .iter()
        .find(|m| m.is_entry)
        .expect("entry module");
    assert_eq!(bodies[&entry.content_hash], b"export const x = 1;");
    let util = resolved
        .modules
        .iter()
        .find(|m| m.subpath == "util.ts")
        .expect("util module");
    assert_eq!(bodies[&util.content_hash], b"export const u = 2;");
    assert_eq!(
        state.lock().unwrap().bundle_wants,
        [None],
        "every body is the whole bundle: no want"
    );
}

#[tokio::test]
async fn publish_sends_bodies_in_canonical_order() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("order", "").await.unwrap();
    // Publish order differs from subpath order, and two modules share one body.
    let module = |subpath: &str, content: &str| PublishModule {
        subpath: subpath.to_string(),
        content: content.as_bytes().to_vec(),
        media_type: "application/typescript".to_string(),
        is_entry: subpath == "z.ts",
    };
    let modules = vec![
        module("z.ts", "export const z = 26;"),
        module("b.ts", "export const shared = true;"),
        module("a.ts", "export const a = 1;"),
        module("c.ts", "export const shared = true;"),
    ];
    api.publish_version(pkg.id, "1.0.0", &json!({}), &modules, &[], None)
        .await
        .expect("publish");

    let expected: Vec<String> = ["z.ts", "b.ts", "a.ts"]
        .iter()
        .map(|subpath| {
            let m = modules.iter().find(|m| m.subpath == *subpath).unwrap();
            sha256_hex(&m.content)
        })
        .collect();
    assert_eq!(
        state.lock().unwrap().bundle_uploads,
        std::slice::from_ref(&expected),
        "the bundle carries each distinct body once, in first-appearance order"
    );

    // The registry lists modules by subpath, but bodies keep the canonical order: the
    // client indexes the bundle by `bodies`.
    let resolved = api
        .resolve_package(Some("wbk"), "order", None)
        .await
        .unwrap();
    let listed: Vec<&str> = resolved
        .modules
        .iter()
        .map(|m| m.subpath.as_str())
        .collect();
    assert_eq!(listed, ["a.ts", "b.ts", "c.ts", "z.ts"]);
    let body_order: Vec<&str> = resolved
        .bodies
        .iter()
        .map(|b| b.content_hash.as_str())
        .collect();
    assert_eq!(body_order, expected);
    let a_hash = sha256_hex(b"export const a = 1;");
    let fetched = api
        .fetch_body(&resolved.bundle_url, &resolved.bodies, &a_hash)
        .await
        .expect("fetch the third body alone");
    assert_eq!(fetched, b"export const a = 1;");
    assert_eq!(
        state.lock().unwrap().bundle_wants,
        [Some("04".to_string())],
        "a.ts is bodies[2] whatever its place in the module list"
    );
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
        .resolve_package(Some("wbk"), "mapper", None)
        .await
        .expect("anonymous resolve of a public package");
    assert_eq!(resolved.version, "1.0.0");
    let entry = resolved
        .modules
        .iter()
        .find(|m| m.is_entry)
        .expect("entry module");
    let body = anon
        .fetch_body(&resolved.bundle_url, &resolved.bodies, &entry.content_hash)
        .await
        .expect("anonymous fetch + verify");
    assert_eq!(body, b"export const x = 1;");

    // A write endpoint still requires a credential — it short-circuits client
    // side before any request leaves the machine.
    let err = anon
        .create_package("private", "x")
        .await
        .expect_err("write needs auth");
    assert!(matches!(err, CloudError::Unauthorized(_)));
}

/// Publishes `files` as `name@version` (creating the namespace when absent), the first file
/// being the entry.
async fn publish_files(api: &PackageApiClient, name: &str, version: &str, files: &[(&str, &[u8])]) {
    let pkg = api.create_package(name, "").await.expect("create");
    let modules: Vec<PublishModule> = files
        .iter()
        .enumerate()
        .map(|(index, (subpath, content))| PublishModule {
            subpath: (*subpath).to_string(),
            content: content.to_vec(),
            media_type: "application/typescript".to_string(),
            is_entry: index == 0,
        })
        .collect();
    api.publish_version(pkg.id, version, &json!({}), &modules, &[], None)
        .await
        .expect("publish");
}

fn module_hash(resolved: &ResolvedPackageWire, subpath: &str) -> String {
    resolved
        .modules
        .iter()
        .find(|m| m.subpath == subpath)
        .expect("module")
        .content_hash
        .clone()
}

/// One zstd frame of `content` that meets the contract's frame rules.
fn frame_of(content: &[u8]) -> Vec<u8> {
    let mut compressor = zstd::bulk::Compressor::new(3).unwrap();
    compressor
        .set_parameter(zstd_safe::CParameter::ChecksumFlag(true))
        .unwrap();
    compressor.compress(content).unwrap()
}

/// Replaces the stored frame of the body hashing to `content_hash`, modelling a store or
/// server that hands out a frame other than the one uploaded.
fn plant_frame(state: &Shared, content_hash: &str, frame: Vec<u8>) {
    state
        .lock()
        .unwrap()
        .frames
        .insert(content_hash.to_string(), frame);
}

/// Fetches `subpath`'s body expecting a refusal, returning its message.
async fn refused_fetch(api: &PackageApiClient, package: &str, subpath: &str) -> String {
    let resolved = api
        .resolve_package(Some("wbk"), package, None)
        .await
        .unwrap();
    let hash = module_hash(&resolved, subpath);
    match api
        .fetch_body(&resolved.bundle_url, &resolved.bodies, &hash)
        .await
    {
        Err(CloudError::SerializationError(message)) => message,
        other => panic!("expected the body to be refused, got {other:?}"),
    }
}

#[tokio::test]
async fn a_frame_of_other_content_is_an_integrity_error() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    publish_files(
        &api,
        "mapper",
        "1.0.0",
        &[("index.ts", b"export const x = 1;")],
    )
    .await;

    // A frame of other content of the same length passes every frame rule; only the
    // SHA-256 of what it decodes to gives it away.
    plant_frame(
        &state,
        &sha256_hex(b"export const x = 1;"),
        frame_of(b"export const y = 2;"),
    );
    let message = refused_fetch(&api, "mapper", "index.ts").await;
    assert!(message.contains("integrity mismatch"), "{message}");
}

#[tokio::test]
async fn binary_and_empty_modules_round_trip() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("fx", "").await.unwrap();

    let bytes: Vec<u8> = vec![0, 159, 146, 150, 255]; // invalid UTF-8
    let modules = vec![
        PublishModule {
            subpath: "fire.bin".to_string(),
            content: bytes.clone(),
            media_type: "application/octet-stream".to_string(),
            is_entry: true,
        },
        PublishModule {
            subpath: "empty.ts".to_string(),
            content: Vec::new(),
            media_type: "application/typescript".to_string(),
            is_entry: false,
        },
    ];
    api.publish_version(pkg.id, "1.0.0", &json!({}), &modules, &[], None)
        .await
        .unwrap();

    let resolved = api.resolve_package(Some("wbk"), "fx", None).await.unwrap();
    let fire = resolved
        .modules
        .iter()
        .find(|m| m.subpath == "fire.bin")
        .unwrap();
    assert_eq!(fire.media_type, "application/octet-stream");
    let fetched = api
        .fetch_bodies(
            &resolved.bundle_url,
            &resolved.bodies,
            &[&fire.content_hash, &module_hash(&resolved, "empty.ts")],
        )
        .await
        .unwrap();
    assert_eq!(
        fetched[&fire.content_hash], bytes,
        "raw bytes round-trip exactly"
    );
    assert_eq!(fetched[&module_hash(&resolved, "empty.ts")], b"");
}

#[tokio::test]
async fn every_publish_uploads_every_body_and_the_store_keeps_one_frame_each() {
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
    let first_frame = state.lock().unwrap().frames[&sha256_hex(b"shared")].clone();

    // v2 reuses the body and adds one: its bundle still carries both (the always-upload
    // rule), and the stored frame of the reused body is left as it was.
    let extra = PublishModule {
        subpath: "b.ts".to_string(),
        content: "extra".to_string().into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: false,
    };
    api.publish_version(pkg.id, "1.1.0", &json!({}), &[shared, extra], &[], None)
        .await
        .unwrap();

    let st = state.lock().unwrap();
    assert_eq!(
        st.bundle_uploads[1],
        [sha256_hex(b"shared"), sha256_hex(b"extra")],
        "every body of the version travels in its bundle"
    );
    assert_eq!(st.frames.len(), 2, "one stored frame per content hash");
    assert_eq!(st.frames[&sha256_hex(b"shared")], first_frame);
}

#[tokio::test]
async fn an_update_fetches_only_the_bodies_it_lacks() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let v1_files: [(&str, &[u8]); 3] = [
        ("index.ts", b"export * from './util.ts';"),
        ("util.ts", b"export const version = 1;"),
        ("data.json", b"{\"rooms\": 3}"),
    ];
    publish_files(&api, "mapper", "1.0.0", &v1_files).await;

    // The install fetches every body; the "cache" is what it holds afterwards.
    let v1 = api
        .resolve_package(Some("wbk"), "mapper", Some("1.0.0"))
        .await
        .unwrap();
    let all: Vec<&str> = v1.modules.iter().map(|m| m.content_hash.as_str()).collect();
    let mut cache = api
        .fetch_bodies(&v1.bundle_url, &v1.bodies, &all)
        .await
        .unwrap();

    // v1.1.0 changes only util.ts.
    let v2_files: [(&str, &[u8]); 3] = [
        ("index.ts", b"export * from './util.ts';"),
        ("util.ts", b"export const version = 2;"),
        ("data.json", b"{\"rooms\": 3}"),
    ];
    publish_files(&api, "mapper", "1.1.0", &v2_files).await;
    let v2 = api
        .resolve_package(Some("wbk"), "mapper", Some("1.1.0"))
        .await
        .unwrap();
    let missing: Vec<&str> = v2
        .modules
        .iter()
        .map(|m| m.content_hash.as_str())
        .filter(|hash| !cache.contains_key(*hash))
        .collect();
    assert_eq!(missing, [sha256_hex(b"export const version = 2;")]);
    let fetched = api
        .fetch_bodies(&v2.bundle_url, &v2.bodies, &missing)
        .await
        .unwrap();
    assert_eq!(fetched.len(), 1);
    cache.extend(fetched);

    assert_eq!(
        state.lock().unwrap().bundle_wants,
        [None, Some("02".to_string())],
        "the install takes the whole bundle; the update selects bodies[1] (util.ts) alone"
    );
    for (subpath, content) in v2_files {
        assert_eq!(cache[&module_hash(&v2, subpath)], content, "{subpath}");
    }
}

#[tokio::test]
async fn a_want_past_eight_bodies_spans_bytes() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let contents: Vec<String> = (0..10).map(|i| format!("export const n = {i};")).collect();
    let files: Vec<(String, &[u8])> = contents
        .iter()
        .enumerate()
        .map(|(i, content)| (format!("m{i}.ts"), content.as_bytes()))
        .collect();
    let files: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(subpath, content)| (subpath.as_str(), *content))
        .collect();
    publish_files(&api, "wide", "1.0.0", &files).await;

    let resolved = api
        .resolve_package(Some("wbk"), "wide", None)
        .await
        .unwrap();
    let wanted = [
        sha256_hex(contents[0].as_bytes()),
        sha256_hex(contents[9].as_bytes()),
    ];
    let fetched = api
        .fetch_bodies(
            &resolved.bundle_url,
            &resolved.bodies,
            &[wanted[0].as_str(), wanted[1].as_str()],
        )
        .await
        .unwrap();
    assert_eq!(fetched[&wanted[0]], contents[0].as_bytes());
    assert_eq!(fetched[&wanted[1]], contents[9].as_bytes());
    assert_eq!(
        state.lock().unwrap().bundle_wants,
        [Some("0102".to_string())]
    );
}

#[tokio::test]
async fn a_bundle_over_the_size_cap_is_refused_at_begin() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("huge", "").await.unwrap();
    state.lock().unwrap().bundle_cap = 16;
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: b"export const big = 'more than sixteen compressed bytes';".to_vec(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];

    match api
        .publish_version(pkg.id, "1.0.0", &json!({}), &modules, &[], None)
        .await
    {
        Err(CloudError::TooLarge(message)) => {
            assert!(message.contains("bundle too large"), "{message}");
        }
        other => panic!("a bundle over the cap must surface as TooLarge, got {other:?}"),
    }
    {
        let st = state.lock().unwrap();
        assert!(st.bundle_uploads.is_empty(), "nothing was uploaded");
        assert!(st.publishes.is_empty(), "begin recorded no publish");
    }
    assert!(api.list_versions(pkg.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_frame_that_decodes_past_its_byte_size_is_refused() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let body = vec![0u8; 300_000];
    publish_files(&api, "bomb", "1.0.0", &[("index.ts", &body)]).await;

    // A frame recording the body's 300,000 bytes whose blocks hold twice that. A 1 KiB
    // window keeps libzstd's ring buffer far below the claim, so only the client's output
    // limit stops it.
    let mut compressor = zstd::bulk::Compressor::new(3).unwrap();
    compressor
        .set_parameter(zstd_safe::CParameter::WindowLog(10))
        .unwrap();
    let mut bomb = compressor.compress(&vec![0u8; 600_000]).unwrap();
    assert_eq!(
        bomb[4] & 0xE0,
        0x80,
        "a 4-byte content size in a multi-segment frame"
    );
    bomb[6..10].copy_from_slice(&300_000u32.to_le_bytes());
    plant_frame(&state, &sha256_hex(&body), bomb);

    let message = refused_fetch(&api, "bomb", "index.ts").await;
    assert!(
        message.contains("decodes to more than its 300000 bytes"),
        "{message}"
    );
}

#[tokio::test]
async fn malformed_frames_from_the_server_are_refused() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let content = b"export const answer = 42;\n".repeat(50);
    publish_files(&api, "mapper", "1.0.0", &[("index.ts", &content)]).await;
    let hash = sha256_hex(&content);
    let good = frame_of(&content);

    let mut truncated = good.clone();
    truncated.truncate(good.len() - 2);
    let mut trailing = good.clone();
    trailing.extend_from_slice(&[0, 0]);
    let mut skippable = vec![0x50, 0x2A, 0x4D, 0x18, 0, 0, 0, 0];
    skippable.extend_from_slice(&good);
    let mut compressor = zstd::bulk::Compressor::new(3).unwrap();
    compressor
        .set_parameter(zstd_safe::CParameter::ContentSizeFlag(false))
        .unwrap();
    let without_size = compressor.compress(&content).unwrap();

    for (case, frame, expected) in [
        ("truncated", truncated, "not a valid frame"),
        ("trailing bytes", trailing, "bytes follow the frame"),
        (
            "skippable frame first",
            skippable,
            "does not start a standard zstd frame",
        ),
        (
            "no content size",
            without_size,
            "does not record its content size",
        ),
        (
            "not zstd",
            b"plain text, not a frame".to_vec(),
            "does not start",
        ),
    ] {
        plant_frame(&state, &hash, frame);
        let message = refused_fetch(&api, "mapper", "index.ts").await;
        assert!(message.contains(&hash), "{case}: {message}");
        assert!(message.contains(expected), "{case}: {message}");
    }
}

#[tokio::test]
async fn a_frame_size_past_the_compress_bound_is_refused_before_reading() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    publish_files(&api, "tiny", "1.0.0", &[("index.ts", b"x")]).await;

    // No frame of one byte exceeds 64 bytes, so a server declaring 100 is refused unread.
    let mut padded = frame_of(b"x");
    padded.resize(100, 0);
    plant_frame(&state, &sha256_hex(b"x"), padded);
    let message = refused_fetch(&api, "tiny", "index.ts").await;
    assert!(message.contains("declares a 100-byte frame"), "{message}");
    assert!(
        state.lock().unwrap().bundle_wants.is_empty(),
        "the bundle was never requested"
    );
}

#[tokio::test]
async fn a_bundle_longer_than_its_frames_is_refused() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    publish_files(&api, "mapper", "1.0.0", &[("index.ts", b"export {};")]).await;
    state.lock().unwrap().bundle_trailer = vec![0];
    let message = refused_fetch(&api, "mapper", "index.ts").await;
    assert!(message.contains("where its frames total"), "{message}");
}

#[tokio::test]
async fn tampered_or_expired_bundle_urls_are_not_found() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    publish_files(&api, "mapper", "1.0.0", &[("index.ts", b"export {};")]).await;
    let hash = sha256_hex(b"export {};");

    let resolved = api
        .resolve_package(Some("wbk"), "mapper", None)
        .await
        .unwrap();
    let tampered = resolved.bundle_url.replace("sig=", "sig=0");
    assert!(matches!(
        api.fetch_body(&tampered, &resolved.bodies, &hash).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));

    state.lock().unwrap().url_ttl_secs = -1;
    let expired = api
        .resolve_package(Some("wbk"), "mapper", None)
        .await
        .unwrap();
    assert!(matches!(
        api.fetch_body(&expired.bundle_url, &expired.bodies, &hash)
            .await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
}

/// The mock refuses what the contract refuses, so the client tests above run against a
/// registry that would catch a client breaking the contract.
#[tokio::test]
async fn the_mock_refuses_what_the_contract_refuses() {
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    let files: [(&str, &[u8]); 3] = [("a.ts", b"a"), ("b.ts", b"b"), ("c.ts", b"c")];
    publish_files(&api, "abc", "1.0.0", &files).await;
    let resolved = api.resolve_package(Some("wbk"), "abc", None).await.unwrap();
    let raw = reqwest::Client::new();

    for (want, status) in [
        ("zz", 400),
        ("1", 400),
        ("0A", 400),
        ("08", 400),
        ("00", 400),
        ("05", 200),
        ("0100", 200),
    ] {
        let response = raw
            .get(format!("{}&want={want}", resolved.bundle_url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "want={want}");
    }

    let pkg = api.create_package("raw", "").await.unwrap();
    let begin = |modules: Value| {
        let raw = raw.clone();
        let url = format!("{base_url}/packages/{}/versions/begin", pkg.id);
        async move {
            raw.post(url)
                .json(&json!({ "version": "1.0.0", "manifest": {}, "modules": modules }))
                .send()
                .await
                .unwrap()
        }
    };
    let module = |subpath: &str, content: &[u8], compressed_size: Value| {
        json!({
            "subpath": subpath, "content_hash": sha256_hex(content),
            "byte_size": content.len(), "compressed_size": compressed_size,
            "media_type": "application/typescript", "is_entry": subpath == "a.ts",
        })
    };
    for (case, modules) in [
        (
            "missing",
            json!([{ "subpath": "a.ts", "content_hash": sha256_hex(b"a"), "byte_size": 1 }]),
        ),
        ("zero", json!([module("a.ts", b"a", json!(0))])),
        ("past the bound", json!([module("a.ts", b"a", json!(65))])),
        (
            "disagreeing",
            json!([
                module("a.ts", b"a", json!(14)),
                module("b.ts", b"a", json!(15))
            ]),
        ),
    ] {
        assert_eq!(begin(modules).await.status().as_u16(), 400, "{case}");
    }

    // A bundle whose frame decodes to other content is a 400 naming the module, and
    // records nothing: finalize then finds the body missing.
    let frame = frame_of(b"b");
    let granted: Value = begin(json!([module("a.ts", b"a", json!(frame.len()))]))
        .await
        .json()
        .await
        .unwrap();
    let bundle = &granted["data"]["bundle"];
    let response = raw
        .put(bundle["url"].as_str().unwrap())
        .header("content-type", "application/zstd")
        .header("x-smudgy-client-version", "0.5.8")
        .body(frame)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    assert!(response.text().await.unwrap().contains("module a.ts"));
    let finalize = raw
        .post(format!("{base_url}/packages/{}/versions/finalize", pkg.id))
        .json(&json!({
            "version": "1.0.0", "manifest": {},
            "modules": [module("a.ts", b"a", json!(14))],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(finalize.status().as_u16(), 400);
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
        owner_nickname: Some("wbk".to_string()),
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

    // Publish "app" carrying a dependency locked to util@1.4.0 (an edge's target must exist).
    publish_simple(&api, "util", "1.4.0", &[]).await;
    let app = api.create_package("app", "").await.unwrap();
    let dep = PublishDependency {
        owner_nickname: Some("wbk".to_string()),
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
        .resolve_package(Some("wbk"), "app", None)
        .await
        .expect("resolve");
    assert_eq!(resolved.dependencies.len(), 1);
    assert_eq!(
        resolved.dependencies[0].owner_nickname.as_deref(),
        Some("wbk")
    );
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
    let result = api.resolve_package(Some("wbk"), "ghost", None).await;
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
        owner: Some("wbk".to_string()),
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
        owner_nickname: Some("wbk".to_string()),
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
    assert_eq!(latest.dependencies[0].owner.as_deref(), Some("wbk"));
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
        owner_nickname: Some("wbk".to_string()),
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
                owner: Some("WBK".to_string()),
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
async fn check_updates_latest_orders_prereleases_by_semver_precedence() {
    // Among prereleases of one numeric triple the server picks latest by semver
    // precedence: numeric identifiers compare as numbers, so "beta.11" outranks
    // "beta.2" (raw text order would say the opposite).
    let (base_url, _state) = spawn_mock().await;
    let api = client(&base_url);
    publish_simple(&api, "edge", "1.0.0-beta.11", &[]).await;
    publish_simple(&api, "edge", "1.0.0-beta.2", &[]).await;

    let response = api
        .check_updates(&[entry("edge", Some("1.0.0-beta.2"))], &[])
        .await
        .expect("check-updates round-trips");
    assert_eq!(
        response.results[0]
            .latest
            .as_ref()
            .map(|l| l.version.as_str()),
        Some("1.0.0-beta.11"),
        "semver precedence compares the numeric identifier as a number"
    );

    // A release of the same triple still outranks every prerelease.
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
        owner_nickname: Some("wbk".to_string()),
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
            owner: Some("wbk".to_string()),
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

// --- global names, clan packages, optional owners ---------------------------

/// Publishes one module of `name@version` into an existing package.
async fn publish_into(
    api: &PackageApiClient,
    package_id: Uuid,
    name: &str,
    version: &str,
    deps: &[PublishDependency],
) -> Result<(), CloudError> {
    let modules = vec![PublishModule {
        subpath: "index.ts".to_string(),
        content: format!("export const v = \"{name}@{version}\";").into_bytes(),
        media_type: "application/typescript".to_string(),
        is_entry: true,
    }];
    let manifest = json!({ "name": name, "version": version });
    api.publish_version(package_id, version, &manifest, &modules, deps, None)
        .await
        .map(|_| ())
}

fn act_as(state: &Shared, nickname: &str) {
    state.lock().unwrap().caller = nickname.to_string();
}

/// A clan whose `members` are its owners, holding every package action.
fn clan_with(state: &Shared, members: &[&str]) -> Uuid {
    let clan = Uuid::new_v4();
    let everything: BTreeSet<String> = PACKAGE_ACTIONS.iter().map(ToString::to_string).collect();
    state.lock().unwrap().clans.insert(
        clan,
        members
            .iter()
            .map(|member| (member.to_string(), everything.clone()))
            .collect(),
    );
    clan
}

/// Adds `member` to `clan` holding exactly `actions` there.
fn join_clan(state: &Shared, clan: Uuid, member: &str, actions: &[&str]) {
    state.lock().unwrap().clans.entry(clan).or_default().insert(
        member.to_string(),
        actions.iter().map(ToString::to_string).collect(),
    );
}

#[tokio::test]
async fn package_names_are_global_and_reserved_forever() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);

    let mine = api.create_package("mapper", "").await.expect("create");
    // Creating the caller's own name again, in any case, returns that package.
    let again = api
        .create_package("Mapper", "")
        .await
        .expect("create-or-get");
    assert_eq!(again.id, mine.id);

    publish_into(&api, mine.id, "mapper", "1.0.0", &[])
        .await
        .expect("private publication claims name");
    // Another owner may keep a draft but cannot publish the claimed name.
    act_as(&state, "alice");
    let draft = api
        .create_package("MAPPER", "")
        .await
        .expect("same-name draft");
    match publish_into(&api, draft.id, "MAPPER", "1.0.0", &[]).await {
        Err(CloudError::PackageNameUnavailable(name)) => assert_eq!(name, "MAPPER"),
        other => panic!("publishing another owner's name must be a 409, got {other:?}"),
    }

    // A deleted package keeps its claim: not even its former owner can take it again.
    act_as(&state, "wbk");
    state.lock().unwrap().packages.retain(|p| p.id != mine.id);
    let draft = api
        .create_package("mapper", "")
        .await
        .expect("replacement draft");
    match publish_into(&api, draft.id, "mapper", "1.0.0", &[]).await {
        Err(CloudError::PackageNameUnavailable(_)) => {}
        other => panic!("a deleted package's name stays claimed, got {other:?}"),
    }

    // A malformed name is a 400, not a name conflict.
    assert!(matches!(
        api.create_package("-dash-first", "").await,
        Err(CloudError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn a_clan_package_has_no_owner_and_resolves_by_name() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let clan = clan_with(&state, &["wbk"]);

    let pkg = api
        .create_clan_package(clan, "guild-tools", "for the guild")
        .await
        .expect("a member creates the clan's package");
    assert!(pkg.is_clan_owned());
    assert_eq!(pkg.owner_id, clan, "a clan package's owner_id is the clan");
    assert_eq!(pkg.owner_nickname, None);
    publish_into(&api, pkg.id, "guild-tools", "1.0.0", &[])
        .await
        .expect("publish");

    // `smudgy:@guild-tools` sends no owner; the answer names none either.
    let resolved = api
        .resolve_package(None, "guild-tools", None)
        .await
        .expect("resolve by name");
    assert_eq!(resolved.package_id, pkg.id);
    assert_eq!(resolved.owner_nickname, None);
    // A clan package cannot be addressed as if it belonged to a user.
    assert!(matches!(
        api.resolve_package(Some("anyone"), "Guild-Tools", None)
            .await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    // A malformed owner segment is the uniform 404.
    for owner in ["no", "has space", "a/b"] {
        assert!(
            matches!(
                api.resolve_package(Some(owner), "guild-tools", None).await,
                Err(CloudError::NotFoundOrNoAccess)
            ),
            "{owner}"
        );
    }

    // A non-member cannot create in the clan: the uniform 404.
    act_as(&state, "alice");
    assert!(matches!(
        api.create_clan_package(clan, "alice-tools", "").await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    // A draft is allowed, but its publication cannot take the clan's name.
    let draft = api.create_package("guild-tools", "").await.unwrap();
    assert!(matches!(
        publish_into(&api, draft.id, "guild-tools", "1.0.0", &[]).await,
        Err(CloudError::PackageNameUnavailable(_))
    ));
}

/// Publishing into a clan: a clan's package follows the clan-wide `package.*` actions, nobody
/// has the owner's view of it, its members see it while it is private, and every refusal is
/// the uniform 404.
#[tokio::test]
async fn a_clan_package_follows_the_clans_package_actions() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let clan = clan_with(&state, &["wbk"]);
    join_clan(&state, clan, "ann", &["package.publish"]);
    join_clan(&state, clan, "bo", &[]);
    let refused =
        |result: Result<_, CloudError>| matches!(result, Err(CloudError::NotFoundOrNoAccess));

    // Creating one needs `package.create`.
    act_as(&state, "ann");
    assert!(refused(
        api.create_clan_package(clan, "guild-maps", "")
            .await
            .map(|_| ())
    ));
    act_as(&state, "wbk");
    let pkg = api
        .create_clan_package(clan, "guild-maps", "the guild's maps")
        .await
        .expect("an owner creates the clan's package");
    let detail = api.get_package(pkg.id).await.expect("the clan's detail");
    assert!(detail.package.is_clan_owned());
    assert!(!detail.viewer_can_admin, "nobody has the owner's view");
    assert_eq!(detail.package.owner_nickname, None);
    assert!(
        api.list_my_packages()
            .await
            .unwrap()
            .iter()
            .all(|mine| mine.package.id != pkg.id),
        "a clan's package is nobody's own"
    );

    // Publishing a version needs `package.publish`; every member sees the private package.
    act_as(&state, "bo");
    assert!(api.get_package(pkg.id).await.is_ok());
    assert!(refused(
        publish_into(&api, pkg.id, "guild-maps", "1.0.0", &[]).await
    ));
    act_as(&state, "ann");
    publish_into(&api, pkg.id, "guild-maps", "1.0.0", &[])
        .await
        .expect("a member holding package.publish publishes");
    publish_into(&api, pkg.id, "guild-maps", "1.1.0", &[])
        .await
        .expect("and a newer version");
    assert_eq!(
        api.get_package(pkg.id)
            .await
            .unwrap()
            .latest_version
            .as_deref(),
        Some("1.1.0")
    );

    // Yanking needs `package.retire`, visibility `package.manage_availability`, the
    // description `package.edit_metadata`.
    assert!(refused(
        api.set_version_yanked(pkg.id, "1.0.0", true)
            .await
            .map(|_| ())
    ));
    assert!(refused(
        api.patch_package(pkg.id, None, Some(true))
            .await
            .map(|_| ())
    ));
    assert!(refused(
        api.patch_package(pkg.id, Some("ours"), None)
            .await
            .map(|_| ())
    ));
    act_as(&state, "wbk");
    api.set_version_yanked(pkg.id, "1.0.0", true)
        .await
        .expect("an owner yanks");
    api.delete_version(pkg.id, "1.0.0")
        .await
        .expect("and retires it");
    // A retired number never returns, and no member, owners included, is shown it.
    let listed = api.list_versions(pkg.id).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|v| v.version.as_str())
            .collect::<Vec<_>>(),
        ["1.1.0"]
    );
    act_as(&state, "ann");
    assert!(matches!(
        publish_into(&api, pkg.id, "guild-maps", "1.0.0", &[]).await,
        Err(CloudError::VersionUnavailable(_))
    ));

    // Someone outside the clan meets the uniform 404 until it is public.
    act_as(&state, "stranger");
    assert!(refused(api.get_package(pkg.id).await.map(|_| ())));
    act_as(&state, "wbk");
    let public = api
        .patch_package(pkg.id, None, Some(true))
        .await
        .expect("an owner makes it public");
    assert!(public.is_public && public.is_clan_owned());
    act_as(&state, "stranger");
    assert!(api.get_package(pkg.id).await.is_ok());
    assert!(refused(
        publish_into(&api, pkg.id, "guild-maps", "2.0.0", &[]).await
    ));
}

#[tokio::test]
async fn ownerless_edges_publish_and_come_back_naming_the_targets_owner() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let clan = clan_with(&state, &["wbk"]);

    let lib = api
        .create_clan_package(clan, "guild-lib", "")
        .await
        .expect("clan lib");
    publish_into(&api, lib.id, "guild-lib", "1.2.0", &[])
        .await
        .expect("publish lib");
    publish_simple(&api, "util", "1.0.0", &[]).await;

    let app = api.create_package("app", "").await.expect("app");
    let mut edges = [
        // `smudgy:@guild-lib`: no owner on the wire.
        PublishDependency {
            owner_nickname: None,
            name: "guild-lib".to_string(),
            range: "^1".to_string(),
            resolved_version: "1.2.0".to_string(),
        },
        // A well-formed but different legacy owner must be rejected.
        PublishDependency {
            owner_nickname: Some("someone".to_string()),
            name: "util".to_string(),
            range: "^1".to_string(),
            resolved_version: "1.0.0".to_string(),
        },
    ];
    assert!(matches!(
        publish_into(&api, app.id, "app", "1.0.0", &edges).await,
        Err(CloudError::InvalidInput(_))
    ));
    edges[1].owner_nickname = Some("wbk".to_string());
    publish_into(&api, app.id, "app", "1.0.0", &edges)
        .await
        .expect("publish with ownerless and matching-owner edges");

    let resolved = api
        .resolve_package(None, "app", None)
        .await
        .expect("resolve app");
    let owner_of = |name: &str| {
        resolved
            .dependencies
            .iter()
            .find(|dep| dep.name == name)
            .map(|dep| dep.owner_nickname.clone())
    };
    assert_eq!(
        owner_of("guild-lib"),
        Some(None),
        "a clan target has no owner"
    );
    assert_eq!(
        owner_of("util"),
        Some(Some("wbk".to_string())),
        "an edge names its target's owner, not the segment it was published with"
    );

    // A malformed owner segment and an unknown target are refused at begin.
    let bad_owner = [PublishDependency {
        owner_nickname: Some("x".to_string()),
        name: "util".to_string(),
        range: "^1".to_string(),
        resolved_version: "1.0.0".to_string(),
    }];
    match publish_into(&api, app.id, "app", "1.0.1", &bad_owner).await {
        Err(CloudError::InvalidInput(message)) => {
            assert!(message.contains("invalid dependency handle"), "{message}");
        }
        other => panic!("a malformed edge owner must be a 400, got {other:?}"),
    }
    let unknown = [PublishDependency {
        owner_nickname: None,
        name: "nowhere".to_string(),
        range: "^1".to_string(),
        resolved_version: "1.0.0".to_string(),
    }];
    match publish_into(&api, app.id, "app", "1.0.1", &unknown).await {
        Err(CloudError::InvalidInput(message)) => {
            assert!(message.contains("unknown dependency: nowhere"), "{message}");
        }
        other => panic!("an unknown edge target must be a 400, got {other:?}"),
    }
}

#[tokio::test]
async fn check_updates_takes_ownerless_entries_and_have_rows() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let clan = clan_with(&state, &["wbk"]);

    let lib = api
        .create_clan_package(clan, "guild-lib", "")
        .await
        .expect("clan lib");
    publish_into(&api, lib.id, "guild-lib", "1.2.0", &[])
        .await
        .expect("publish lib");
    let app = api.create_package("app", "").await.expect("app");
    let edge = [PublishDependency {
        owner_nickname: None,
        name: "guild-lib".to_string(),
        range: "^1".to_string(),
        resolved_version: "1.2.0".to_string(),
    }];
    publish_into(&api, app.id, "app", "1.0.0", &edge)
        .await
        .expect("publish app");

    let entries = [
        // `smudgy:@app`
        CheckUpdatesEntry {
            owner: None,
            name: "app".to_string(),
            installed: Some("1.0.0".to_string()),
        },
        // A legacy user address cannot select a clan-owned package.
        CheckUpdatesEntry {
            owner: Some("wbk".to_string()),
            name: "guild-lib".to_string(),
            installed: None,
        },
        // A malformed owner is the uniform per-entry miss.
        CheckUpdatesEntry {
            owner: Some("?".to_string()),
            name: "app".to_string(),
            installed: None,
        },
    ];
    let response = api.check_updates(&entries, &[]).await.expect("check");
    let [app_result, lib_result, malformed] = response.results.as_slice() else {
        panic!("one result per entry: {:?}", response.results);
    };
    assert_eq!(
        app_result.owner, None,
        "an ownerless entry is echoed without one"
    );
    assert_eq!(app_result.status, "ok");
    let latest = app_result.latest.as_ref().expect("latest");
    assert_eq!(
        latest.dependencies[0].owner, None,
        "a clan edge has no owner"
    );
    assert_eq!(app_result.closure.len(), 1);
    assert_eq!(app_result.closure[0].name, "guild-lib");
    assert_eq!(
        app_result.closure[0].owner, None,
        "a clan node has no owner"
    );
    assert_eq!(
        lib_result.owner.as_deref(),
        Some("wbk"),
        "the owner is echoed"
    );
    assert_eq!(lib_result.status, "not_found");
    assert_eq!(malformed.status, "not_found");

    // A `have` row without an owner elides the node it names.
    let response = api
        .check_updates(
            &entries[..1],
            &[CheckUpdatesHave {
                owner: None,
                name: "GUILD-LIB".to_string(),
                version: "1.2.0".to_string(),
            }],
        )
        .await
        .expect("check with have");
    assert!(response.results[0].closure.is_empty());
}

// --- uploads refused during garbage collection ------------------------------

#[tokio::test]
async fn an_upload_refused_mid_collection_begins_the_publish_again() {
    for refusal in [CollectionRefusal::Conflict, CollectionRefusal::Internal] {
        let (base_url, state) = spawn_mock().await;
        let api = client(&base_url);
        let pkg = api.create_package("mapper", "").await.expect("create");
        {
            let mut st = state.lock().unwrap();
            st.collection_refusals = 1;
            st.collection_refusal = refusal;
        }
        publish_into(&api, pkg.id, "mapper", "1.0.0", &[])
            .await
            .expect("the retry from begin publishes");
        let st = state.lock().unwrap();
        assert_eq!(st.begins, 2, "the refused upload began the publish again");
        assert_eq!(st.bundle_uploads.len(), 1, "one upload was stored");
        assert_eq!(st.packages[0].versions.len(), 1);
    }
}

#[tokio::test]
async fn a_second_collection_refusal_surfaces_and_publishes_nothing() {
    let (base_url, state) = spawn_mock().await;
    let api = client(&base_url);
    let pkg = api.create_package("mapper", "").await.expect("create");
    state.lock().unwrap().collection_refusals = 2;
    match publish_into(&api, pkg.id, "mapper", "1.0.0", &[]).await {
        Err(CloudError::BodyBeingCollected) => {}
        other => panic!("a second refusal must surface, got {other:?}"),
    }
    let st = state.lock().unwrap();
    assert_eq!(st.begins, 2, "the publish began again once, and only once");
    assert!(st.bundle_uploads.is_empty());
    assert!(st.packages[0].versions.is_empty());
}
