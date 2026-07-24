//! Response envelope, error taxonomy, and credential dispatch — mirrors
//! `models.rs` (`ApiResponse`/`ApiError`) and `auth.rs` of the real server.

use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use semver::Version;
use serde_json::{Value, json};
use uuid::Uuid;

use super::state::{MockState, SESSION_PREFIX};

/// `{"success":true,"data":…,"error":null}` at the given status.
pub fn envelope(status: u16, data: Value) -> Response {
    let code = StatusCode::from_u16(status).expect("valid status");
    (
        code,
        axum::Json(json!({"success": true, "data": data, "error": null})),
    )
        .into_response()
}

pub fn ok(data: Value) -> Response {
    envelope(200, data)
}

pub fn created(data: Value) -> Response {
    envelope(201, data)
}

/// The enumeration-flat `202 {status:"accepted"}` body.
pub fn accepted() -> Response {
    envelope(202, json!({"status": "accepted"}))
}

/// Empty-body 204 (no envelope).
pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// `{"success":false,"data":null,"error":msg}` at the given status.
pub fn err(status: u16, msg: &str) -> Response {
    let code = StatusCode::from_u16(status).expect("valid status");
    (
        code,
        axum::Json(json!({"success": false, "data": null, "error": msg})),
    )
        .into_response()
}

/// `{"success":false,"data":null,"error":code,"details":{…}}` — the CAS
/// conflict shape (`api_error_response` attaching `ApiError::details`).
pub fn err_with_details(status: u16, msg: &str, details: Value) -> Response {
    let code = StatusCode::from_u16(status).expect("valid status");
    (
        code,
        axum::Json(json!({
            "success": false,
            "data": null,
            "error": msg,
            "details": details,
        })),
    )
        .into_response()
}

/// The single uniform 404 used for every resource/validator denial.
pub fn not_found() -> Response {
    err(404, "Not found")
}

/// 403 with the machine code the verified-email gate emits.
pub fn email_not_verified() -> Response {
    err(403, "email_not_verified")
}

pub fn bad_request(msg: &str) -> Response {
    err(400, msg)
}

/// 409 used when a globally-unique nickname is already taken.
pub fn conflict(msg: &str) -> Response {
    err(409, msg)
}

/// Handlers use `Result<Response, Response>` so denials can use `?`.
pub type Handled = Result<Response, Response>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredKind {
    Session,
    ApiKey,
}

/// Authorization-header dispatch: optional `Bearer ` strip, then
/// `smudgy_sess_` prefix -> sessions, anything else -> api keys.
pub fn authenticate(state: &MockState, headers: &HeaderMap) -> Result<(Uuid, CredKind), Response> {
    let raw = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| err(401, "Missing or invalid credentials"))?;
    let token = raw.strip_prefix("Bearer ").unwrap_or(raw);
    if token.starts_with(SESSION_PREFIX) {
        state
            .sessions
            .get(token)
            .filter(|s| s.expires_at > Utc::now())
            .map(|s| (s.user_id, CredKind::Session))
            .ok_or_else(|| err(401, "Invalid session"))
    } else {
        state
            .api_keys
            .get(token)
            .map(|k| (k.user_id, CredKind::ApiKey))
            .ok_or_else(|| err(401, "Invalid API key"))
    }
}

/// Mirrors the server's `client_below_floor` gate (`http_handler.rs`): reject a
/// client older than `state.min_client_version` with a 426 `client_upgrade_required`.
/// A GUI/session request with a missing/garbled version is treated as ancient;
/// other callers without a version are left alone. A `None` floor (the default
/// for every test) never rejects.
pub fn client_upgrade_rejection(state: &MockState, headers: &HeaderMap) -> Option<Response> {
    let min = Version::parse(state.min_client_version.as_deref()?).ok()?;

    let claimed = headers
        .get("x-smudgy-client-version")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| Version::parse(raw.trim()).ok());

    let is_gui = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.strip_prefix("Bearer ").unwrap_or(h))
        .is_some_and(|c| c.starts_with(SESSION_PREFIX));

    let effective = match claimed {
        Some(v) => Some(v),
        None if is_gui => Some(Version::new(0, 0, 0)),
        None => None,
    };

    match effective {
        Some(v) if v < min => Some(err(426, "client_upgrade_required")),
        _ => None,
    }
}

/// Mirrors the server's `upgrade_target`: the newest version to advertise via
/// the `x-smudgy-upgrade-available` header when the (parseable) client is
/// allowed but behind — `min <= client < newest`. `None` disables the hint.
pub fn upgrade_available_for(state: &MockState, headers: &HeaderMap) -> Option<String> {
    let newest = Version::parse(state.newest_client_version.as_deref()?).ok()?;
    if newest == Version::new(0, 0, 0) {
        return None;
    }
    let min = state
        .min_client_version
        .as_deref()
        .and_then(|s| Version::parse(s).ok())
        .unwrap_or_else(|| Version::new(0, 0, 0));
    let client = headers
        .get("x-smudgy-client-version")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Version::parse(s.trim()).ok())?;
    (client >= min && client < newest).then(|| newest.to_string())
}

/// Verified-email gate: 403 `email_not_verified` for unverified/missing users.
pub fn gate_verified(state: &MockState, user_id: Uuid) -> Result<(), Response> {
    if state.email_verified(user_id) {
        Ok(())
    } else {
        Err(email_not_verified())
    }
}

/// Parse a JSON body; mirrors the enveloped 400 of the clean handlers.
pub fn parse_body<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, Response> {
    serde_json::from_str(body).map_err(|e| bad_request(&format!("Invalid JSON: {e}")))
}

/// Path-param uuid for /areas routes: malformed -> 400 `Invalid map ID: …`.
pub fn parse_area_id(raw: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(raw).map_err(|e| bad_request(&format!("Invalid map ID: {e}")))
}

/// Path-param uuid for social/share routes: malformed -> uniform 404.
pub fn parse_social_id(raw: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(raw).map_err(|_| not_found())
}
