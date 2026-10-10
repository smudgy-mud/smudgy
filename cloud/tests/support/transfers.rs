//! Ownership transfer surface: `POST /areas|/atlases/{id}/transfer`,
//! `GET /transfers`, `GET /clans/{c}/transfers`, `POST
//! /transfers/{id}/accept|decline`, `DELETE /transfers/{id}`. Fidelity
//! reference: the smudgy-cloudflare service (`src/transfers/*.ts`,
//! `importTransfer` and `importClanTransfer` in `src/library/maps/moves.ts`;
//! docs/format-3.md §5.3, §5.4).
//!
//! Only a user's own maps and atlases are offered, by their owner, to a
//! friend or to a clan they are an active member of; a clan's are never
//! transferred (the uniform 404, whoever asks). Accepting an offer to a user
//! moves the subject to them with its owner Secrets, drops the grants they
//! held and admin grants, and gives the former owner an admin share-back
//! and every action on each owner Secret. Accepting an offer to a clan, by a
//! holder of `atlas.accept_transfer`, makes the subject the clan's: owner
//! Secrets become Member-owned Secrets owned by the offerer, their grants to
//! active members carry over merged per member, shares end, and nobody gets
//! a share-back. Either acceptance ends the moved maps' links to every
//! other clan at its commit point, as their owner ending them would: the
//! filing goes, and that clan's Secrets on the map are suppressed and kept
//! (back if the map is filed in that clan again). The answer is the same
//! whether or not the map carried any. Either acceptance leaves out the
//! moved maps' exits into rooms of other maps' Secrets the accepting user
//! does not read, with the connections they leave without members.
//!
//! An acceptance claims its offer, then flips the subject only while the
//! offerer still owns it and is not being deleted. One that stops before
//! its flip puts the offer back while it could still be made, and cancels
//! it otherwise (`MockState::interrupt_acceptance`).

use std::collections::{BTreeSet, HashMap};

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use super::clan_secrets::{ClanSecretGrantRecord, ClanSecretRecord};
use super::clans::{ACCEPT_TRANSFER, ClanRecipient, ClanResource};
use super::http::{authenticate, bad_request, err, gate_verified, not_found, ok, parse_body};
use super::secret_grants::{ADD, COPY, EDIT, MANAGE_ACCESS, REMOVE};
use super::social::Shared;
use super::state::{
    ConnectionRecord, ExitRecord, GrantRecord, MockState, PendingTransferRecord, SecretGrantRecord,
};

fn gate(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (viewer, _) = authenticate(st, headers)?;
    gate_verified(st, viewer)?;
    Ok(viewer)
}

/// The user who owns a subject; `None` for a clan's map or atlas, which no
/// user owns.
fn subject_owner(st: &MockState, area_id: Option<Uuid>, atlas_id: Option<Uuid>) -> Option<Uuid> {
    let owner = if let Some(a) = area_id {
        st.areas.get(&a).map(|r| (r.user_id, r.clan_id))
    } else if let Some(a) = atlas_id {
        st.atlases.get(&a).map(|r| (r.user_id, r.clan_id))
    } else {
        None
    };
    owner
        .filter(|(_, clan)| clan.is_none())
        .map(|(user, _)| user)
}

/// Accepted friendship + no blocks either direction + BOTH verified.
fn transfer_gate(st: &MockState, from: Uuid, to: Uuid) -> bool {
    st.are_friends(from, to)
        && !st.blocked_pair(from, to)
        && st.email_verified(from)
        && st.email_verified(to)
}

fn active_member(st: &MockState, user: Uuid, clan: Uuid) -> bool {
    st.clans
        .clans
        .get(&clan)
        .is_some_and(|record| record.has_member(user))
}

/// `user`'s clan actions on one of `clan`'s folders, or on the clan itself.
fn holds_accept(st: &MockState, clan: Uuid, user: Uuid, resource: ClanResource) -> bool {
    st.clans
        .clans
        .get(&clan)
        .filter(|record| !record.dissolved)
        .is_some_and(|record| record.actions_on(user, resource).contains(ACCEPT_TRANSFER))
}

