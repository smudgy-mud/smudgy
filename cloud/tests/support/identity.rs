//! P1 — identity: /auth/* and /me* handlers.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::{Duration, Utc};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{
    CredKind, accepted, authenticate, bad_request, conflict, created, err, no_content, not_found,
    ok, parse_body,
};
use super::state::{
    API_KEY_PREFIX, ApiKeyRecord, EmailCodeRecord, MockState, SESSION_PREFIX, SessionRecord,
    UserRecord, gen_code, gen_token,
};

pub type Shared = Arc<Mutex<MockState>>;

// ---------------------------------------------------------------------------
// Validation (identity/validate.rs equivalents).
// ---------------------------------------------------------------------------

fn valid_email(email: &str) -> bool {
    if email.len() > 255 {
        return false;
    }
    let mut parts = email.splitn(2, '@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

fn valid_nickname(nickname: &str) -> bool {
    (3..=24).contains(&nickname.len())
        && nickname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Mirror the server's `validate::normalize_email` (trim + lowercase) so the
/// email stored and returned by the mock matches the server regardless of the
/// casing or surrounding whitespace the caller sent.
fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn profile_json(user: &UserRecord) -> Value {
    json!({
        "id": user.id,
        "email": user.email,
        "nickname": user.nickname,
        "requested_nickname": user.requested_nickname,
        "email_verified_at": user.email_verified_at,
        "nickname_updated_at": user.nickname_updated_at,
        "created_at": user.created_at,
    })
}

// ---------------------------------------------------------------------------
// /auth/*
// ---------------------------------------------------------------------------

/// Supersede any open code for `user_id` (marked consumed, not deleted),
/// then mint and store a fresh one — `issue_and_send_code` sans the email.
fn issue_code(st: &mut MockState, user_id: Uuid) {
    for c in st.email_codes.iter_mut().filter(|c| c.user_id == user_id) {
        c.consumed = true;
    }
    st.email_codes.push(EmailCodeRecord {
        code: gen_code(),
        user_id,
        consumed: false,
    });
}

#[derive(Deserialize)]
struct SignupRequest {
    email: String,
    nickname: String,
}

/// POST /auth/signup — 202 always (enumeration-flat); 400 only on format.
/// Insert-only: an existing email is never touched (it gets the
/// account-exists notice instead of a code).
pub async fn signup(State(state): State<Shared>, body: String) -> Response {
    let req: SignupRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if !valid_email(&req.email) || !valid_nickname(&req.nickname) {
        return bad_request("Invalid signup input");
    }

    let email = normalize_email(&req.email);
    let mut st = state.lock();
    if st.user_by_email(&email).is_none() {
        let user_id = Uuid::new_v4();
        st.users.push(UserRecord {
            id: user_id,
            email,
            nickname: None,
            requested_nickname: Some(req.nickname),
            email_verified_at: None,
            nickname_updated_at: None,
            created_at: Utc::now(),
        });
        issue_code(&mut st, user_id);
    }
    accepted()
}

#[derive(Deserialize)]
struct EmailOnlyRequest {
    email: String,
}

/// POST /auth/login — 202 always (enumeration-flat). The unified passwordless
/// entry: mails a sign-in code, **creating the account on first sight** (a
/// nickname-less row — verify resolves that to `needs_nickname`) so one
/// email-only call serves new and returning users. Insert-only — an existing
/// row is never touched. Doubles as the resend path (supersedes any open code).
pub async fn login(State(state): State<Shared>, body: String) -> Response {
    let req: EmailOnlyRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if !valid_email(&req.email) {
        return bad_request("Invalid email");
    }

    let email = normalize_email(&req.email);
    let mut st = state.lock();
    let user_id = match st.user_by_email(&email).map(|u| u.id) {
        Some(id) => id,
        None => {
            let user_id = Uuid::new_v4();
            st.users.push(UserRecord {
                id: user_id,
                email,
                nickname: None,
                requested_nickname: None,
                email_verified_at: None,
                nickname_updated_at: None,
                created_at: Utc::now(),
            });
            user_id
        }
    };
    issue_code(&mut st, user_id);
    accepted()
}

#[derive(Deserialize)]
struct VerifyEmailRequest {
    email: String,
    code: String,
}

/// POST /auth/verify-email — consume the code, mint a session. The first
/// verify allocates the handle + marks the email verified; a returning user
/// just gets the session. Unknown email and wrong/expired code are the same
/// uniform 404.
pub async fn verify_email(State(state): State<Shared>, body: String) -> Response {
    let req: VerifyEmailRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if !valid_email(&req.email) {
        return not_found();
    }

    let email = normalize_email(&req.email);
    let mut st = state.lock();
    let Some((user_id, already_verified)) = st
        .user_by_email(&email)
        .map(|u| (u.id, u.email_verified_at.is_some()))
    else {
        return not_found();
    };

    {
        let Some(code) = st
            .email_codes
            .iter_mut()
            .find(|c| c.user_id == user_id && c.code == req.code && !c.consumed)
        else {
            return not_found();
        };
        code.consumed = true;
    }

    let mut needs_nickname = false;
    if !already_verified {
        let requested = st.user(user_id).and_then(|u| u.requested_nickname.clone());
        needs_nickname = match requested {
            // The explicit signup path requested a handle: claim it (a collision
            // leaves it unallocated and prompts for another).
            Some(nick) => !st.claim_nickname(user_id, &nick),
            // The unified email-only path requested none: verify the email but
            // leave the nickname unallocated, prompting the user post-sign-in.
            None => true,
        };
        let Some(user) = st.user_mut(user_id) else {
            return not_found();
        };
        user.email_verified_at = Some(Utc::now());
    }

    let session_token = gen_token(SESSION_PREFIX);
    st.sessions.insert(
        session_token.clone(),
        SessionRecord {
            id: Uuid::new_v4(),
            user_id,
            created_at: Utc::now(),
            expires_at: Utc::now() + Duration::days(365),
            last_used_at: None,
        },
    );

    let user = st.user(user_id).expect("user exists");
    let mut data = json!({
        "session_token": session_token,
        "user": profile_json(user),
    });
    // `needs_nickname` is OMITTED when false (skip_serializing_if).
    if needs_nickname {
        data["needs_nickname"] = json!(true);
    }
    ok(data)
}

/// POST /auth/logout — session-shaped credential only; idempotent 204.
pub async fn logout(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let Some(raw) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return err(401, "Missing or invalid credentials");
    };
    let token = raw.strip_prefix("Bearer ").unwrap_or(raw);
    if !token.starts_with(SESSION_PREFIX) {
        return err(401, "Session authentication required");
    }
    state.lock().sessions.remove(token);
    no_content()
}

/// POST /auth/refresh — session-only; slide the presented session's idle
/// deadline to `now() + 365 days`, stamp `last_used_at`, and return the
/// updated SessionView row. An unknown/expired token is 401 (mirrors the
/// server: an expired session can't be revived here, only re-minted via
/// verify-email). Mirrors `SessionQueries::refresh_session`.
pub async fn refresh_session(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let Some(raw) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return err(401, "Missing or invalid credentials");
    };
    let token = raw.strip_prefix("Bearer ").unwrap_or(raw);
    if !token.starts_with(SESSION_PREFIX) {
        return err(401, "Session authentication required");
    }

    let now = Utc::now();
    let mut st = state.lock();
    let Some(session) = st.sessions.get_mut(token).filter(|s| s.expires_at > now) else {
        return err(401, "Invalid session");
    };
    session.expires_at = now + Duration::days(365);
    session.last_used_at = Some(now);
    ok(json!({
        "id": session.id,
        "created_at": session.created_at,
        "last_used_at": session.last_used_at,
        "expires_at": session.expires_at,
    }))
}

// ---------------------------------------------------------------------------
// /me*
// ---------------------------------------------------------------------------

/// GET /me — full profile (the only place email is serialized).
pub async fn get_me(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    let (user_id, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    match st.user(user_id) {
        Some(user) => ok(profile_json(user)),
        None => not_found(),
    }
}

#[derive(Deserialize)]
struct PatchMeRequest {
    nickname: Option<String>,
}

/// PATCH /me — set a new globally-unique nickname; no cooldown.
pub async fn patch_me(State(state): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let mut st = state.lock();
    let (user_id, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: PatchMeRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };

    if let Some(nickname) = req.nickname {
        if !valid_nickname(&nickname) {
            return bad_request("Invalid nickname");
        }
        if !st.claim_nickname(user_id, &nickname) {
            return conflict("That nickname is taken; please choose another.");
        }
        // Mirror the server's `set_nickname`: PATCH /me updates only the handle
        // (`nickname`, via `claim_nickname` above) and its timestamp — it leaves
        // `requested_nickname` (set at account creation) untouched.
    }
    match st.user(user_id) {
        Some(user) => ok(profile_json(user)),
        None => not_found(),
    }
}

/// GET /me/api-keys — metadata only, created_at DESC, never key material.
pub async fn list_api_keys(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    let (user_id, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut keys: Vec<&ApiKeyRecord> = st
        .api_keys
        .values()
        .filter(|k| k.user_id == user_id)
        .collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.created_at));
    let rows: Vec<Value> = keys
        .into_iter()
        .map(|k| {
            json!({
                "id": k.id,
                "key_suffix": k.key_suffix,
                "created_at": k.created_at,
                "last_used_at": k.last_used_at,
            })
        })
        .collect();
    ok(json!(rows))
}

/// POST /me/api-keys — SESSION-ONLY; full key material shown exactly once.
pub async fn create_api_key(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let mut st = state.lock();
    let (user_id, kind) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if kind != CredKind::Session {
        return err(401, "Session authentication required");
    }

    let api_key = gen_token(API_KEY_PREFIX);
    let key_suffix: String = api_key.chars().skip(api_key.len() - 8).collect();
    let record = ApiKeyRecord {
        id: Uuid::new_v4(),
        user_id,
        key_suffix: key_suffix.clone(),
        created_at: Utc::now(),
        last_used_at: None,
    };
    let response = json!({
        "id": record.id,
        "api_key": api_key,
        "key_suffix": key_suffix,
        "created_at": record.created_at,
    });
    st.api_keys.insert(api_key.clone(), record);
    created(response)
}

/// DELETE /me/api-keys/{id} — 204 on delete; 404 when not the caller's.
pub async fn delete_api_key(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(key_id) = Uuid::parse_str(&raw_id) else {
        return bad_request(&format!("Invalid key ID: {raw_id}"));
    };
    let mut st = state.lock();
    let (user_id, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let token = st
        .api_keys
        .iter()
        .find(|(_, k)| k.id == key_id && k.user_id == user_id)
        .map(|(t, _)| t.clone());
    match token {
        Some(token) => {
            st.api_keys.remove(&token);
            no_content()
        }
        None => not_found(),
    }
}

/// GET /me/sessions — unexpired sessions, metadata only.
pub async fn list_sessions(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    let (user_id, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let now = Utc::now();
    let mut sessions: Vec<&SessionRecord> = st
        .sessions
        .values()
        .filter(|s| s.user_id == user_id && s.expires_at > now)
        .collect();
    // last_used_at DESC NULLS LAST, then created_at DESC as a stable fallback.
    sessions.sort_by(|a, b| {
        b.last_used_at
            .cmp(&a.last_used_at)
            .then(b.created_at.cmp(&a.created_at))
    });
    let rows: Vec<Value> = sessions
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "created_at": s.created_at,
                "last_used_at": s.last_used_at,
                "expires_at": s.expires_at,
            })
        })
        .collect();
    ok(json!(rows))
}

/// DELETE /me/sessions/{id} — 204 on delete; 404 when not the caller's.
pub async fn delete_session(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(session_id) = Uuid::parse_str(&raw_id) else {
        return bad_request(&format!("Invalid session ID: {raw_id}"));
    };
    let mut st = state.lock();
    let (user_id, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let token = st
        .sessions
        .iter()
        .find(|(_, s)| s.id == session_id && s.user_id == user_id)
        .map(|(t, _)| t.clone());
    match token {
        Some(token) => {
            st.sessions.remove(&token);
            no_content()
        }
        None => not_found(),
    }
}
