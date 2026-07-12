//! P3 — social surface: /users/lookup, /friends*, /blocks*. All verified-
//! gated. Mirrors `social/handlers.rs` + the db.rs friend state machine:
//! enumeration-flat 202s, shadow-pendings, block cascades over share grants.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{
    accepted, authenticate, gate_verified, no_content, not_found, ok, parse_body, parse_social_id,
};
use super::state::{BlockRecord, FriendStatus, FriendshipRecord, MockState};

pub type Shared = Arc<Mutex<MockState>>;

/// Auth + verified gate shared by every social handler.
fn social_gate(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (viewer, _) = authenticate(st, headers)?;
    gate_verified(st, viewer)?;
    Ok(viewer)
}

// ---------------------------------------------------------------------------
// GET /users/lookup?handle=<nickname>
// ---------------------------------------------------------------------------

/// Exact nickname lookup (case-insensitive); the handle is the globally-unique
/// nickname. Uniform 404 for miss AND malformed. No block oracle — blocks are
/// ignored.
pub async fn lookup(
    State(state): State<Shared>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    if let Err(e) = social_gate(&st, &headers) {
        return e;
    }

    let Some(nick) = params.get("nickname").map(|h| h.trim()).filter(|h| valid_nickname(h)) else {
        return not_found();
    };
    match st.users.iter().find(|u| {
        u.nickname
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(nick))
    }) {
        Some(user) => ok(json!({
            "user_id": user.id,
            "nickname": user.nickname,
        })),
        None => not_found(),
    }
}