/// Whether `user` handles a clan's incoming transfers: `atlas.accept_transfer`
/// on the clan, or on any of its folders.
fn receives_transfers(st: &MockState, clan: Uuid, user: Uuid) -> bool {
    holds_accept(st, clan, user, ClanResource::Clan)
        || st
            .atlases
            .values()
            .filter(|atlas| atlas.clan_id == Some(clan))
            .any(|atlas| holds_accept(st, clan, user, ClanResource::Atlas(atlas.id)))
}

fn nickname(st: &MockState, user: Uuid) -> Option<String> {
    st.user(user).and_then(|record| record.nickname.clone())
}

/// An offer as the server serves it; absent fields are omitted, and an
/// offer to a clan names the clan in place of a user.
fn transfer_json(st: &MockState, t: &PendingTransferRecord) -> Value {
    let subject_name = if let Some(a) = t.area_id {
        st.areas.get(&a).map(|r| r.name.clone())
    } else if let Some(a) = t.atlas_id {
        st.atlases.get(&a).map(|r| r.name.clone())
    } else {
        None
    };
    let mut out = json!({
        "id": t.id,
        "subject_kind": t.subject_kind,
        "from_user_id": t.from_user_id,
        "status": t.status,
        "created_at": t.created_at,
    });
    if let Some(area) = t.area_id {
        out["area_id"] = json!(area);
    }
    if let Some(atlas) = t.atlas_id {
        out["atlas_id"] = json!(atlas);
    }
    if let Some(to) = t.to_user_id {
        out["to_user_id"] = json!(to);
        if let Some(name) = nickname(st, to) {
            out["to_nickname"] = json!(name);
        }
    }
    if let Some(clan) = t.to_clan_id {
        out["to_clan_id"] = json!(clan);
        out["to_clan_name"] = json!(st.clans.clans.get(&clan).map(|record| &record.name));
        out["ownership"] = json!(t.ownership.unwrap_or("clan"));
    }
    if let Some(at) = t.responded_at {
        out["responded_at"] = json!(at);
    }
    if let Some(name) = subject_name {
        out["subject_name"] = json!(name);
    }
    if let Some(name) = nickname(st, t.from_user_id) {
        out["from_nickname"] = json!(name);
    }
    out
}

fn optional_uuid(body: &Value, field: &str) -> Result<Option<Uuid>, Response> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => Uuid::parse_str(raw)
            .map(Some)
            .map_err(|_| bad_request(&format!("`{field}` must be a UUID"))),
        Some(_) => Err(bad_request(&format!("`{field}` must be a UUID"))),
    }
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
    create_transfer(&state, &headers, &body, Some(area_id), None, false)
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
    create_transfer(&state, &headers, &body, None, Some(atlas_id), false)
}

