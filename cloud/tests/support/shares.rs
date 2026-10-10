//! P3 — share grants: POST/GET /shares, PATCH/DELETE /shares/{id},
//! GET /areas/{id}/shares. Mirrors `ShareQueries` in the real db.rs: the
//! friendship/block validator, covering-parent inference, depth-1 children,
//! clamp/delete cascades, the reaching-grant tree. Every denial -> 404.
//!
//! On a clan's Clan-owned map the same routes make outside shares
//! (docs/clans.md §5.5): view only, of one map, never of its Secrets, by a
//! member holding `area.share_external`, lasting while the sharer holds it.

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
    authenticate, bad_request, created, gate_verified, not_found, ok, parse_area_id, parse_body,
    parse_social_id,
};
use super::state::{GrantRecord, MockState};

pub type Shared = Arc<Mutex<MockState>>;

fn share_gate(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (viewer, _) = authenticate(st, headers)?;
    gate_verified(st, viewer)?;
    Ok(viewer)
}

pub fn grant_json(g: &GrantRecord) -> Value {
    let mut out = json!({
        "id": g.id,
        "owner_id": g.owner_id,
        "grantor_id": g.grantor_id,
        "grantee_id": g.grantee_id,
        "area_id": g.area_id,
        "atlas_id": g.atlas_id,
        "can_edit": g.can_edit,
        "can_reshare": g.can_reshare,
        "can_copy": g.can_copy,
        // No longer a flag; rows always carry `false`.
        "include_secrets": false,
        "can_admin": g.can_admin,
        "parent_grant_id": g.parent_grant_id,
        "created_at": g.created_at,
        "updated_at": g.updated_at,
    });
    // Advisory host hints, snapshotted at creation (§4.2); key omitted when the
    // grant carried none (skip-when-none), matching `ShareGrant`'s serde.
    if let Some(hints) = &g.host_hints {
        out["host_hints"] = json!(hints);
    }
    out
}

/// The `validate_grant_write` invariant set: friendship + blocks + child
/// flags <= the SPECIFIC parent's stored caps. Uniform 404 on any failure.
#[allow(clippy::too_many_arguments)]
fn validate_grant_write(
    st: &MockState,
    grantor_id: Uuid,
    grantee_id: Uuid,
    owner_id: Uuid,
    parent_grant_id: Option<Uuid>,
    can_edit: bool,
    can_reshare: bool,
    can_copy: bool,
    can_admin: bool,
) -> Result<(), Response> {
    if grantee_id == owner_id {
        return Err(not_found());
    }
    if !st.are_friends(grantor_id, grantee_id)
        || st.blocked_pair(grantor_id, grantee_id)
        || st.blocked_pair(owner_id, grantee_id)
    {
        return Err(not_found());
    }
    if let Some(pid) = parent_grant_id {
        let Some(parent) = st.grant(pid) else {
            return Err(not_found());
        };
        if parent.owner_id != owner_id
            || parent.grantee_id != grantor_id
            || !parent.can_reshare
            || (can_edit && !parent.can_edit)
            || (can_copy && !parent.can_copy)
            || can_reshare
            || can_admin
        {
            return Err(not_found());
        }
    }
    Ok(())
}

/// `find_covering_parent`: one of the caller's OWN grants with reshare that
/// covers the child scope and carries at least the requested edit/copy.
/// Prefers an exact Area parent over an Atlas one.
fn find_covering_parent(
    st: &MockState,
    caller: Uuid,
    owner: Uuid,
    child_area: Option<Uuid>,
    child_atlas: Option<Uuid>,
    req_can_edit: bool,
    req_can_copy: bool,
) -> Option<Uuid> {
    let area_atlas = child_area
        .and_then(|a| st.areas.get(&a))
        .and_then(|a| a.atlas_id);
    let mut candidates: Vec<&GrantRecord> = st
        .grants
        .iter()
        .filter(|g| {
            g.grantee_id == caller
                && g.owner_id == owner
                && g.can_reshare
                && (g.can_edit || !req_can_edit)
                && (g.can_copy || !req_can_copy)
                && ((child_area.is_some() && g.area_id == child_area)
                    || (child_area.is_some() && g.atlas_id.is_some() && g.atlas_id == area_atlas)
                    || (child_atlas.is_some() && g.atlas_id == child_atlas))
        })
        .collect();
    // ORDER BY area_id NULLS LAST: exact Area parent preferred.
    candidates.sort_by_key(|g| g.area_id.is_none());
    candidates.first().map(|g| g.id)
}