/// Nickname charset/length: `^[A-Za-z0-9_-]{3,24}$` (mirrors the server).
fn valid_nickname(s: &str) -> bool {
    let len = s.chars().count();
    (3..=24).contains(&len)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ---------------------------------------------------------------------------
// /friends*
// ---------------------------------------------------------------------------

/// GET /friends — Accepted pairs either side; since = responded_at||created_at.
pub async fn list_friends(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut rows: Vec<(chrono::DateTime<Utc>, Value)> = st
        .friendships
        .iter()
        .filter(|f| {
            f.status == FriendStatus::Accepted
                && (f.requester_id == viewer || f.addressee_id == viewer)
        })
        .map(|f| {
            let other = if f.requester_id == viewer {
                f.addressee_id
            } else {
                f.requester_id
            };
            let user = st.user(other);
            let since = f.responded_at.unwrap_or(f.created_at);
            (
                since,
                json!({
                    "user_id": other,
                    "nickname": user.and_then(|u| u.nickname.clone()),
                    "since": since,
                }),
            )
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    let rows: Vec<Value> = rows.into_iter().map(|(_, v)| v).collect();
    ok(json!(rows))
}

/// GET /friends/requests — incoming hides blocked pairs (shadow-pendings);
/// outgoing shows shadow-pendings as normal pendings.
pub async fn list_requests(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let request_row = |user_id: Uuid, f: &FriendshipRecord| {
        let user = st.user(user_id);
        json!({
            "user_id": user_id,
            "nickname": user.and_then(|u| u.nickname.clone()),
            "created_at": f.created_at,
        })
    };

    let mut incoming: Vec<(chrono::DateTime<Utc>, Value)> = st
        .friendships
        .iter()
        .filter(|f| {
            f.status == FriendStatus::Pending
                && f.addressee_id == viewer
                && !st.blocked_pair(f.requester_id, f.addressee_id)
        })
        .map(|f| (f.created_at, request_row(f.requester_id, f)))
        .collect();
    incoming.sort_by_key(|row| std::cmp::Reverse(row.0));

    let mut outgoing: Vec<(chrono::DateTime<Utc>, Value)> = st
        .friendships
        .iter()
        .filter(|f| f.status == FriendStatus::Pending && f.requester_id == viewer)
        .map(|f| (f.created_at, request_row(f.addressee_id, f)))
        .collect();
    outgoing.sort_by_key(|row| std::cmp::Reverse(row.0));

    let incoming: Vec<Value> = incoming.into_iter().map(|(_, v)| v).collect();
    let outgoing: Vec<Value> = outgoing.into_iter().map(|(_, v)| v).collect();
    ok(json!({"incoming": incoming, "outgoing": outgoing}))
}

#[derive(Deserialize)]
struct FriendRequestBody {
    user_id: Uuid,
}

/// POST /friends/requests — 202 ALWAYS. Auto-accept the inverse pending when
/// unblocked; insert a (possibly shadow) pending when no pair row exists.
pub async fn send_request(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: FriendRequestBody = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let target = req.user_id;

    if target == viewer || st.user(target).is_none() {
        return accepted();
    }
    let blocked = st.blocked_pair(viewer, target);
    let pair_exists = st.friendships.iter().any(|f| {
        (f.requester_id == viewer && f.addressee_id == target)
            || (f.requester_id == target && f.addressee_id == viewer)
    });

    // Auto-accept the inverse pending (target -> viewer) when not blocked.
    if !blocked
        && let Some(f) = st.friendships.iter_mut().find(|f| {
            f.requester_id == target
                && f.addressee_id == viewer
                && f.status == FriendStatus::Pending
        })
    {
        f.status = FriendStatus::Accepted;
        f.responded_at = Some(Utc::now());
    }
    // Insert a viewer -> target Pending when NO pair row exists (this is the
    // shadow-pending under a block).
    if !pair_exists {
        st.friendships.push(FriendshipRecord {
            requester_id: viewer,
            addressee_id: target,
            status: FriendStatus::Pending,
            created_at: Utc::now(),
            responded_at: None,
        });
    }
    accepted()
}

/// POST /friends/requests/{requester_id}/accept — addressee-only; refuses
/// (uniform 404) when no real pending or any block exists.
pub async fn accept_request(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(requester) = parse_social_id(&raw_id) else {
        return not_found();
    };
    if st.blocked_pair(viewer, requester) {
        return not_found();
    }
    let flipped = st.friendships.iter_mut().find(|f| {
        f.requester_id == requester && f.addressee_id == viewer && f.status == FriendStatus::Pending
    });
    match flipped {
        Some(f) => {
            f.status = FriendStatus::Accepted;
            f.responded_at = Some(Utc::now());
            no_content()
        }
        None => not_found(),
    }
}

/// DELETE /friends/requests/{user_id} — decline or cancel; idempotent 204.
pub async fn delete_request(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(other) = parse_social_id(&raw_id) else {
        return not_found();
    };
    st.friendships.retain(|f| {
        !(f.status == FriendStatus::Pending
            && ((f.requester_id == viewer && f.addressee_id == other)
                || (f.requester_id == other && f.addressee_id == viewer)))
    });
    no_content()
}

/// DELETE /friends/{user_id} — unfriend; deletes the pair's grants in BOTH
/// directions (subtrees cascade). Idempotent 204.
pub async fn unfriend(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(other) = parse_social_id(&raw_id) else {
        return not_found();
    };
    st.friendships.retain(|f| {
        !(f.status == FriendStatus::Accepted
            && ((f.requester_id == viewer && f.addressee_id == other)
                || (f.requester_id == other && f.addressee_id == viewer)))
    });
    let doomed: Vec<Uuid> = st
        .grants
        .iter()
        .filter(|g| {
            (g.grantor_id == viewer && g.grantee_id == other)
                || (g.grantor_id == other && g.grantee_id == viewer)
        })
        .map(|g| g.id)
        .collect();
    st.delete_grants_cascading(&doomed);
    super::transfers::cancel_live_transfers_between(&mut st, viewer, other);
    no_content()
}

// ---------------------------------------------------------------------------
// /blocks*
// ---------------------------------------------------------------------------

/// PUT /blocks/{user_id} — idempotent; deletes the pair friendship, the pair
/// grants both directions, AND the owner-wide grant cascade both directions.
pub async fn block(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(target) = parse_social_id(&raw_id) else {
        return not_found();
    };
    if target == viewer {
        return no_content(); // self-block is a no-op 204
    }

    st.friendships.retain(|f| {
        !((f.requester_id == viewer && f.addressee_id == target)
            || (f.requester_id == target && f.addressee_id == viewer))
    });
    let doomed: Vec<Uuid> = st
        .grants
        .iter()
        .filter(|g| {
            (g.grantor_id == viewer && g.grantee_id == target)
                || (g.grantor_id == target && g.grantee_id == viewer)
                || (g.owner_id == viewer && g.grantee_id == target)
                || (g.owner_id == target && g.grantee_id == viewer)
        })
        .map(|g| g.id)
        .collect();
    st.delete_grants_cascading(&doomed);
    super::transfers::cancel_live_transfers_between(&mut st, viewer, target);

    let already = st
        .blocks
        .iter()
        .any(|b| b.blocker_id == viewer && b.blocked_id == target);
    if !already {
        st.blocks.push(BlockRecord {
            blocker_id: viewer,
            blocked_id: target,
            created_at: Utc::now(),
        });
    }
    no_content()
}

/// DELETE /blocks/{user_id} — deletes the block row only; restores nothing.
pub async fn unblock(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(target) = parse_social_id(&raw_id) else {
        return not_found();
    };
    st.blocks
        .retain(|b| !(b.blocker_id == viewer && b.blocked_id == target));
    no_content()
}

/// GET /blocks — the caller's own blocks with handles, created_at DESC.
pub async fn list_blocks(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    let viewer = match social_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut rows: Vec<(chrono::DateTime<Utc>, Value)> = st
        .blocks
        .iter()
        .filter(|b| b.blocker_id == viewer)
        .map(|b| {
            let user = st.user(b.blocked_id);
            (
                b.created_at,
                json!({
                    "user_id": b.blocked_id,
                    "nickname": user.and_then(|u| u.nickname.clone()),
                    "created_at": b.created_at,
                }),
            )
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    let rows: Vec<Value> = rows.into_iter().map(|(_, v)| v).collect();
    ok(json!(rows))
}
