//! Ownership transfer surface: `POST /areas|/atlases/{id}/transfer`,
//! `GET /transfers`, `POST /transfers/{id}/accept|decline`, `DELETE /transfers/{id}`.
//! Mirrors the deployed server's `TransferQueries` (in-memory, single-threaded — no
//! advisory locks / GUC / isolation needed). All verified-gated; transfer is raw
//! `is_owner` (never `can_admin`); on accept the former owner is always auto-granted
//! a fresh new-owner-minted `can_admin` share-back.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{authenticate, bad_request, err, gate_verified, not_found, ok};
use super::social::Shared;
use super::state::{GrantRecord, MockState, PendingTransferRecord};

fn gate(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (viewer, _) = authenticate(st, headers)?;
    gate_verified(st, viewer)?;
    Ok(viewer)
}

fn subject_owner(st: &MockState, area_id: Option<Uuid>, atlas_id: Option<Uuid>) -> Option<Uuid> {
    if let Some(a) = area_id {
        st.areas.get(&a).map(|r| r.user_id)
    } else if let Some(a) = atlas_id {
        st.atlases.get(&a).map(|r| r.user_id)
    } else {
        None
    }
}

/// Accepted friendship + no blocks either direction + BOTH verified.
fn transfer_gate(st: &MockState, from: Uuid, to: Uuid) -> bool {
    st.are_friends(from, to)
        && !st.blocked_pair(from, to)
        && st.email_verified(from)
        && st.email_verified(to)
}

fn transfer_json(st: &MockState, t: &PendingTransferRecord) -> Value {
    let subject_name = if let Some(a) = t.area_id {
        st.areas.get(&a).map(|r| r.name.clone())
    } else if let Some(a) = t.atlas_id {
        st.atlases.get(&a).map(|r| r.name.clone())
    } else {
        None
    };
    json!({
        "id": t.id,
        "subject_kind": t.subject_kind,
        "area_id": t.area_id,
        "atlas_id": t.atlas_id,
        "from_user_id": t.from_user_id,
        "to_user_id": t.to_user_id,
        "status": t.status,
        "created_at": t.created_at,
        "responded_at": t.responded_at,
        "expires_at": Value::Null,
        "subject_name": subject_name,
    })
}

#[derive(Deserialize)]
struct OfferBody {
    to_user_id: Uuid,
}

pub async fn create_area_transfer(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Ok(area_id) = Uuid::parse_str(&raw) else {
        return not_found();
    };
    create_transfer(&state, &headers, &body, Some(area_id), None)
}

pub async fn create_atlas_transfer(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Ok(atlas_id) = Uuid::parse_str(&raw) else {
        return not_found();
    };
    create_transfer(&state, &headers, &body, None, Some(atlas_id))
}

fn create_transfer(
    state: &Shared,
    headers: &HeaderMap,
    body: &str,
    area_id: Option<Uuid>,
    atlas_id: Option<Uuid>,
) -> Response {
    let mut st = state.lock();
    let caller = match gate(&st, headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: OfferBody = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return bad_request(&format!("Invalid JSON: {e}")),
    };

    // Raw is_owner only; subject must exist and be owned by the caller.
    match subject_owner(&st, area_id, atlas_id) {
        Some(o) if o == caller => {}
        _ => return not_found(),
    }
    if req.to_user_id == caller {
        return not_found();
    }
    if !transfer_gate(&st, caller, req.to_user_id) {
        return not_found();
    }
    // One live offer per subject -> 409.
    if st.pending_transfers.iter().any(|t| {
        t.status == "Offered" && t.area_id == area_id && t.atlas_id == atlas_id
    }) {
        return err(409, "transfer_already_pending");
    }

    let rec = PendingTransferRecord {
        id: Uuid::new_v4(),
        subject_kind: if area_id.is_some() { "area" } else { "atlas" }.to_string(),
        area_id,
        atlas_id,
        from_user_id: caller,
        to_user_id: req.to_user_id,
        status: "Offered".to_string(),
        created_at: Utc::now(),
        responded_at: None,
    };
    let view = transfer_json(&st, &rec);
    st.pending_transfers.push(rec);
    super::http::created(view)
}

pub async fn list_transfers(
    State(state): State<Shared>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    let caller = match gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let offered = match params.get("direction").map(String::as_str) {
        Some("offered") => true,
        Some("received") => false,
        _ => return bad_request("direction must be 'offered' or 'received'"),
    };
    let rows: Vec<Value> = st
        .pending_transfers
        .iter()
        .filter(|t| {
            t.status == "Offered"
                && if offered {
                    t.from_user_id == caller
                } else {
                    t.to_user_id == caller
                }
        })
        .map(|t| transfer_json(&st, t))
        .collect();
    ok(Value::Array(rows))
}

#[derive(Deserialize, Default)]
struct AcceptBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    atlas_id: Option<Uuid>,
}