/// Whether `caller` may PATCH/DELETE the grant: grantor, owner, or an
/// ancestor grantor up the parent chain.
fn caller_may_manage(st: &MockState, caller: Uuid, grant: &GrantRecord) -> bool {
    if caller == grant.grantor_id || caller == grant.owner_id {
        return true;
    }
    let mut cursor = grant.parent_grant_id;
    while let Some(pid) = cursor {
        let Some(parent) = st.grant(pid) else { break };
        if parent.grantor_id == caller {
            return true;
        }
        cursor = parent.parent_grant_id;
    }
    false
}

fn grant_depth(st: &MockState, grant: &GrantRecord) -> i32 {
    let mut depth = 0;
    let mut cursor = grant.parent_grant_id;
    while let Some(pid) = cursor {
        depth += 1;
        cursor = st.grant(pid).and_then(|p| p.parent_grant_id);
    }
    depth
}

// ---------------------------------------------------------------------------
// POST /shares
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ScopeBody {
    area_id: Option<Uuid>,
    atlas_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct CreateShareRequest {
    grantee_id: Uuid,
    scope: ScopeBody,
    #[serde(default)]
    can_edit: bool,
    #[serde(default)]
    can_reshare: bool,
    #[serde(default)]
    can_copy: bool,
    /// No longer a share flag: `true` is a 400, `false` is accepted.
    #[serde(default)]
    include_secrets: bool,
    #[serde(default)]
    can_admin: bool,
    /// Grantor-authored advisory host hints (§4.2). Absent = none.
    #[serde(default)]
    host_hints: Option<Vec<String>>,
}

/// The 400 for an `include_secrets: true` share or patch: Secrets are shared
/// one at a time, and silently dropping the flag would quietly withhold what
/// the sharer meant to share.
const INCLUDE_SECRETS: &str =
    "include_secrets is no longer a share flag; share each Secret through /secrets/{id}/grants";

/// Maximum number of host hints a single share may carry (mirrors the server's
/// `MAX_HOST_HINTS`).
const MAX_HOST_HINTS: usize = 32;
/// Maximum byte length of a single host hint (mirrors `MAX_HOST_HINT_LEN`).
const MAX_HOST_HINT_LEN: usize = 255;

/// Normalize a grantor-supplied `host_hints` payload for storage: trim each
/// entry and drop empties, returning `None` when the payload is absent or
/// reduces to nothing. Rejects (uniform 400) more than `MAX_HOST_HINTS` entries
/// or any entry longer than `MAX_HOST_HINT_LEN` bytes — mirrors the real
/// `normalize_host_hints` in db.rs (length is checked before trimming empties;
/// the `"host"`/`"host:port"` format itself is NOT validated).
fn normalize_host_hints(hints: Option<&[String]>) -> Result<Option<Vec<String>>, Response> {
    let Some(hints) = hints else {
        return Ok(None);
    };
    if hints.len() > MAX_HOST_HINTS {
        return Err(bad_request("at most 32 host hints are allowed"));
    }
    let mut out = Vec::with_capacity(hints.len());
    for h in hints {
        let trimmed = h.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() > MAX_HOST_HINT_LEN {
            return Err(bad_request("each host hint must be at most 255 characters"));
        }
        out.push(trimmed.to_string());
    }
    Ok(if out.is_empty() { None } else { Some(out) })
}

/// POST /shares — create or idempotently re-grant; ALL denials uniform 404.
pub async fn create_share(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    let caller = match share_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: CreateShareRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if req.include_secrets {
        return bad_request(INCLUDE_SECRETS);
    }
    // Untagged scope: the Area variant is tried first, so area_id wins.
    let (area_id, atlas_id) = match (req.scope.area_id, req.scope.atlas_id) {
        (Some(a), _) => (Some(a), None),
        (None, Some(t)) => (None, Some(t)),
        (None, None) => return bad_request("Invalid JSON: unknown share scope"),
    };

    if let Some(aid) = area_id
        && st
            .areas
            .get(&aid)
            .is_some_and(|area| area.clan_id.is_some())
    {
        return create_outside_share(&mut st, caller, aid, &req);
    }
    if let Some(aid) = atlas_id
        && st
            .atlases
            .get(&aid)
            .is_some_and(|atlas| atlas.clan_id.is_some())
    {
        return not_found();
    }

    // 1. Resolve the subject's TRUE owner; nonexistent -> 404.
    let owner_id = if let Some(aid) = area_id {
        match st.areas.get(&aid) {
            Some(a) => a.user_id,
            None => return not_found(),
        }
    } else if let Some(aid) = atlas_id {
        match st.atlases.get(&aid) {
            Some(a) => a.user_id,
            None => return not_found(),
        }
    } else {
        return not_found();
    };
    if req.grantee_id == owner_id {
        return not_found();
    }

    // 2. Standing + parent inference. An effective can_admin grantor acts AS
    //    the owner (root path); can_admin itself is owner-minted only.
    let caller_is_owner = caller == owner_id;
    let caller_is_admin = !caller_is_owner
        && if let Some(aid) = area_id {
            st.caps(caller, aid).is_some_and(|c| c.can_admin)
        } else if let Some(tid) = atlas_id {
            st.grants
                .iter()
                .any(|g| g.grantee_id == caller && g.atlas_id == Some(tid) && g.can_admin)
        } else {
            false
        };
    if req.can_admin && !caller_is_owner {
        return not_found(); // admins cannot appoint sub-admins
    }
    let parent_grant_id = if caller_is_owner || caller_is_admin {
        None
    } else {
        match find_covering_parent(
            &st,
            caller,
            owner_id,
            area_id,
            atlas_id,
            req.can_edit,
            req.can_copy,
        ) {
            Some(pid) => Some(pid),
            None => return not_found(),
        }
    };

    // 3. Depth-1: children never carry reshare / admin; can_admin rides only
    //    an owner root.
    let (can_edit, can_reshare, can_copy, can_admin) = if parent_grant_id.is_some() {
        (req.can_edit, false, req.can_copy, false)
    } else {
        (
            req.can_edit,
            req.can_reshare,
            req.can_copy,
            req.can_admin && caller_is_owner,
        )
    };

    // Guard CHECK: grantor <> grantee.
    if caller == req.grantee_id {
        return not_found();
    }

    if let Err(e) = validate_grant_write(
        &st,
        caller,
        req.grantee_id,
        owner_id,
        parent_grant_id,
        can_edit,
        can_reshare,
        can_copy,
        can_admin,
    ) {
        return e;
    }

    // Snapshot the grantor's advisory host hints for THIS share; a re-grant is
    // a fresh consent moment, so the new snapshot REPLACES any prior one (a
    // hint-less re-grant clears it).
    let host_hints = match normalize_host_hints(req.host_hints.as_deref()) {
        Ok(h) => h,
        Err(e) => return e,
    };

    // 4. Idempotent upsert on (grantor, grantee, scope).
    let existing = st.grants.iter_mut().find(|g| {
        g.grantor_id == caller
            && g.grantee_id == req.grantee_id
            && g.area_id == area_id
            && g.atlas_id == atlas_id
    });
    let row = if let Some(g) = existing {
        g.can_edit = can_edit;
        g.can_reshare = can_reshare;
        g.can_copy = can_copy;
        g.can_admin = can_admin;
        g.parent_grant_id = parent_grant_id;
        g.host_hints = host_hints;
        g.updated_at = Utc::now();
        g.clone()
    } else {
        let grant = GrantRecord {
            id: Uuid::new_v4(),
            owner_id,
            grantor_id: caller,
            grantee_id: req.grantee_id,
            area_id,
            atlas_id,
            can_edit,
            can_reshare,
            can_copy,
            can_admin,
            host_hints,
            parent_grant_id,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        st.grants.push(grant.clone());
        grant
    };
    created(grant_json(&row))
}

// ---------------------------------------------------------------------------
// GET /shares?direction=given|received
// ---------------------------------------------------------------------------

/// GET /shares — the caller's own rows + chain depth, created order.
pub async fn list_shares(
    State(state): State<Shared>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    let caller = match share_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let given = match params.get("direction").map(String::as_str) {
        Some("given") => true,
        Some("received") => false,
        _ => return bad_request("direction must be 'given' or 'received'"),
    };
    let rows: Vec<Value> = st
        .grants
        .iter()
        .filter(|g| {
            if given {
                g.grantor_id == caller
            } else {
                g.grantee_id == caller
            }
        })
        .map(|g| {
            let mut row = grant_json(g);
            row["depth"] = json!(grant_depth(&st, g));
            row
        })
        .collect();
    ok(json!(rows))
}

// ---------------------------------------------------------------------------
// PATCH /shares/{id}
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PatchShareRequest {
    can_edit: Option<bool>,
    can_reshare: Option<bool>,
    can_copy: Option<bool>,
    /// No longer a share flag: `true` is a 400, `false` is accepted.
    include_secrets: Option<bool>,
    #[serde(default)]
    can_admin: Option<bool>,
}

/// PATCH /shares/{id} — raises re-validate; lowering can_edit/can_copy clamps
/// descendants; lowering can_reshare DELETES descendants.
pub async fn patch_share(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    let caller = match share_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(grant_id) = parse_social_id(&raw_id) else {
        return not_found();
    };
    let req: PatchShareRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if req.include_secrets == Some(true) {
        return bad_request(INCLUDE_SECRETS);
    }

    let Some(cur) = st.grant(grant_id).cloned() else {
        return not_found();
    };
    if let Some(area) = outside_share_area(&st, &cur) {
        if !may_revoke_outside(&st, caller, &cur, area) {
            return not_found();
        }
        let raised = [req.can_edit, req.can_reshare, req.can_copy, req.can_admin]
            .into_iter()
            .any(|flag| flag == Some(true));
        if raised {
            return bad_request("An outside share is view only");
        }
        return ok(grant_json(&cur));
    }
    if !caller_may_manage(&st, caller, &cur) {
        return not_found();
    }

    let new_can_edit = req.can_edit.unwrap_or(cur.can_edit);
    let new_can_reshare = req.can_reshare.unwrap_or(cur.can_reshare);
    let new_can_copy = req.can_copy.unwrap_or(cur.can_copy);
    let new_can_admin = req.can_admin.unwrap_or(cur.can_admin);

    // can_admin: owner-only to set/raise/remove, and only on an owner-minted root.
    if new_can_admin != cur.can_admin {
        let owner_minted_root = cur.parent_grant_id.is_none() && cur.grantor_id == cur.owner_id;
        if caller != cur.owner_id || !owner_minted_root {
            return not_found();
        }
    }

    let raised = (new_can_edit && !cur.can_edit)
        || (new_can_reshare && !cur.can_reshare)
        || (new_can_copy && !cur.can_copy)
        || (new_can_admin && !cur.can_admin);
    if raised {
        if cur.parent_grant_id.is_some() && (new_can_reshare || new_can_admin) {
            return not_found();
        }
        let (v_reshare, v_admin) = if cur.parent_grant_id.is_some() {
            (false, false)
        } else {
            (new_can_reshare, new_can_admin)
        };
        if let Err(e) = validate_grant_write(
            &st,
            cur.grantor_id,
            cur.grantee_id,
            cur.owner_id,
            cur.parent_grant_id,
            new_can_edit,
            v_reshare,
            new_can_copy,
            v_admin,
        ) {
            return e;
        }
    }

    let updated = {
        let g = st.grant_mut(grant_id).expect("grant exists");
        g.can_edit = new_can_edit;
        g.can_reshare = new_can_reshare;
        g.can_copy = new_can_copy;
        g.can_admin = new_can_admin;
        g.updated_at = Utc::now();
        g.clone()
    };

    let lowered_edit = cur.can_edit && !new_can_edit;
    let lowered_copy = cur.can_copy && !new_can_copy;
    let lowered_reshare = cur.can_reshare && !new_can_reshare;
    if lowered_reshare {
        let children: Vec<Uuid> = st.grant_descendants(grant_id);
        st.grants.retain(|g| !children.contains(&g.id));
    } else if lowered_edit || lowered_copy {
        let descendants = st.grant_descendants(grant_id);
        for g in st.grants.iter_mut().filter(|g| descendants.contains(&g.id)) {
            g.can_edit = g.can_edit && new_can_edit;
            g.can_copy = g.can_copy && new_can_copy;
            g.updated_at = Utc::now();
        }
    }
    ok(grant_json(&updated))
}

/// DELETE /shares/{id} — grantor/owner/ancestor; subtree cascades. 200 null.
pub async fn delete_share(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    let caller = match share_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(grant_id) = parse_social_id(&raw_id) else {
        return not_found();
    };
    let Some(cur) = st.grant(grant_id).cloned() else {
        return not_found();
    };
    let allowed = match outside_share_area(&st, &cur) {
        Some(area) => may_revoke_outside(&st, caller, &cur, area),
        None => caller_may_manage(&st, caller, &cur),
    };
    if !allowed {
        return not_found();
    }
    st.delete_grants_cascading(&[grant_id]);
    ok(Value::Null)
}

// ---------------------------------------------------------------------------
// GET /areas/{id}/shares
// ---------------------------------------------------------------------------

/// GET /areas/{id}/shares — owner: full reaching tree; re-sharer: own
/// subtree; grantee: own row(s); else uniform 404. depth from visible root.
pub async fn area_shares(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let st = state.lock();
    let caller = match share_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(area) = st.areas.get(&area_id) else {
        return not_found();
    };
    if area.clan_id.is_some() {
        return list_outside_shares(&st, caller, area);
    }
    let is_owner = area.user_id == caller;

    // Grants REACHING this area: Area-scope on it, or Atlas-scope on its
    // CURRENT atlas.
    let reaching: Vec<&GrantRecord> = st
        .grants
        .iter()
        .filter(|g| {
            g.area_id == Some(area_id) || (g.atlas_id.is_some() && g.atlas_id == area.atlas_id)
        })
        .collect();
    let reaching_ids: Vec<Uuid> = reaching.iter().map(|g| g.id).collect();

    // Roots: every reaching grant for the owner; the caller's own otherwise.
    // Descend ONLY into descendants that also reach the area; keep MIN depth.
    let mut depths: std::collections::HashMap<Uuid, i32> = std::collections::HashMap::new();
    let mut frontier: Vec<(Uuid, i32)> = reaching
        .iter()
        .filter(|g| is_owner || g.grantee_id == caller)
        .map(|g| (g.id, 0))
        .collect();
    while let Some((id, depth)) = frontier.pop() {
        if depths.get(&id).is_some_and(|&d| d <= depth) {
            continue;
        }
        depths.insert(id, depth);
        for child in st
            .grants
            .iter()
            .filter(|g| g.parent_grant_id == Some(id) && reaching_ids.contains(&g.id))
        {
            frontier.push((child.id, depth + 1));
        }
    }

    if depths.is_empty() && !is_owner {
        return not_found();
    }

    let mut nodes: Vec<(i32, usize, Value)> = Vec::new();
    for (idx, g) in st.grants.iter().enumerate() {
        let Some(depth) = depths.get(&g.id) else {
            continue;
        };
        let mut node = grant_json(g);
        node["depth"] = json!(depth);
        if let Some(nickname) = st.user(g.grantee_id).and_then(|u| u.nickname.clone()) {
            node["grantee_nickname"] = json!(nickname);
        }
        nodes.push((*depth, idx, node));
    }
    nodes.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let rows: Vec<Value> = nodes.into_iter().map(|(_, _, v)| v).collect();
    ok(json!(rows))
}

// ---------------------------------------------------------------------------
// Outside shares of a clan's Clan-owned maps (docs/clans.md §5.5)
// ---------------------------------------------------------------------------

/// The clan map an outside share names; `None` for every other share.
fn outside_share_area<'a>(
    st: &'a MockState,
    grant: &GrantRecord,
) -> Option<&'a super::state::AreaRecord> {
    grant
        .area_id
        .and_then(|area| st.areas.get(&area))
        .filter(|area| area.clan_id.is_some())
}