fn create_transfer(
    state: &Shared,
    headers: &HeaderMap,
    body: &str,
    area_id: Option<Uuid>,
    atlas_id: Option<Uuid>,
    legacy: bool,
) -> Response {
    let mut st = state.lock();
    let caller = match gate(&st, headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let fields: Value = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // Exactly one recipient: a user or a clan.
    let (to_user, to_clan) = match (
        optional_uuid(&fields, "to_user_id"),
        optional_uuid(&fields, "to_clan_id"),
    ) {
        (Err(e), _) | (_, Err(e)) => return e,
        (Ok(Some(_)), Ok(Some(_))) => {
            return bad_request("expected one of `to_user_id` and `to_clan_id`");
        }
        (Ok(None), Ok(None)) => return bad_request("missing field `to_user_id`"),
        (Ok(user), Ok(clan)) => (user, clan),
    };
    // Whose each map becomes in the clan; a 400 on an offer to a user.
    let ownership = match fields.get("ownership") {
        None | Some(Value::Null) => to_clan.map(|_| "clan"),
        Some(value) => match (value.as_str(), to_clan) {
            (Some("clan"), Some(_)) => Some("clan"),
            (Some("members"), Some(_)) => Some("members"),
            _ => return bad_request("`ownership` is `clan` or `members`, on an offer to a clan"),
        },
    };

    let direct = to_clan.is_some() && !legacy;
    let operation = if direct {
        match optional_uuid(&fields, "operation_id") {
            Ok(Some(id)) => id,
            Ok(None) => return bad_request("missing field `operation_id`"),
            Err(error) => return error,
        }
    } else {
        Uuid::new_v4()
    };
    let destination = if direct && area_id.is_some() {
        match optional_uuid(&fields, "atlas_id") {
            Ok(Some(id)) => Some(id),
            Ok(None) => return bad_request("missing field `atlas_id`"),
            Err(error) => return error,
        }
    } else {
        None
    };
    if direct {
        if let Some(previous) = st.pending_transfers.iter().find(|t| t.id == operation) {
            if previous.from_user_id != caller {
                return not_found();
            }
            if !previous.direct
                || previous.area_id != area_id
                || previous.atlas_id != atlas_id
                || previous.to_clan_id != to_clan
                || previous.ownership != ownership
                || previous.destination_atlas_id != destination
            {
                return bad_request("operation_id already names a different transfer");
            }
            if previous.status == "Accepted" {
                return ok(transfer_json(&st, previous));
            }
            if previous.status != "Cancelled" {
                return err(409, "transfer_already_pending");
            }
        }
        let clan = to_clan.expect("direct clan transfer");
        let allowed = match destination {
            Some(folder) => {
                st.atlases
                    .get(&folder)
                    .is_some_and(|a| a.clan_id == Some(clan))
                    && holds_accept(&st, clan, caller, ClanResource::Atlas(folder))
            }
            None => holds_accept(&st, clan, caller, ClanResource::Clan),
        };
        if !allowed {
            return not_found();
        }
    }

    // Raw owner only, and never a clan's map or atlas.
    match subject_owner(&st, area_id, atlas_id) {
        Some(o) if o == caller => {}
        _ => return not_found(),
    }
    let allowed = match (to_user, to_clan) {
        (Some(to), _) => to != caller && transfer_gate(&st, caller, to),
        // Friendship and blocks play no part in an offer to a clan.
        (None, Some(clan)) => active_member(&st, caller, clan),
        (None, None) => false,
    };
    if !allowed {
        return not_found();
    }
    // One live offer per subject -> 409.
    if st
        .pending_transfers
        .iter()
        .any(|t| t.status == "Offered" && t.area_id == area_id && t.atlas_id == atlas_id)
    {
        return err(409, "transfer_already_pending");
    }

    st.pending_transfers.retain(|record| record.id != operation);
    let rec = PendingTransferRecord {
        id: operation,
        direct,
        destination_atlas_id: destination,
        subject_kind: if area_id.is_some() { "area" } else { "atlas" }.to_string(),
        area_id,
        atlas_id,
        from_user_id: caller,
        to_user_id: to_user,
        to_clan_id: to_clan,
        ownership,
        status: "Offered".to_string(),
        created_at: Utc::now(),
        responded_at: None,
    };
    let view = transfer_json(&st, &rec);
    st.pending_transfers.push(rec);
    if direct {
        match accept_for_clan(&mut st, caller, operation, None, destination) {
            Ok(()) => {
                let completed = mark(&mut st, operation, "Accepted").expect("recorded transfer");
                ok(transfer_json(&st, &completed))
            }
            Err(response) => {
                mark(&mut st, operation, "Cancelled");
                response
            }
        }
    } else {
        super::http::created(view)
    }
}

/// Live offers matching `keep`, newest first.
fn live_offers(st: &MockState, keep: impl Fn(&PendingTransferRecord) -> bool) -> Value {
    let mut rows: Vec<&PendingTransferRecord> = st
        .pending_transfers
        .iter()
        .filter(|t| t.status == "Offered" && keep(t))
        .collect();
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    Value::Array(rows.into_iter().map(|t| transfer_json(st, t)).collect())
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
    match params.get("direction").map(String::as_str) {
        // The caller's offers, to users and to clans alike.
        Some("offered") => ok(live_offers(&st, |t| t.from_user_id == caller)),
        // Only offers made to the caller; a clan's are listed per clan.
        Some("received") => ok(live_offers(&st, |t| t.to_user_id == Some(caller))),
        _ => bad_request("direction must be 'offered' or 'received'"),
    }
}

/// `GET /clans/{c}/transfers`: the clan's live offers, for holders of
/// `atlas.accept_transfer` on the clan or any of its folders.
pub async fn list_clan_transfers(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(clan) = Uuid::parse_str(&raw) else {
        return bad_request("Invalid clan ID");
    };
    let st = state.lock();
    let caller = match gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !receives_transfers(&st, clan, caller) {
        return not_found();
    }
    ok(live_offers(&st, |t| t.to_clan_id == Some(clan)))
}

/// The maps a subject carries: the map, or every map filed in the atlas.
fn subject_maps(st: &MockState, area_id: Option<Uuid>, atlas_id: Option<Uuid>) -> Vec<Uuid> {
    match (area_id, atlas_id) {
        (Some(area), _) => vec![area],
        (None, Some(atlas)) => {
            let mut members: Vec<(u64, Uuid)> = st
                .areas
                .values()
                .filter(|a| a.atlas_id == Some(atlas))
                .map(|a| (a.created_seq, a.id))
                .collect();
            members.sort_unstable();
            members.into_iter().map(|(_, id)| id).collect()
        }
        (None, None) => Vec::new(),
    }
}

/// Whether the offer could still be made: its offerer is not being deleted
/// and still owns the subject, and is the recipient's friend with no block
/// between them, or an active member of the recipient clan.
fn still_valid(st: &MockState, offer: &PendingTransferRecord) -> bool {
    let from = offer.from_user_id;
    !st.deleting_accounts.contains(&from)
        && subject_owner(st, offer.area_id, offer.atlas_id) == Some(from)
        && match (offer.to_user_id, offer.to_clan_id) {
            (Some(to), _) => st.are_friends(from, to) && !st.blocked_pair(from, to),
            (None, Some(clan)) => active_member(st, from, clan),
            (None, None) => false,
        }
}

/// The steps between an acceptance's claim and its flip. A test's
/// [`MockState::interrupt_acceptance`] runs here and stops the acceptance
/// when it says so, as exports a write kept from sealing three times do; so
/// does a flip that finds the offer could no longer be made (the offerer no
/// longer owning the subject or being deleted, an unfriending or block
/// between them, the offerer's departure from the recipient clan) or the
/// recipient clan dissolving. A stopped acceptance puts the offer back if it
/// could still be made and cancels it otherwise, so a departure,
/// unfriending, block or deletion that ran meanwhile is never undone; it
/// answers the uniform 404. Nothing of the subject reaches the recipient
/// before the flip.
fn claim_and_flip(st: &mut MockState, offer: &PendingTransferRecord) -> Result<(), Response> {
    if st.deleting_accounts.contains(&offer.from_user_id) {
        return Err(not_found());
    }
    let stopped = st
        .interrupt_acceptance
        .take()
        .is_some_and(|during| during(st, offer));
    let flips = still_valid(st, offer)
        && offer.to_clan_id.is_none_or(|clan| {
            st.clans
                .clans
                .get(&clan)
                .is_some_and(|record| !record.dissolving && !record.dissolved)
        });
    if stopped || !flips {
        if !still_valid(st, offer) {
            mark(st, offer.id, "Cancelled");
        }
        return Err(not_found());
    }
    Ok(())
}

fn mark(st: &mut MockState, id: Uuid, status: &str) -> Option<PendingTransferRecord> {
    let offer = st.pending_transfers.iter_mut().find(|t| t.id == id)?;
    offer.status = status.to_string();
    offer.responded_at = Some(Utc::now());
    Some(offer.clone())
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
    let fields: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match parse_body(&body) {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
    let name = match fields.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name.clone()),
        Some(_) => return bad_request("`name` must be a string"),
    };
    let atlas = match optional_uuid(&fields, "atlas_id") {
        Ok(v) => v,
        Err(e) => return e,
    };

    let mine = st
        .pending_transfers
        .iter()
        .find(|t| t.id == transfer_id && t.to_user_id == Some(caller) && t.status == "Offered")
        .cloned();
    let result = match mine {
        Some(offer) => accept_for_user(&mut st, caller, &offer, name, atlas),
        None => accept_for_clan(&mut st, caller, transfer_id, name, atlas),
    };
    match result {
        Ok(()) => {
            let offer = mark(&mut st, transfer_id, "Accepted").expect("the offer exists");
            ok(transfer_json(&st, &offer))
        }
        Err(response) => response,
    }
}