pub async fn accept_transfer(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    let caller = match gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(transfer_id) = Uuid::parse_str(&raw) else {
        return not_found();
    };
    let req: AcceptBody = if body.trim().is_empty() {
        AcceptBody::default()
    } else {
        match serde_json::from_str(&body) {
            Ok(r) => r,
            Err(e) => return bad_request(&format!("Invalid JSON: {e}")),
        }
    };

    let Some(offer) = st
        .pending_transfers
        .iter()
        .find(|t| t.id == transfer_id && t.to_user_id == caller && t.status == "Offered")
        .cloned()
    else {
        return not_found();
    };
    let from = offer.from_user_id;
    let to = caller;
    let area_id = offer.area_id;
    let atlas_id = offer.atlas_id;

    // Re-validate: subject still owned by `from`, gate still holds.
    match subject_owner(&st, area_id, atlas_id) {
        Some(o) if o == from => {}
        _ => return not_found(),
    }
    if !transfer_gate(&st, from, to) {
        return not_found();
    }

    if let Some(subj) = area_id {
        // Cleanup + re-stamp the area-scoped grants.
        st.grants
            .retain(|g| !(g.area_id == Some(subj) && (g.grantee_id == to || g.can_admin)));
        for g in st.grants.iter_mut().filter(|g| g.area_id == Some(subj)) {
            g.owner_id = to;
        }
        // Refile into a caller-owned atlas if asked, else eject.
        let refile = req
            .atlas_id
            .filter(|a| st.atlases.get(a).is_some_and(|r| r.user_id == to));
        if let Some(area) = st.areas.get_mut(&subj) {
            area.user_id = to;
            area.atlas_id = refile;
            if let Some(n) = req.name.clone() {
                area.name = n;
            }
            area.rev += 1;
            area.public_rev += 1;
        }
    } else if let Some(subj) = atlas_id {
        let members: Vec<Uuid> = st
            .areas
            .values()
            .filter(|a| a.atlas_id == Some(subj))
            .map(|a| a.id)
            .collect();
        st.grants.retain(|g| {
            !((g.atlas_id == Some(subj) || g.area_id.is_some_and(|a| members.contains(&a)))
                && (g.grantee_id == to || g.can_admin))
        });
        for g in st.grants.iter_mut().filter(|g| {
            g.atlas_id == Some(subj) || g.area_id.is_some_and(|a| members.contains(&a))
        }) {
            g.owner_id = to;
        }
        if let Some(at) = st.atlases.get_mut(&subj) {
            at.user_id = to;
            if let Some(n) = req.name.clone() {
                at.name = n;
            }
        }
        for m in &members {
            if let Some(a) = st.areas.get_mut(m) {
                a.user_id = to;
                a.rev += 1;
                a.public_rev += 1;
            }
        }
    }

    // Auto-admin: the former owner becomes a can_admin deputy (new-owner-minted).
    st.grants.push(GrantRecord {
        id: Uuid::new_v4(),
        owner_id: to,
        grantor_id: to,
        grantee_id: from,
        area_id,
        atlas_id,
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        include_secrets: false,
        can_admin: true,
        parent_grant_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    });

    let view = {
        if let Some(t) = st.pending_transfers.iter_mut().find(|t| t.id == transfer_id) {
            t.status = "Accepted".to_string();
            t.responded_at = Some(Utc::now());
        }
        let t = st
            .pending_transfers
            .iter()
            .find(|t| t.id == transfer_id)
            .cloned()
            .unwrap();
        transfer_json(&st, &t)
    };
    ok(view)
}

pub async fn decline_transfer(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    respond_to_offer(&state, &headers, &raw, false)
}

pub async fn cancel_transfer(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    respond_to_offer(&state, &headers, &raw, true)
}

/// `decline` (recipient) or `cancel` (initiator) — mark the live offer terminal.
fn respond_to_offer(state: &Shared, headers: &HeaderMap, raw: &str, is_cancel: bool) -> Response {
    let mut st = state.lock();
    let caller = match gate(&st, headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(id) = Uuid::parse_str(raw) else {
        return not_found();
    };
    let Some(t) = st.pending_transfers.iter_mut().find(|t| {
        t.id == id
            && t.status == "Offered"
            && if is_cancel {
                t.from_user_id == caller
            } else {
                t.to_user_id == caller
            }
    }) else {
        return not_found();
    };
    t.status = if is_cancel { "Cancelled" } else { "Declined" }.to_string();
    t.responded_at = Some(Utc::now());
    ok(Value::Null)
}

/// Cancel any live offer between the pair, either direction — called
/// from unfriend/block. Mirrors the server's `cancel_live_transfers_between`.
pub fn cancel_live_transfers_between(st: &mut MockState, a: Uuid, b: Uuid) {
    for t in st.pending_transfers.iter_mut().filter(|t| {
        t.status == "Offered"
            && ((t.from_user_id == a && t.to_user_id == b)
                || (t.from_user_id == b && t.to_user_id == a))
    }) {
        t.status = "Cancelled".to_string();
        t.responded_at = Some(Utc::now());
    }
}