fn shares_externally(st: &MockState, user: Uuid, area: &super::state::AreaRecord) -> bool {
    area.member_owned.is_none()
        && super::clan_maps::area_actions(st, user, area).is_some_and(|actions| {
            actions.contains("area.read") && actions.contains("area.share_external")
        })
}

fn is_clan_owner(st: &MockState, user: Uuid, area: &super::state::AreaRecord) -> bool {
    area.clan_id
        .and_then(|clan| st.clans.clans.get(&clan))
        .is_some_and(|clan| !clan.dissolved && clan.has_owner(user))
}

/// The outside shares of a Clan-owned map that are live, oldest first:
/// those whose sharer still holds `area.share_external` on it.
pub fn live_outside_shares<'a>(
    st: &'a MockState,
    area: &super::state::AreaRecord,
) -> Vec<&'a GrantRecord> {
    let mut shares: Vec<&GrantRecord> = st
        .grants
        .iter()
        .filter(|grant| {
            grant.area_id == Some(area.id) && shares_externally(st, grant.grantor_id, area)
        })
        .collect();
    shares.sort_by_key(|grant| (grant.created_at, grant.id));
    shares
}

/// Whether `user` sees a Clan-owned map's outside shares: one who reads it
/// and holds `area.share_external` on it, clan owners among them.
pub fn sees_outside_shares(st: &MockState, user: Uuid, area: &super::state::AreaRecord) -> bool {
    area.member_owned.is_none()
        && (is_clan_owner(st, user, area) || shares_externally(st, user, area))
}