/// Moves the subject to the user the offer names.
fn accept_for_user(
    st: &mut MockState,
    to: Uuid,
    offer: &PendingTransferRecord,
    name: Option<String>,
    atlas: Option<Uuid>,
) -> Result<(), Response> {
    let from = offer.from_user_id;
    let (area_id, atlas_id) = (offer.area_id, offer.atlas_id);
    if subject_owner(st, area_id, atlas_id) != Some(from) || !transfer_gate(st, from, to) {
        return Err(not_found());
    }
    let maps = subject_maps(st, area_id, atlas_id);
    // A map is filed in one of the recipient's own atlases, or left loose.
    let refile = match (area_id, atlas) {
        (Some(_), Some(target)) => {
            if st.atlases.get(&target).is_none_or(|r| r.user_id != to) {
                return Err(not_found());
            }
            Some(target)
        }
        _ => None,
    };
    super::mutations::transfer_ids_available(st, &maps, to, None)?;
    claim_and_flip(st, offer)?;
    // Exits into rooms of Secrets the recipient does not read are left out.
    let unread = unread_foreign_exits(st, to, &maps);
    leave_out_exits(st, &maps, &unread);

    // Grants the recipient held, and admin grants, go with everything
    // re-shared under them (the server's `subtrees`, recursive over the
    // parent grant); the rest move to the new owner and keep their grantors.
    let covers = |g: &GrantRecord| {
        g.area_id.is_some_and(|a| maps.contains(&a))
            || (atlas_id.is_some() && g.atlas_id == atlas_id)
    };
    let dropped: Vec<Uuid> = st
        .grants
        .iter()
        .filter(|g| covers(g) && (g.grantee_id == to || g.can_admin))
        .map(|g| g.id)
        .collect();
    st.delete_grants_cascading(&dropped);
    for g in st.grants.iter_mut().filter(|g| covers(g)) {
        g.owner_id = to;
    }
    if let Some(subject) = atlas_id
        && let Some(record) = st.atlases.get_mut(&subject)
    {
        record.user_id = to;
        if let Some(n) = name.clone() {
            record.name = n;
        }
        record.rev += 1;
    }
    let now = Utc::now();
    for map in &maps {
        let Some(area) = st.areas.get_mut(map) else {
            continue;
        };
        area.user_id = to;
        if area_id.is_some() {
            // A transferred map leaves its atlas.
            area.atlas_id = refile;
            if let Some(n) = name.clone() {
                area.name = n;
            }
        }
        area.rev += 1;
        // Owner Secrets come along; the recipient's grants on them are
        // dropped, and the former owner keeps every action. A clan's
        // Secrets stay that clan's, suppressed with its link.
        for secret in area
            .secrets
            .iter_mut()
            .filter(|secret| secret.clan.is_none())
        {
            secret.grants.retain(|grant| grant.grantee_id != to);
            secret.grants.push(SecretGrantRecord {
                id: Uuid::new_v4(),
                grantor_id: to,
                grantee_id: from,
                actions: [ADD, EDIT, REMOVE, MANAGE_ACCESS, COPY]
                    .into_iter()
                    .collect(),
                created_at: now,
                updated_at: now,
            });
        }
    }

    // Share-back: the former owner becomes a can_admin deputy.
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
        can_admin: true,
        host_hints: None,
        parent_grant_id: None,
        created_at: now,
        updated_at: now,
    });
    Ok(())
}