/// Its sharer, or a clan owner.
fn may_revoke_outside(
    st: &MockState,
    caller: Uuid,
    grant: &GrantRecord,
    area: &super::state::AreaRecord,
) -> bool {
    grant.grantor_id == caller || is_clan_owner(st, caller, area)
}

/// Whether an outside share lets `viewer` read the clan's map: one made to
/// them by a member who still holds `area.share_external` on it.
pub fn outside_reader(st: &MockState, viewer: Uuid, area: &super::state::AreaRecord) -> bool {
    area.clan_id.is_some()
        && st.grants.iter().any(|grant| {
            grant.area_id == Some(area.id)
                && grant.grantee_id == viewer
                && shares_externally(st, grant.grantor_id, area)
        })
}

/// Deletes the outside shares whose sharer no longer may share their map,
/// as every change to clan access runs it: a share ends when its sharer
/// leaves or loses `area.read` or `area.share_external` there, and does not
/// return when they regain it.
pub fn sweep_outside_shares(st: &mut MockState) {
    let ended: Vec<Uuid> = st
        .grants
        .iter()
        .filter(|grant| {
            outside_share_area(st, grant)
                .is_some_and(|area| !shares_externally(st, grant.grantor_id, area))
        })
        .map(|grant| grant.id)
        .collect();
    if !ended.is_empty() {
        st.delete_grants_cascading(&ended);
    }
}

/// The sharer's departure deletes the outside shares they made.
pub fn forget_outside_shares_by(st: &mut MockState, clan: Uuid, user: Uuid) {
    let maps: Vec<Uuid> = st
        .areas
        .values()
        .filter(|area| area.clan_id == Some(clan))
        .map(|area| area.id)
        .collect();
    st.grants.retain(|grant| {
        !(grant.grantor_id == user && grant.area_id.is_some_and(|area| maps.contains(&area)))
    });
}

fn create_outside_share(
    st: &mut MockState,
    caller: Uuid,
    area_id: Uuid,
    req: &CreateShareRequest,
) -> Response {
    let area = &st.areas[&area_id];
    let clan_id = area.clan_id.expect("a clan map");
    if !shares_externally(st, caller, area) {
        return not_found();
    }
    if req.can_edit || req.can_reshare || req.can_copy || req.can_admin {
        return bad_request("An outside share is view only");
    }
    let outsider = st
        .clans
        .clans
        .get(&clan_id)
        .is_none_or(|clan| !clan.has_member(req.grantee_id));
    if caller == req.grantee_id
        || !outsider
        || !st.are_friends(caller, req.grantee_id)
        || st.blocked_pair(caller, req.grantee_id)
    {
        return not_found();
    }
    let existing = st.grants.iter().find(|grant| {
        grant.grantor_id == caller
            && grant.grantee_id == req.grantee_id
            && grant.area_id == Some(area_id)
    });
    if let Some(grant) = existing {
        return created(grant_json(grant));
    }
    let now = Utc::now();
    let grant = GrantRecord {
        id: Uuid::new_v4(),
        owner_id: clan_id,
        grantor_id: caller,
        grantee_id: req.grantee_id,
        area_id: Some(area_id),
        atlas_id: None,
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
        host_hints: None,
        parent_grant_id: None,
        created_at: now,
        updated_at: now,
    };
    st.grants.push(grant.clone());
    created(grant_json(&grant))
}

/// A clan map's outside shares, oldest first, to those who read it and hold
/// `area.share_external` on it, clan owners among them, each with who made
/// it.
fn list_outside_shares(st: &MockState, caller: Uuid, area: &super::state::AreaRecord) -> Response {
    let owner_sees = is_clan_owner(st, caller, area) && area.member_owned.is_none();
    if !owner_sees && !shares_externally(st, caller, area) {
        return not_found();
    }
    let nickname = |user: Uuid| st.user(user).and_then(|record| record.nickname.clone());
    let mut shares: Vec<&GrantRecord> = st
        .grants
        .iter()
        .filter(|grant| grant.area_id == Some(area.id))
        .collect();
    shares.sort_by_key(|grant| (grant.created_at, grant.id));
    let rows: Vec<Value> = shares
        .into_iter()
        .map(|grant| {
            let mut row = grant_json(grant);
            row["depth"] = json!(0);
            row["grantor_nickname"] = json!(nickname(grant.grantor_id));
            row["grantee_nickname"] = json!(nickname(grant.grantee_id));
            row
        })
        .collect();
    ok(json!(rows))
}