/// Moves the subject into the clan an offer names, through a member holding
/// `atlas.accept_transfer`.
fn accept_for_clan(
    st: &mut MockState,
    caller: Uuid,
    transfer_id: Uuid,
    name: Option<String>,
    atlas: Option<Uuid>,
) -> Result<(), Response> {
    let offer = st
        .pending_transfers
        .iter()
        .find(|t| t.id == transfer_id && t.status == "Offered")
        .cloned()
        .filter(|t| {
            t.to_clan_id
                .is_some_and(|clan| active_member(st, caller, clan))
        })
        .ok_or_else(not_found)?;
    let clan = offer.to_clan_id.expect("an offer to a clan");
    if !receives_transfers(st, clan, caller) {
        return Err(not_found());
    }
    let (area_id, atlas_id) = (offer.area_id, offer.atlas_id);
    if area_id.is_some() && atlas.is_none() {
        return Err(bad_request("missing field `atlas_id`"));
    }
    let from = offer.from_user_id;
    if subject_owner(st, area_id, atlas_id) != Some(from) || !active_member(st, from, clan) {
        return Err(not_found());
    }
    // A map is filed in a clan folder where the acceptor holds the action;
    // an atlas needs it on the clan.
    let filed = match area_id {
        Some(_) => {
            let target = atlas.expect("checked above");
            let in_clan = st
                .atlases
                .get(&target)
                .is_some_and(|record| record.clan_id == Some(clan));
            if !in_clan || !holds_accept(st, clan, caller, ClanResource::Atlas(target)) {
                return Err(not_found());
            }
            Some(target)
        }
        None => {
            if !holds_accept(st, clan, caller, ClanResource::Clan) {
                return Err(not_found());
            }
            None
        }
    };
    if st
        .clans
        .clans
        .get(&clan)
        .is_some_and(|record| record.dissolving)
    {
        return Err(err(409, "clan_dissolving"));
    }
    let maps = subject_maps(st, area_id, atlas_id);
    super::mutations::transfer_ids_available(st, &maps, caller, Some(clan))?;
    claim_and_flip(st, &offer)?;
    // Exits into rooms of Secrets the accepting member does not read are
    // left out.
    let unread = unread_foreign_exits(st, caller, &maps);
    leave_out_exits(st, &maps, &unread);

    // Shares end, with everything re-shared under them: the clan's grants
    // decide who reads it.
    let ended: Vec<Uuid> = st
        .grants
        .iter()
        .filter(|g| {
            g.area_id.is_some_and(|a| maps.contains(&a))
                || (atlas_id.is_some() && g.atlas_id == atlas_id)
        })
        .map(|g| g.id)
        .collect();
    st.delete_grants_cascading(&ended);
    if let Some(subject) = atlas_id
        && let Some(record) = st.atlases.get_mut(&subject)
    {
        record.user_id = Uuid::nil();
        record.clan_id = Some(clan);
        if let Some(n) = name.clone() {
            record.name = n;
        }
        record.rev += 1;
    }
    let members: BTreeSet<Uuid> = st.clans.clans[&clan]
        .members
        .keys()
        .copied()
        .filter(|user| active_member(st, *user, clan))
        .collect();
    let now = Utc::now();
    for map in &maps {
        let Some(area) = st.areas.get_mut(map) else {
            continue;
        };
        area.user_id = Uuid::nil();
        area.clan_id = Some(clan);
        // "Stays mine": each map arrives Member-owned, the offerer its
        // only owner.
        if offer.ownership == Some("members") {
            area.member_owned = Some(super::clan_maps::MemberOwnedRecord {
                owners: vec![(from, now)],
                frozen: false,
            });
        }
        if area_id.is_some() {
            area.atlas_id = filed;
            if let Some(n) = name.clone() {
                area.name = n;
            }
        }
        area.rev += 1;
        for secret in &mut area.secrets {
            if secret.clan.is_some() {
                continue;
            }
            // Owner Secrets become Member-owned Clan Secrets: the offerer is
            // their recorded owner and creator. Grants to active members
            // carry, one per member with the union of their actions, from
            // the grantor of the oldest; the rest end.
            let mut grants = std::mem::take(&mut secret.grants);
            grants.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            let mut merged: Vec<ClanSecretGrantRecord> = Vec::new();
            for grant in grants {
                if grant.grantee_id == from || !members.contains(&grant.grantee_id) {
                    continue;
                }
                let recipient = ClanRecipient::User(grant.grantee_id);
                match merged.iter_mut().find(|held| held.recipient == recipient) {
                    Some(held) => held.actions.extend(grant.actions),
                    None => merged.push(ClanSecretGrantRecord {
                        id: grant.id,
                        recipient,
                        actions: grant.actions,
                        grantor_id: grant.grantor_id,
                        created_at: grant.created_at,
                        updated_at: grant.updated_at,
                    }),
                }
            }
            secret.clan = Some(ClanSecretRecord {
                clan_id: clan,
                ownership: "members",
                created_by: from,
                owners: vec![(from, now)],
                grants: merged,
                access_rev: 0,
                frozen: false,
                offers: Vec::new(),
            });
        }
    }
    Ok(())
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

/// `decline` (the recipient, or for a clan any holder of
/// `atlas.accept_transfer` there) or `cancel` (the offerer) — mark the live
/// offer terminal.
fn respond_to_offer(state: &Shared, headers: &HeaderMap, raw: &str, is_cancel: bool) -> Response {
    let mut st = state.lock();
    let caller = match gate(&st, headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(id) = Uuid::parse_str(raw) else {
        return not_found();
    };
    let Some(offer) = st
        .pending_transfers
        .iter()
        .find(|t| t.id == id && t.status == "Offered")
        .cloned()
    else {
        return not_found();
    };
    let allowed = if is_cancel {
        offer.from_user_id == caller
    } else {
        offer.to_user_id == Some(caller)
            || offer.to_clan_id.is_some_and(|clan| {
                active_member(&st, caller, clan) && receives_transfers(&st, clan, caller)
            })
    };
    if !allowed {
        return not_found();
    }
    mark(
        &mut st,
        id,
        if is_cancel { "Cancelled" } else { "Declined" },
    );
    ok(Value::Null)
}

/// Cancel any live offer between the pair, either direction — called
/// from unfriend/block. Mirrors the server's `cancel_live_transfers_between`.
pub fn cancel_live_transfers_between(st: &mut MockState, a: Uuid, b: Uuid) {
    for t in st.pending_transfers.iter_mut().filter(|t| {
        t.status == "Offered"
            && ((t.from_user_id == a && t.to_user_id == Some(b))
                || (t.from_user_id == b && t.to_user_id == Some(a)))
    }) {
        t.status = "Cancelled".to_string();
        t.responded_at = Some(Utc::now());
    }
}

/// Cancels the live offers to a clan: every one when it dissolves, or one
/// member's when they leave or are removed.
pub fn cancel_offers_to_clan(st: &mut MockState, clan: Uuid, from: Option<Uuid>) {
    for t in st.pending_transfers.iter_mut().filter(|t| {
        t.status == "Offered"
            && t.to_clan_id == Some(clan)
            && from.is_none_or(|user| t.from_user_id == user)
    }) {
        t.status = "Cancelled".to_string();
        t.responded_at = Some(Utc::now());
    }
}

/// Cancels the live offers of a map or folder being deleted (the server's
/// `cancelOffersOf`, run as a removal finishes).
pub fn cancel_offers_of(st: &mut MockState, subject: Uuid) {
    for t in st.pending_transfers.iter_mut().filter(|t| {
        t.status == "Offered" && (t.area_id == Some(subject) || t.atlas_id == Some(subject))
    }) {
        t.status = "Cancelled".to_string();
        t.responded_at = Some(Utc::now());
    }
}

/// The exits of `maps`, their own and their Secrets', into rooms of Secrets
/// on maps that do not move with them and that `acceptor` does not read
/// (the server's `unreadSecrets`, judged before anything moves).
fn unread_foreign_exits(st: &MockState, acceptor: Uuid, maps: &[Uuid]) -> BTreeSet<Uuid> {
    maps.iter()
        .filter_map(|map| st.areas.get(map))
        .flat_map(|area| {
            std::iter::once(&area.exits).chain(area.secrets.iter().map(|secret| &secret.exits))
        })
        .flatten()
        .filter(|exit| {
            exit.to_secret.is_some()
                && exit.to_area_id.is_some_and(|to| !maps.contains(&to))
                && !st.shows_exit(acceptor, exit)
        })
        .map(|exit| exit.id)
        .collect()
}

/// Leaves `unread` exits out of a transfer, with the connections they
/// leave without members (docs/format-3.md §5.3; the server's
/// `insertMovedArea`).
fn leave_out_exits(st: &mut MockState, maps: &[Uuid], unread: &BTreeSet<Uuid>) {
    fn leave_out(
        exits: &mut Vec<ExitRecord>,
        connections: &mut Vec<ConnectionRecord>,
        unread: &BTreeSet<Uuid>,
    ) {
        let touched: BTreeSet<Uuid> = exits
            .iter()
            .filter(|exit| unread.contains(&exit.id))
            .map(|exit| exit.connection_id)
            .collect();
        exits.retain(|exit| !unread.contains(&exit.id));
        connections.retain(|connection| {
            !touched.contains(&connection.id)
                || exits.iter().any(|exit| exit.connection_id == connection.id)
        });
    }
    if unread.is_empty() {
        return;
    }
    for map in maps {
        let Some(area) = st.areas.get_mut(map) else {
            continue;
        };
        leave_out(&mut area.exits, &mut area.connections, unread);
        for secret in &mut area.secrets {
            leave_out(&mut secret.exits, &mut secret.connections, unread);
        }
    }
}

/// Seed an offer created before direct clan transfers. This is a fixture,
/// never an HTTP route, so new clients cannot use it to bypass permissions.
impl super::MockHandle {
    pub async fn legacy_clan_transfer(
        &self,
        user: &super::TestUser,
        area: Option<smudgy_cloud::AreaId>,
        atlas: Option<smudgy_cloud::AtlasId>,
        clan: Uuid,
        ownership: smudgy_cloud::clan_maps::MapOwnership,
    ) -> Result<smudgy_cloud::cloud_api::TransferView, smudgy_cloud::CloudError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", user.api_key).parse().unwrap(),
        );
        let response = create_transfer(
            &self.state,
            &headers,
            &json!({"to_clan_id": clan, "ownership": ownership}).to_string(),
            area.map(|a| a.0),
            atlas.map(|a| a.0),
            true,
        );
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        if status >= 400 {
            return Err(smudgy_cloud::CloudError::from_status(
                status,
                body["error"].as_str().unwrap_or_default(),
            ));
        }
        Ok(serde_json::from_value(body["data"].clone()).unwrap())
    }
}
