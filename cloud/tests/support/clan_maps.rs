//! Folders (`/atlases`) for users and clans, clan maps (Clan-owned and
//! Member-owned), a Member-owned map's owners and the ownership offers that
//! change who owns a clan's map, and the copies a member takes when they
//! leave (`/areas/{a}/owners`, `/areas/{a}/ownership-offers`,
//! `/me/area-offers`, `/clans/{c}/area-offers`, `/clans/{c}/copies`).
//! Fidelity reference: the Cloudflare service's docs/clans.md §6–§7 and
//! docs/format-3.md §5.2–§5.4.
//!
//! A Member-owned map is reached only by grants naming it alone, which its
//! active recorded owners write; the clan's owners hold nothing on it, and
//! to anyone who does not read it the clan answers as if it did not exist.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{delete, get, post};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::clans::{
    ClanGrantRecord, ClanGrantScope, ClanRecipient, ClanRecord, ClanResource, ordered,
};
use super::http::{
    Handled, authenticate, bad_request, conflict, created, gate_verified, not_found, ok,
};
use super::mock_server::MockHandle;
use super::state::{AreaRecord, AtlasRecord, Caps, MockState};

pub type Shared = Arc<Mutex<MockState>>;

const NAME_LIMIT: usize = 255;

/// Everything a Member-owned map's active recorded owner holds on it, and
/// everything its own grants may give (docs/clans.md §7.1, §7.4).
pub const MEMBER_OWNED_ACTIONS: [&str; 9] = [
    "area.read",
    "area.add",
    "area.edit",
    "area.remove_content",
    "area.rename",
    "area.refile",
    "area.delete",
    "area.copy",
    "secret.create_member_owned",
];

/// A Member-owned map's recorded owners, oldest first, and whether it is
/// frozen (its last owner's account is gone).
#[derive(Debug, Clone, Default)]
pub struct MemberOwnedRecord {
    pub owners: Vec<(Uuid, DateTime<Utc>)>,
    pub frozen: bool,
}

/// A pending ownership offer on a clan's map.
#[derive(Debug, Clone)]
pub struct AreaOfferRecord {
    pub id: Uuid,
    pub area_id: Uuid,
    pub clan_id: Uuid,
    /// `(user, accepted)`; empty on an offer to the clan.
    pub recipients: Vec<(Uuid, bool)>,
    /// What the map becomes: `"members"` or `"clan"`.
    pub ownership: &'static str,
    pub replace: bool,
    pub initiator_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub seq: u64,
    /// What the offer was made against; an acceptance finding it changed
    /// voids the offer.
    pub snapshot: AccessKey,
}

/// What an offer is made against: the map's ownership, whether it is
/// frozen, its owners, and the grants naming it with when each last changed.
pub type AccessKey = (bool, bool, Vec<Uuid>, Vec<(Uuid, DateTime<Utc>)>);

// ---------------------------------------------------------------------------
// Access
// ---------------------------------------------------------------------------

fn active_clan(st: &MockState, clan: Uuid) -> Option<&ClanRecord> {
    st.clans.clans.get(&clan).filter(|record| !record.dissolved)
}

/// The 409 `clan_dissolving` for a change to a Member-owned map, or to what
/// it holds, while its clan dissolves: its owners' copies are made or being
/// made, and the dissolution deletes it (docs/clans.md §2). `None` for any
/// other map, and for a map that does not exist.
pub fn refused_while_disposing(st: &MockState, area_id: Uuid) -> Option<Response> {
    let area = st.areas.get(&area_id)?;
    let clan = area.clan_id?;
    (area.member_owned.is_some() && active_clan(st, clan).is_some_and(|clan| clan.dissolving))
        .then(|| conflict("clan_dissolving"))
}

/// Where `clan` holds `area`: its folder (`None` for a Member-owned map left
/// unfiled), and whether the map is Member-owned.
pub fn placement(st: &MockState, clan: Uuid, area: Uuid) -> Option<(Option<Uuid>, bool)> {
    st.areas
        .get(&area)
        .filter(|record| record.clan_id == Some(clan))
        .map(|record| (record.atlas_id, record.member_owned.is_some()))
}

/// The map's resource as grant coverage sees it.
pub fn resource(area: &AreaRecord) -> ClanResource {
    ClanResource::Area {
        id: area.id,
        atlas: area.atlas_id,
        member_owned: area.member_owned.is_some(),
    }
}

/// Whether `user` is an active recorded owner of the Member-owned `area`.
pub fn active_owner(st: &MockState, user: Uuid, area: &AreaRecord) -> bool {
    let (Some(record), Some(clan)) = (&area.member_owned, area.clan_id) else {
        return false;
    };
    record.owners.iter().any(|(owner, _)| *owner == user)
        && active_clan(st, clan).is_some_and(|clan| clan.has_member(user))
}

/// Who holds ownership authority over a clan's map: a Member-owned map's
/// active owners (none while it is frozen), a Clan-owned map's clan owners.
pub fn has_authority(st: &MockState, user: Uuid, area: &AreaRecord) -> bool {
    match &area.member_owned {
        Some(record) => !record.frozen && active_owner(st, user, area),
        None => area
            .clan_id
            .and_then(|clan| active_clan(st, clan))
            .is_some_and(|clan| clan.has_owner(user)),
    }
}

/// The caller's clan map actions on a clan's map (the server's
/// `areaActions`); `None` on every other map. A Member-owned map gives its
/// active owners every action, and anyone else what the grants naming it
/// alone give.
pub fn area_actions(st: &MockState, viewer: Uuid, area: &AreaRecord) -> Option<BTreeSet<String>> {
    let clan_id = area.clan_id?;
    let Some(clan) = active_clan(st, clan_id) else {
        return Some(BTreeSet::new());
    };
    let mut actions = if active_owner(st, viewer, area) {
        MEMBER_OWNED_ACTIONS
            .iter()
            .map(ToString::to_string)
            .collect()
    } else {
        clan.actions_on(viewer, resource(area))
    };
    if !actions.contains("area.read") {
        actions.clear();
    }
    Some(actions)
}

/// Whether `viewer` reads the clan's map.
pub fn reads(st: &MockState, viewer: Uuid, area: &AreaRecord) -> bool {
    area_actions(st, viewer, area).is_some_and(|actions| actions.contains("area.read"))
}

/// Folds clan map actions into a viewer's capabilities (the server's
/// `clanAccess`): edit with any content action, copy with `area.copy`.
pub fn fold_actions(caps: &mut Caps, actions: &BTreeSet<String>) {
    if !actions.contains("area.read") {
        return;
    }
    caps.can_view = true;
    caps.can_edit |= ["area.add", "area.edit", "area.remove_content"]
        .iter()
        .any(|action| actions.contains(*action));
    caps.can_copy |= actions.contains("area.copy");
}

/// The owner and clan fields of a map's list row or projection header.
pub fn ownership_fields(st: &MockState, viewer: Uuid, area: &AreaRecord, out: &mut Value) {
    let Some(clan_id) = area.clan_id else {
        return;
    };
    out["user_id"] = Value::Null;
    out["clan_id"] = json!(clan_id);
    match &area.member_owned {
        Some(record) => {
            out["ownership"] = json!("members");
            out["owned_by_me"] = json!(active_owner(st, viewer, area));
            if record.frozen {
                out["frozen"] = json!(true);
            }
        }
        None => out["ownership"] = json!("clan"),
    }
    let actions = area_actions(st, viewer, area).unwrap_or_default();
    if actions.is_empty() && super::shares::outside_reader(st, viewer, area) {
        out["actions"] = json!(["area.read"]);
    } else {
        out["actions"] = json!(ordered(&actions));
    }
}

/// The list-row extras: the clan's name on its maps.
pub fn list_fields(st: &MockState, viewer: Uuid, area: &AreaRecord, out: &mut Value) {
    ownership_fields(st, viewer, area, out);
    if let Some(clan) = area.clan_id.and_then(|clan| st.clans.clans.get(&clan)) {
        out["clan_name"] = json!(clan.name);
    }
}

/// Takes a deleted map out of every grant scope and drops its offers.
pub fn forget_area(st: &mut MockState, area: Uuid) {
    st.area_offers.retain(|offer| offer.area_id != area);
    for clan in st.clans.clans.values_mut() {
        clan.drop_target(area);
    }
    super::shares::sweep_outside_shares(st);
}

/// When a grant scope names a Member-owned map of `clan`: that map. A scope
/// naming one beside other IDs is the caller's to learn of only when they
/// read it.
pub fn member_owned_target(st: &MockState, clan: Uuid, scope: &ClanGrantScope) -> Option<Uuid> {
    let ClanGrantScope::Areas(ids) = scope else {
        return None;
    };
    ids.iter().copied().find(|id| {
        st.areas
            .get(id)
            .is_some_and(|area| area.clan_id == Some(clan) && area.member_owned.is_some())
    })
}

/// Whether `caller` may see a grant that names a Member-owned map: its
/// active owners every one, its recipients their own and their groups'.
pub fn may_see_member_grant(
    st: &MockState,
    clan: &ClanRecord,
    caller: Uuid,
    area: Uuid,
    recipient: ClanRecipient,
) -> bool {
    let Some(record) = st.areas.get(&area) else {
        return false;
    };
    if active_owner(st, caller, record) {
        return true;
    }
    reads(st, caller, record)
        && match recipient {
            ClanRecipient::User(user) => user == caller,
            ClanRecipient::Group(group) => clan.member_groups(caller).contains(&group),
        }
}

/// Checks a grant write on a Member-owned map (docs/clans.md §7.4): only its
/// active owners write, on an unfrozen map, a scope naming it alone, with
/// its own actions; `area.read` is always present. Returns the actions to
/// store.
pub fn member_grant_actions(
    st: &MockState,
    caller: Uuid,
    area: Uuid,
    scope: &ClanGrantScope,
    actions: &BTreeSet<String>,
    may_grant: &BTreeSet<String>,
) -> Result<BTreeSet<String>, Response> {
    let record = st.areas.get(&area).ok_or_else(not_found)?;
    if !reads(st, caller, record) {
        return Err(not_found());
    }
    if !has_authority(st, caller, record) {
        return Err(not_found());
    }
    if scope.ids().is_some_and(|ids| ids.len() != 1) {
        return Err(bad_request("A Member-owned map is granted alone"));
    }
    if !may_grant.is_empty()
        || actions
            .iter()
            .any(|action| !MEMBER_OWNED_ACTIONS.contains(&action.as_str()))
    {
        return Err(bad_request(
            "That action does not apply to a Member-owned map",
        ));
    }
    let mut stored = actions.clone();
    stored.insert("area.read".to_string());
    Ok(stored)
}

// ---------------------------------------------------------------------------
// Request plumbing
// ---------------------------------------------------------------------------

fn respond(handled: Handled) -> Response {
    handled.unwrap_or_else(|response| response)
}

fn verified(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (caller, _) = authenticate(st, headers)?;
    gate_verified(st, caller)?;
    Ok(caller)
}

fn json_object(body: &str) -> Result<Map<String, Value>, Response> {
    if body.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(fields)) => Ok(fields),
        _ => Err(bad_request("expected a JSON object")),
    }
}

fn id_param(raw: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(raw).map_err(|_| not_found())
}

fn optional_uuid(fields: &Map<String, Value>, name: &str) -> Result<Option<Uuid>, Response> {
    match fields.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => Uuid::parse_str(raw)
            .map(Some)
            .map_err(|_| bad_request(&format!("Invalid `{name}`"))),
        Some(_) => Err(bad_request(&format!("Invalid `{name}`"))),
    }
}

fn valid_name(fields: &Map<String, Value>) -> Result<String, Response> {
    let name = fields
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request("missing field `name`"))?;
    if name.chars().count() > NAME_LIMIT || name.contains('\0') {
        return Err(bad_request("Invalid name"));
    }
    Ok(name.to_string())
}

fn nickname(st: &MockState, user: Uuid) -> Option<String> {
    st.user(user).and_then(|record| record.nickname.clone())
}

/// A clan map the caller reads, or the uniform 404.
fn readable_area<'a>(
    st: &'a MockState,
    caller: Uuid,
    raw: &str,
) -> Result<&'a AreaRecord, Response> {
    let area_id = Uuid::parse_str(raw).map_err(|_| not_found())?;
    st.areas
        .get(&area_id)
        .filter(|area| area.clan_id.is_some() && reads(st, caller, area))
        .ok_or_else(not_found)
}

fn ownership_snapshot(st: &MockState, area: &AreaRecord) -> AccessKey {
    let mut owners: Vec<Uuid> = area
        .member_owned
        .as_ref()
        .map(|record| record.owners.iter().map(|(user, _)| *user).collect())
        .unwrap_or_default();
    owners.sort();
    let mut grants: Vec<(Uuid, DateTime<Utc>)> = area
        .clan_id
        .and_then(|clan| st.clans.clans.get(&clan))
        .map(|clan| {
            clan.grants
                .iter()
                .filter(|grant| {
                    matches!(&grant.scope, ClanGrantScope::Areas(ids) if ids.contains(&area.id))
                })
                .map(|grant| (grant.id, grant.updated_at))
                .collect()
        })
        .unwrap_or_default();
    grants.sort();
    (
        area.member_owned.is_some(),
        area.member_owned
            .as_ref()
            .is_some_and(|record| record.frozen),
        owners,
        grants,
    )
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/atlases", get(list_atlases).post(create_atlas))
        .route(
            "/atlases/:atlas_id",
            axum::routing::patch(rename_atlas).delete(delete_atlas),
        )
        .route(
            "/atlases/:atlas_id/local-move",
            post(finish_local_atlas_move),
        )
        .route("/areas/:area_id/owners", get(list_owners))
        .route("/areas/:area_id/owners/:user_id", delete(remove_owner))
        .route(
            "/areas/:area_id/ownership-offers",
            get(list_area_offers).post(make_offer),
        )
        .route(
            "/areas/:area_id/ownership-offers/:offer_id",
            delete(withdraw_offer),
        )
        .route(
            "/areas/:area_id/ownership-offers/:offer_id/accept",
            post(accept_offer),
        )
        .route(
            "/areas/:area_id/ownership-offers/:offer_id/decline",
            post(decline_offer),
        )
        .route("/me/area-offers", get(my_offers))
        .route("/clans/:clan_id/area-offers", get(clan_offers))
        .route("/clans/:clan_id/copies", post(copy_my_maps))
}

// ===== folders ==============================================================

fn atlas_view(atlas: &AtlasRecord) -> Value {
    let mut view = json!({
        "id": atlas.id,
        "user_id": atlas.user_id,
        "name": atlas.name,
        "created_at": atlas.created_at,
        "rev": atlas.rev,
    });
    if let Some(clan) = atlas.clan_id {
        view["user_id"] = Value::Null;
        view["clan_id"] = json!(clan);
    }
    view
}

/// The caller's folders, those they administer, and their clans' folders
/// they hold an action on or read a map in, by name. A clan folder's count
/// is of the maps the caller reads.
pub async fn list_atlases(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let (viewer, _) = authenticate(&st, &headers)?;
        let mut rows: Vec<(String, Uuid, Value)> = Vec::new();
        for atlas in st.atlases.values() {
            if let Some(clan_id) = atlas.clan_id {
                let Some(clan) = active_clan(&st, clan_id) else {
                    continue;
                };
                if !clan.has_member(viewer) {
                    continue;
                }
                let actions = clan.actions_on(viewer, ClanResource::Atlas(atlas.id));
                let readable = st
                    .areas
                    .values()
                    .filter(|area| {
                        area.clan_id == Some(clan_id)
                            && area.atlas_id == Some(atlas.id)
                            && reads(&st, viewer, area)
                    })
                    .count();
                if actions.is_empty() && readable == 0 {
                    continue;
                }
                rows.push((
                    atlas.name.clone(),
                    atlas.id,
                    json!({
                        "id": atlas.id,
                        "name": atlas.name,
                        "created_at": atlas.created_at,
                        "area_count": readable,
                        "rev": atlas.rev,
                        "is_owner": false,
                        "can_admin": actions.contains("atlas.rename")
                            || actions.contains("atlas.delete"),
                        "clan_id": clan_id,
                        "clan_name": clan.name,
                        "actions": ordered(&actions),
                    }),
                ));
                continue;
            }
            let owner = atlas.user_id == viewer;
            let administers = st.grants.iter().any(|grant| {
                grant.grantee_id == viewer && grant.atlas_id == Some(atlas.id) && grant.can_admin
            });
            if !owner && !administers {
                continue;
            }
            let count = st
                .areas
                .values()
                .filter(|area| area.atlas_id == Some(atlas.id))
                .count();
            let mut row = json!({
                "id": atlas.id,
                "name": atlas.name,
                "created_at": atlas.created_at,
                "area_count": count,
                "rev": atlas.rev,
                "is_owner": owner,
                "can_admin": true,
            });
            if !owner && let Some(nickname) = nickname(&st, atlas.user_id) {
                row["owner_nickname"] = json!(nickname);
            }
            rows.push((atlas.name.clone(), atlas.id, row));
        }
        rows.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        Ok(ok(json!(
            rows.into_iter().map(|(_, _, row)| row).collect::<Vec<_>>()
        )))
    })())
}

/// `POST /atlases`, with `clan_id` in the clan (`atlas.create`).
pub async fn create_atlas(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let (viewer, _) = authenticate(&st, &headers)?;
        let fields = json_object(&body)?;
        let name = valid_name(&fields)?;
        let clan = optional_uuid(&fields, "clan_id")?;
        if let Some(clan_id) = clan {
            gate_verified(&st, viewer)?;
            let clan = active_clan(&st, clan_id).ok_or_else(not_found)?;
            if !clan
                .actions_on(viewer, ClanResource::Clan)
                .contains("atlas.create")
            {
                return Err(not_found());
            }
        }
        let atlas = AtlasRecord {
            id: Uuid::new_v4(),
            user_id: if clan.is_some() { Uuid::nil() } else { viewer },
            clan_id: clan,
            name,
            created_at: Utc::now(),
            rev: 1,
        };
        let view = atlas_view(&atlas);
        st.atlases.insert(atlas.id, atlas);
        Ok(created(view))
    })())
}

/// Whether `viewer` holds `action` on the folder: its owner every one, a
/// clan folder's members as the clan's grants give.
fn may_change_atlas(st: &MockState, viewer: Uuid, atlas: &AtlasRecord, action: &str) -> bool {
    match atlas.clan_id {
        Some(clan) => active_clan(st, clan).is_some_and(|clan| {
            clan.actions_on(viewer, ClanResource::Atlas(atlas.id))
                .contains(action)
        }),
        None => atlas.user_id == viewer,
    }
}

pub async fn rename_atlas(
    State(state): State<Shared>,
    Path(raw_atlas): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let atlas_id = id_param(&raw_atlas)?;
        let (viewer, _) = authenticate(&st, &headers)?;
        let fields = json_object(&body)?;
        let name = match fields.get("name") {
            None | Some(Value::Null) => None,
            Some(_) => Some(valid_name(&fields)?),
        };
        let atlas = st.atlases.get(&atlas_id).ok_or_else(not_found)?;
        if !may_change_atlas(&st, viewer, atlas, "atlas.rename") {
            return Err(not_found());
        }
        let atlas = st.atlases.get_mut(&atlas_id).expect("found above");
        if let Some(name) = name
            && name != atlas.name
        {
            atlas.name = name;
            atlas.rev += 1;
        }
        Ok(ok(atlas_view(atlas)))
    })())
}

/// A user's folder goes and its maps stay, unfiled. A clan's folder must
/// hold no Clan-owned map, and no Member-owned map its deleter reads; the
/// Member-owned maps its deleter does not read leave it silently, unfiled,
/// and it leaves every grant scope. Its live transfer offers end.
pub async fn delete_atlas(
    State(state): State<Shared>,
    Path(raw_atlas): Path<String>,
    headers: HeaderMap,
) -> Response {
    remove_atlas(&state, &raw_atlas, &headers, false)
}

pub async fn finish_local_atlas_move(
    State(state): State<Shared>,
    Path(raw_atlas): Path<String>,
    headers: HeaderMap,
) -> Response {
    remove_atlas(&state, &raw_atlas, &headers, true)
}

fn remove_atlas(
    state: &Shared,
    raw_atlas: &str,
    headers: &HeaderMap,
    local_move: bool,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let atlas_id = id_param(raw_atlas)?;
        let (viewer, _) = authenticate(&st, headers)?;
        let atlas = st.atlases.get(&atlas_id).ok_or_else(not_found)?;
        if !may_change_atlas(&st, viewer, atlas, "atlas.delete") {
            return Err(not_found());
        }
        if local_move {
            if atlas.clan_id.is_some() || atlas.user_id != viewer {
                return Err(not_found());
            }
            if st
                .areas
                .values()
                .any(|area| area.atlas_id == Some(atlas_id))
            {
                return Err(conflict("atlas_not_empty"));
            }
        }
        if let Some(clan_id) = atlas.clan_id {
            let blocked = st.areas.values().any(|area| {
                area.clan_id == Some(clan_id)
                    && area.atlas_id == Some(atlas_id)
                    && (area.member_owned.is_none() || reads(&st, viewer, area))
            });
            if blocked {
                return Err(conflict("atlas_not_empty"));
            }
            for area in st.areas.values_mut() {
                if area.clan_id == Some(clan_id) && area.atlas_id == Some(atlas_id) {
                    area.atlas_id = None;
                }
            }
            if let Some(clan) = st.clans.clans.get_mut(&clan_id) {
                clan.drop_target(atlas_id);
            }
            super::shares::sweep_outside_shares(&mut st);
        } else {
            let doomed: Vec<Uuid> = st
                .grants
                .iter()
                .filter(|grant| grant.atlas_id == Some(atlas_id))
                .map(|grant| grant.id)
                .collect();
            st.delete_grants_cascading(&doomed);
            for area in st.areas.values_mut() {
                if area.atlas_id == Some(atlas_id) {
                    area.atlas_id = None;
                    area.rev += 1;
                }
            }
        }
        st.atlases.remove(&atlas_id);
        super::transfers::cancel_offers_of(&mut st, atlas_id);
        Ok(ok(Value::Null))
    })())
}

// ===== clan maps ============================================================

/// `POST /areas` with `clan_id`: a map in one of the clan's folders.
/// Clan-owned (the default) needs `area.create` there and gives its creator
/// Editor on it; Member-owned needs `area.create_member_owned` and makes the
/// creator its first owner.
pub fn create_clan_area(
    st: &mut MockState,
    viewer: Uuid,
    clan_id: Uuid,
    atlas_id: Option<Uuid>,
    name: String,
    ownership: Option<&str>,
) -> Result<Uuid, Response> {
    gate_verified(st, viewer)?;
    let Some(atlas_id) = atlas_id else {
        return Err(bad_request(
            "A clan's map is created in one of its atlases: atlas_id is required",
        ));
    };
    let member_owned = match ownership {
        None | Some("clan") => false,
        Some("members") => true,
        Some(_) => return Err(bad_request("`ownership` is `clan` or `members`")),
    };
    let clan = active_clan(st, clan_id).ok_or_else(not_found)?;
    let in_clan = st
        .atlases
        .get(&atlas_id)
        .is_some_and(|atlas| atlas.clan_id == Some(clan_id));
    let needed = if member_owned {
        "area.create_member_owned"
    } else {
        "area.create"
    };
    if !in_clan
        || !clan
            .actions_on(viewer, ClanResource::Atlas(atlas_id))
            .contains(needed)
    {
        return Err(not_found());
    }
    if clan.dissolving {
        return Err(conflict("clan_dissolving"));
    }
    let seq = st.next_seq();
    let mut area = AreaRecord::new(Uuid::new_v4(), Uuid::nil(), Some(atlas_id), name, seq);
    area.clan_id = Some(clan_id);
    if member_owned {
        area.member_owned = Some(MemberOwnedRecord {
            owners: vec![(viewer, Utc::now())],
            frozen: false,
        });
    }
    let id = area.id;
    st.areas.insert(id, area);
    // A Clan-owned map's creator starts as Editor on it, through a grant
    // naming that map alone (docs/clans.md §6).
    if !member_owned {
        let seq = st.next_seq();
        let now = Utc::now();
        if let Some(clan) = st.clans.clans.get_mut(&clan_id) {
            clan.grants.push(ClanGrantRecord {
                id: Uuid::new_v4(),
                recipient: ClanRecipient::User(viewer),
                scope: ClanGrantScope::Areas(BTreeSet::from([id])),
                actions: CREATOR_ACTIONS.iter().map(ToString::to_string).collect(),
                may_grant: BTreeSet::new(),
                issuer_id: viewer,
                parent_id: None,
                created_at: now,
                updated_at: now,
                seq,
            });
        }
    }
    Ok(id)
}

/// What a Clan-owned map's creator starts with: Editor.
const CREATOR_ACTIONS: [&str; 4] = ["area.read", "area.add", "area.edit", "area.remove_content"];

/// `PUT /areas/{id}` on a clan map: renaming needs `area.rename`; moving it
/// to another of the clan's folders `area.refile` and `atlas.accept_filing`
/// there. `atlas_id: null` is a 400.
pub fn may_update_clan_area(
    st: &MockState,
    viewer: Uuid,
    area: &AreaRecord,
    renames: bool,
    atlas: Option<Option<Uuid>>,
) -> Result<(), Response> {
    let Some(clan_id) = area.clan_id else {
        return Ok(());
    };
    let clan = active_clan(st, clan_id).ok_or_else(not_found)?;
    let held = area_actions(st, viewer, area).unwrap_or_default();
    if !held.contains("area.read") {
        return Err(not_found());
    }
    if renames && !held.contains("area.rename") {
        return Err(not_found());
    }
    match atlas {
        None => Ok(()),
        Some(None) => Err(bad_request("A clan's map stays in one of its atlases")),
        Some(Some(target)) if Some(target) == area.atlas_id => Ok(()),
        Some(Some(target)) => {
            let in_clan = st
                .atlases
                .get(&target)
                .is_some_and(|folder| folder.clan_id == Some(clan_id));
            if in_clan
                && held.contains("area.refile")
                && clan
                    .actions_on(viewer, ClanResource::Atlas(target))
                    .contains("atlas.accept_filing")
            {
                Ok(())
            } else {
                Err(not_found())
            }
        }
    }
}

// ===== owners ===============================================================

pub async fn list_owners(
    State(state): State<Shared>,
    Path(raw_area): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let area = readable_area(&st, viewer, &raw_area)?;
        let clan = area.clan_id.and_then(|clan| active_clan(&st, clan));
        let rows: Vec<Value> = area
            .member_owned
            .iter()
            .flat_map(|record| &record.owners)
            .map(|(user, added_at)| {
                json!({
                    "user_id": user,
                    "nickname": nickname(&st, *user),
                    "active": clan.is_some_and(|clan| clan.has_member(*user)),
                    "added_at": added_at,
                })
            })
            .collect();
        Ok(ok(json!(rows)))
    })())
}

pub async fn remove_owner(
    State(state): State<Shared>,
    Path((raw_area, raw_user)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let user = id_param(&raw_user)?;
        let area = readable_area(&st, viewer, &raw_area)?;
        let area_id = area.id;
        let record = area.member_owned.as_ref().ok_or_else(not_found)?;
        if record.frozen
            || !active_owner(&st, viewer, area)
            || !record.owners.iter().any(|(owner, _)| *owner == user)
        {
            return Err(not_found());
        }
        // While the clan dissolves, the owners its copies go to are fixed.
        if let Some(dissolving) = refused_while_disposing(&st, area_id) {
            return Err(dissolving);
        }
        if record.owners.len() == 1 {
            return Err(conflict("last_owner"));
        }
        let record = st
            .areas
            .get_mut(&area_id)
            .and_then(|area| area.member_owned.as_mut())
            .expect("found above");
        record.owners.retain(|(owner, _)| *owner != user);
        Ok(ok(Value::Null))
    })())
}

// ===== ownership offers =====================================================

fn offer_view(st: &MockState, offer: &AreaOfferRecord) -> Value {
    let area_name = st
        .areas
        .get(&offer.area_id)
        .map(|area| area.name.clone())
        .unwrap_or_default();
    let recipients: Vec<Value> = offer
        .recipients
        .iter()
        .map(|(user, accepted)| {
            let mut row = json!({ "user_id": user, "accepted": accepted });
            if let Some(name) = nickname(st, *user) {
                row["nickname"] = json!(name);
            }
            row
        })
        .collect();
    let mut view = json!({
        "id": offer.id,
        "area_id": offer.area_id,
        "area_name": area_name,
        "clan_id": offer.clan_id,
        "recipients": recipients,
        "ownership": offer.ownership,
        "replace": offer.replace,
        "initiator_id": offer.initiator_id,
        "created_at": offer.created_at,
    });
    if let Some(name) = nickname(st, offer.initiator_id) {
        view["initiator_nickname"] = json!(name);
    }
    view
}

fn offers_view<'a>(st: &MockState, offers: impl Iterator<Item = &'a AreaOfferRecord>) -> Value {
    let mut offers: Vec<&AreaOfferRecord> = offers.collect();
    offers.sort_by_key(|offer| (offer.created_at, offer.seq));
    json!(
        offers
            .into_iter()
            .map(|offer| offer_view(st, offer))
            .collect::<Vec<_>>()
    )
}

/// Whether `user` may accept offers to make `clan`'s maps Clan-owned
/// somewhere: its owners, and holders of `atlas.accept_transfer`.
fn accepts_for_clan(clan: &ClanRecord, user: Uuid) -> bool {
    clan.has_owner(user) || clan.holds_anywhere(user, "atlas.accept_transfer")
}

fn recipients_of(fields: &Map<String, Value>) -> Result<Vec<Uuid>, Response> {
    let mut users = Vec::new();
    if let Some(value) = fields.get("user_id")
        && !value.is_null()
    {
        let raw = value
            .as_str()
            .ok_or_else(|| bad_request("Invalid `user_id`"))?;
        users.push(Uuid::parse_str(raw).map_err(|_| bad_request("Invalid `user_id`"))?);
    }
    match fields.get("user_ids") {
        None | Some(Value::Null) => {}
        Some(Value::Array(values)) => {
            for value in values {
                let raw = value
                    .as_str()
                    .ok_or_else(|| bad_request("Invalid `user_ids`"))?;
                let user = Uuid::parse_str(raw).map_err(|_| bad_request("Invalid `user_ids`"))?;
                if users.contains(&user) {
                    return Err(bad_request("`user_ids` are distinct"));
                }
                users.push(user);
            }
        }
        Some(_) => return Err(bad_request("Invalid `user_ids`")),
    }
    if users.len() > 16 {
        return Err(bad_request("an offer names at most 16 users"));
    }
    Ok(users)
}

pub async fn make_offer(
    State(state): State<Shared>,
    Path(raw_area): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let fields = json_object(&body)?;
        let users = recipients_of(&fields)?;
        let ownership = match fields.get("ownership").and_then(Value::as_str) {
            Some("members") => "members",
            Some("clan") => "clan",
            _ => return Err(bad_request("`ownership` is `members` or `clan`")),
        };
        let replace = fields
            .get("replace")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let area = readable_area(&st, viewer, &raw_area)?;
        let clan_id = area.clan_id.expect("a clan map");
        let clan = active_clan(&st, clan_id).ok_or_else(not_found)?;
        let member_owned = area.member_owned.is_some();
        if !has_authority(&st, viewer, area) {
            return Err(not_found());
        }
        let to_clan = ownership == "clan";
        if to_clan != users.is_empty() {
            return Err(bad_request("an offer to the clan names nobody"));
        }
        if to_clan && !member_owned {
            return Err(not_found());
        }
        if !to_clan {
            if !member_owned && replace {
                return Err(bad_request("`replace` applies only between members"));
            }
            let owners: Vec<Uuid> = area
                .member_owned
                .iter()
                .flat_map(|record| record.owners.iter().map(|(user, _)| *user))
                .collect();
            let eligible = users.iter().all(|user| {
                clan.has_member(*user) && reads(&st, *user, area) && !owners.contains(user)
            });
            if !eligible {
                return Err(not_found());
            }
        }
        let area_id = area.id;
        let snapshot = ownership_snapshot(&st, area);
        let seq = st.next_seq();
        st.area_offers.retain(|offer| {
            offer.area_id != area_id
                || !(if to_clan {
                    offer.recipients.is_empty()
                } else {
                    offer
                        .recipients
                        .iter()
                        .any(|(user, _)| users.contains(user))
                })
        });
        let offer = AreaOfferRecord {
            id: Uuid::new_v4(),
            area_id,
            clan_id,
            recipients: users.into_iter().map(|user| (user, false)).collect(),
            ownership,
            replace,
            initiator_id: viewer,
            created_at: Utc::now(),
            seq,
            snapshot,
        };
        let view = offer_view(&st, &offer);
        st.area_offers.push(offer);
        Ok(created(view))
    })())
}

pub async fn list_area_offers(
    State(state): State<Shared>,
    Path(raw_area): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let area = readable_area(&st, viewer, &raw_area)?;
        let authority = has_authority(&st, viewer, area);
        let offers = st.area_offers.iter().filter(|offer| {
            offer.area_id == area.id
                && (authority || offer.recipients.iter().any(|(user, _)| *user == viewer))
        });
        Ok(ok(offers_view(&st, offers)))
    })())
}

pub async fn my_offers(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        // The standing offers naming the caller on maps they read.
        let offers = st.area_offers.iter().filter(|offer| {
            offer.recipients.iter().any(|(user, _)| *user == viewer)
                && active_clan(&st, offer.clan_id).is_some_and(|clan| clan.has_member(viewer))
                && st
                    .areas
                    .get(&offer.area_id)
                    .is_some_and(|area| reads(&st, viewer, area))
        });
        Ok(ok(offers_view(&st, offers)))
    })())
}

pub async fn clan_offers(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let clan_id = id_param(&raw_clan)?;
        let clan = active_clan(&st, clan_id).ok_or_else(not_found)?;
        if !accepts_for_clan(clan, viewer) {
            return Err(not_found());
        }
        let offers = st
            .area_offers
            .iter()
            .filter(|offer| offer.clan_id == clan_id && offer.recipients.is_empty());
        Ok(ok(offers_view(&st, offers)))
    })())
}

/// Whether an offer still stands: its map unchanged since, its initiator
/// still holding what making it needed, and each recipient still eligible.
fn offer_stands(st: &MockState, offer: &AreaOfferRecord) -> bool {
    let Some(area) = st.areas.get(&offer.area_id) else {
        return false;
    };
    let Some(clan) = active_clan(st, offer.clan_id) else {
        return false;
    };
    if ownership_snapshot(st, area) != offer.snapshot
        || !has_authority(st, offer.initiator_id, area)
    {
        return false;
    }
    offer
        .recipients
        .iter()
        .all(|(user, _)| clan.has_member(*user) && reads(st, *user, area))
}

fn offer_index(st: &MockState, area: Uuid, raw_offer: &str) -> Result<usize, Response> {
    let offer_id = id_param(raw_offer)?;
    st.area_offers
        .iter()
        .position(|offer| offer.id == offer_id && offer.area_id == area)
        .ok_or_else(not_found)
}

/// Applies an accepted offer to its map.
fn apply_offer(st: &mut MockState, offer: &AreaOfferRecord, atlas: Option<Uuid>) {
    let now = Utc::now();
    let clan_id = offer.clan_id;
    let Some(area) = st.areas.get_mut(&offer.area_id) else {
        return;
    };
    let area_id = area.id;
    let recipients: Vec<Uuid> = offer.recipients.iter().map(|(user, _)| *user).collect();
    if offer.ownership == "clan" {
        // Make Clan-owned: its owners clear and it is filed where accepted.
        area.member_owned = None;
        area.atlas_id = atlas.or(area.atlas_id);
        return;
    }
    match &mut area.member_owned {
        Some(record) => {
            if offer.replace {
                record.owners.clear();
            }
            for user in recipients {
                if !record.owners.iter().any(|(owner, _)| *owner == user) {
                    record.owners.push((user, now));
                }
            }
        }
        None => {
            // Back to members: clan-wide and folder grants stop reaching it;
            // its own grants keep what a Member-owned map allows; its
            // Clan-owned Secrets become Member-owned with the new owners.
            area.member_owned = Some(MemberOwnedRecord {
                owners: recipients.iter().map(|user| (*user, now)).collect(),
                frozen: false,
            });
            for secret in &mut area.secrets {
                if let Some(record) = secret.clan.as_mut()
                    && record.ownership == "clan"
                {
                    record.ownership = "members";
                    record.owners = recipients.iter().map(|user| (*user, now)).collect();
                    record.access_rev += 1;
                }
            }
            if let Some(clan) = st.clans.clans.get_mut(&clan_id) {
                clan.narrow_to_member_owned(area_id, &MEMBER_OWNED_ACTIONS);
            }
        }
    }
    super::shares::sweep_outside_shares(st);
}

pub async fn accept_offer(
    State(state): State<Shared>,
    Path((raw_area, raw_offer)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let area_id = Uuid::parse_str(&raw_area).map_err(|_| not_found())?;
        let fields = json_object(&body)?;
        let index = offer_index(&st, area_id, &raw_offer)?;
        let offer = st.area_offers[index].clone();
        let clan = active_clan(&st, offer.clan_id).ok_or_else(not_found)?;
        if offer.recipients.is_empty() {
            if !accepts_for_clan(clan, viewer) {
                return Err(not_found());
            }
            let atlas = optional_uuid(&fields, "atlas_id")?
                .ok_or_else(|| bad_request("missing field `atlas_id`"))?;
            let in_clan = st
                .atlases
                .get(&atlas)
                .is_some_and(|folder| folder.clan_id == Some(offer.clan_id));
            if !in_clan
                || !(clan.has_owner(viewer)
                    || clan
                        .actions_on(viewer, ClanResource::Atlas(atlas))
                        .contains("atlas.accept_transfer"))
            {
                return Err(not_found());
            }
            if !offer_stands(&st, &offer) {
                st.area_offers.remove(index);
                return Err(not_found());
            }
            if active_clan(&st, offer.clan_id).is_some_and(|clan| clan.dissolving) {
                return Err(conflict("clan_dissolving"));
            }
            st.area_offers.remove(index);
            apply_offer(&mut st, &offer, Some(atlas));
            return Ok(ok(
                json!({ "area_id": area_id, "ownership": "clan", "atlas_id": atlas }),
            ));
        }
        if !offer.recipients.iter().any(|(user, _)| *user == viewer) {
            return Err(not_found());
        }
        // A dissolving clan's maps change hands no more.
        if clan.dissolving {
            return Err(conflict("clan_dissolving"));
        }
        if !offer_stands(&st, &offer) {
            st.area_offers.remove(index);
            return Err(not_found());
        }
        let record = &mut st.area_offers[index];
        for (user, accepted) in &mut record.recipients {
            if *user == viewer {
                *accepted = true;
            }
        }
        let complete = record.recipients.iter().all(|(_, accepted)| *accepted);
        if complete {
            let offer = st.area_offers.remove(index);
            apply_offer(&mut st, &offer, None);
        }
        let area = st.areas.get(&area_id).ok_or_else(not_found)?;
        let ownership = if area.member_owned.is_some() {
            "members"
        } else {
            "clan"
        };
        Ok(ok(
            json!({ "area_id": area_id, "ownership": ownership, "atlas_id": area.atlas_id }),
        ))
    })())
}

pub async fn decline_offer(
    State(state): State<Shared>,
    Path((raw_area, raw_offer)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let area_id = Uuid::parse_str(&raw_area).map_err(|_| not_found())?;
        let index = offer_index(&st, area_id, &raw_offer)?;
        let offer = &st.area_offers[index];
        let clan = active_clan(&st, offer.clan_id).ok_or_else(not_found)?;
        // A named recipient declines only a map they still read, as the
        // offer lists only to them then.
        let allowed = if offer.recipients.is_empty() {
            accepts_for_clan(clan, viewer)
        } else {
            offer.recipients.iter().any(|(user, _)| *user == viewer)
                && st
                    .areas
                    .get(&area_id)
                    .is_some_and(|area| reads(&st, viewer, area))
        };
        if !allowed {
            return Err(not_found());
        }
        st.area_offers.remove(index);
        Ok(ok(Value::Null))
    })())
}

pub async fn withdraw_offer(
    State(state): State<Shared>,
    Path((raw_area, raw_offer)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let area = readable_area(&st, viewer, &raw_area)?;
        if !has_authority(&st, viewer, area) {
            return Err(not_found());
        }
        let index = offer_index(&st, area.id, &raw_offer)?;
        st.area_offers.remove(index);
        Ok(ok(Value::Null))
    })())
}

// ===== departure copies =====================================================

/// Why an owner copy is made: its ID derives from it, so each copy is made
/// once (docs/clans.md §7.6; the service derives the ID by HMAC under the
/// clan's copy key, which the mock stands in for with the clan's ID).
#[derive(Debug, Clone, Copy)]
pub enum CopyFor {
    /// `POST /clans/{c}/copies`, once per membership: when the member
    /// joined is part of the ID.
    Leaving(DateTime<Utc>),
    /// A removal, once per departure.
    Removal(DateTime<Utc>),
    Dissolution,
}

fn copy_id(clan: Uuid, area: Uuid, owner: Uuid, purpose: CopyFor) -> Uuid {
    let what = match purpose {
        CopyFor::Leaving(joined) => format!("leaving\0{}", joined.timestamp_micros()),
        CopyFor::Removal(joined) => format!("removal\0{}", joined.timestamp_micros()),
        CopyFor::Dissolution => "dissolution".to_string(),
    };
    Uuid::new_v5(&clan, format!("{what}\0{area}\0{owner}").as_bytes())
}

/// An owner's copies of `maps`, each made once (docs/clans.md §7.6): none
/// for an account being deleted; an exit into another of these maps leads
/// into its copy, and an exit into a map the owner reads elsewhere stays,
/// while the rest dangle, as any copy's do. Returns the `(copy, original)`
/// pairs this call made.
pub fn owner_copies(
    st: &mut MockState,
    clan: Uuid,
    owner: Uuid,
    maps: &[Uuid],
    purpose: CopyFor,
) -> Vec<(Uuid, Uuid)> {
    if st.deleting_accounts.contains(&owner) {
        return Vec::new();
    }
    let remap: HashMap<Uuid, Uuid> = maps
        .iter()
        .map(|area| (*area, copy_id(clan, *area, owner, purpose)))
        .collect();
    let mut made = Vec::new();
    for area in maps {
        let copy = remap[area];
        if st.areas.contains_key(&copy) {
            continue;
        }
        if departure_copy(st, owner, *area, copy, &remap) {
            made.push((copy, *area));
        }
    }
    made
}

/// Copies a Member-owned map into `owner`'s own maps as `copy_id`, unfiled
/// and named as a copy is: the map whole, their own Private additions, and
/// of its Clan Secrets those they own, as owner Secrets of the copy. Exits
/// follow [`owner_copies`]. Whether the map was there to copy.
fn departure_copy(
    st: &mut MockState,
    owner: Uuid,
    area_id: Uuid,
    copy_id: Uuid,
    remap: &HashMap<Uuid, Uuid>,
) -> bool {
    let Some(source) = st.areas.get(&area_id).cloned() else {
        return false;
    };
    let seq = st.next_seq();
    let mut header = AreaRecord::new(copy_id, owner, None, format!("{} (copy)", source.name), seq);
    header.copied_from_area_id = Some(area_id);
    header.copied_from_rev = Some(source.rev);
    header.copied_at = Some(Utc::now());
    header.properties.clone_from(&source.properties);
    header.rooms.clone_from(&source.rooms);
    header.labels.clone_from(&source.labels);
    header.shapes.clone_from(&source.shapes);
    for exit in &source.exits {
        if exit.to_secret.is_some() {
            continue;
        }
        let mut copied = exit.clone();
        copied.id = Uuid::new_v4();
        if let Some(target) = exit.to_area_id {
            if let Some(mapped) = remap.get(&target) {
                copied.to_area_id = Some(*mapped);
            } else if !st.caps(owner, target).is_some_and(|caps| caps.can_view) {
                copied.to_area_id = None;
                copied.to_room_number = None;
                copied.to_direction = None;
            }
        }
        header.exits.push(copied);
    }
    header.connections.clone_from(&source.connections);
    for secret in &source.secrets {
        let owned = secret.clan.as_ref().is_some_and(|record| {
            record.ownership == "members" && record.owners.iter().any(|(user, _)| *user == owner)
        });
        if owned {
            let mut copied = super::state::SecretRecord::new(Uuid::new_v4(), secret.name.clone());
            copied.color.clone_from(&secret.color);
            copied.properties.clone_from(&secret.properties);
            copied.rooms.clone_from(&secret.rooms);
            copied.labels.clone_from(&secret.labels);
            copied.shapes.clone_from(&secret.shapes);
            header.secrets.push(copied);
        }
    }
    st.areas.insert(copy_id, header);
    true
}

/// The Member-owned maps of `clan` whose recorded owners include `user`.
pub fn owned_maps(st: &MockState, clan: Uuid, user: Uuid) -> Vec<Uuid> {
    st.areas
        .values()
        .filter(|area| {
            area.clan_id == Some(clan)
                && area
                    .member_owned
                    .as_ref()
                    .is_some_and(|record| record.owners.iter().any(|(owner, _)| *owner == user))
        })
        .map(|area| area.id)
        .collect()
}

/// A member's departure (docs/clans.md §1.4): the ownership offers they made
/// or received are withdrawn; their recorded ownership stays, dormant; a
/// removed member's Member-owned maps are copied into their own maps.
pub fn on_departure(st: &mut MockState, clan: Uuid, user: Uuid, removed: Option<DateTime<Utc>>) {
    st.area_offers.retain(|offer| {
        offer.clan_id != clan
            || (offer.initiator_id != user && !offer.recipients.iter().any(|(u, _)| *u == user))
    });
    if let Some(joined) = removed {
        let maps = owned_maps(st, clan, user);
        owner_copies(st, clan, user, &maps, CopyFor::Removal(joined));
    }
}

/// Account deletion (docs/clans.md §7.6): the account's recorded ownership
/// goes, and a Member-owned map left with no recorded owner freezes; the
/// offers it made or received go.
pub fn forget_account(st: &mut MockState, user: Uuid) {
    st.area_offers.retain(|offer| {
        offer.initiator_id != user && !offer.recipients.iter().any(|(u, _)| *u == user)
    });
    for area in st.areas.values_mut() {
        if let Some(record) = &mut area.member_owned
            && record.owners.iter().any(|(owner, _)| *owner == user)
        {
            record.owners.retain(|(owner, _)| *owner != user);
            if record.owners.is_empty() {
                record.frozen = true;
            }
        }
    }
}

/// Dissolution: each Member-owned map's active recorded owners get a copy,
/// each owner's maps together, and only then do the maps go.
pub fn on_dissolution(st: &mut MockState, clan: Uuid, active: &[Uuid]) {
    let maps: Vec<(Uuid, Vec<Uuid>)> = st
        .areas
        .values()
        .filter(|area| area.clan_id == Some(clan))
        .filter_map(|area| {
            area.member_owned.as_ref().map(|record| {
                (
                    area.id,
                    record
                        .owners
                        .iter()
                        .map(|(user, _)| *user)
                        .filter(|user| active.contains(user))
                        .collect(),
                )
            })
        })
        .collect();
    let mut by_owner: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
    for (area, owners) in &maps {
        for owner in owners {
            by_owner.entry(*owner).or_default().push(*area);
        }
    }
    for (owner, areas) in by_owner {
        owner_copies(st, clan, owner, &areas, CopyFor::Dissolution);
    }
    for (area, _) in maps {
        super::areas::remove_area(st, area);
    }
    st.area_offers.retain(|offer| offer.clan_id != clan);
}

pub async fn copy_my_maps(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let viewer = verified(&st, &headers)?;
        let clan_id = id_param(&raw_clan)?;
        let clan = active_clan(&st, clan_id).ok_or_else(not_found)?;
        if !clan.has_member(viewer) {
            return Err(not_found());
        }
        // Each map is copied once per membership: a repeat lists only the
        // copies it made itself.
        let joined = clan.members[&viewer].joined_at;
        let maps = owned_maps(&st, clan_id, viewer);
        let made = owner_copies(&mut st, clan_id, viewer, &maps, CopyFor::Leaving(joined));
        let mut copies = Vec::new();
        for (copy, area) in made {
            copies.push(json!({
                "area_id": copy,
                "name": st.areas[&copy].name,
                "copied_from": area,
            }));
        }
        Ok(created(json!({ "copies": copies })))
    })())
}

// ---------------------------------------------------------------------------
// Test seeding
// ---------------------------------------------------------------------------

impl MockHandle {
    /// Creates a folder in a clan directly.
    pub fn create_clan_atlas(&self, clan: Uuid, name: &str) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        st.atlases.insert(
            id,
            AtlasRecord {
                id,
                user_id: Uuid::nil(),
                clan_id: Some(clan),
                name: name.to_string(),
                created_at: Utc::now(),
                rev: 1,
            },
        );
        id
    }

    /// Creates a Clan-owned map in a clan folder directly.
    pub fn create_clan_area(&self, clan: Uuid, atlas: Uuid, name: &str) -> smudgy_cloud::AreaId {
        let mut st = self.state.lock();
        let seq = st.next_seq();
        let mut area = AreaRecord::new(Uuid::new_v4(), Uuid::nil(), Some(atlas), name.into(), seq);
        area.clan_id = Some(clan);
        let id = area.id;
        st.areas.insert(id, area);
        smudgy_cloud::AreaId(id)
    }

    /// Creates a Member-owned map in a clan folder directly, owned by
    /// `owners`.
    pub fn create_member_owned_area(
        &self,
        clan: Uuid,
        atlas: Uuid,
        name: &str,
        owners: &[Uuid],
    ) -> smudgy_cloud::AreaId {
        let mut st = self.state.lock();
        let seq = st.next_seq();
        let mut area = AreaRecord::new(Uuid::new_v4(), Uuid::nil(), Some(atlas), name.into(), seq);
        area.clan_id = Some(clan);
        area.member_owned = Some(MemberOwnedRecord {
            owners: owners.iter().map(|user| (*user, Utc::now())).collect(),
            frozen: false,
        });
        let id = area.id;
        st.areas.insert(id, area);
        smudgy_cloud::AreaId(id)
    }

    /// A clan map's recorded owners; `None` for a Clan-owned map.
    pub fn map_owners(&self, area: smudgy_cloud::AreaId) -> Option<Vec<Uuid>> {
        self.state
            .lock()
            .areas
            .get(&area.0)
            .and_then(|area| area.member_owned.as_ref())
            .map(|record| record.owners.iter().map(|(user, _)| *user).collect())
    }

    /// Freezes a Member-owned map, as its last owner's account deletion
    /// does.
    pub fn freeze_map(&self, area: smudgy_cloud::AreaId) {
        let mut st = self.state.lock();
        if let Some(record) = st
            .areas
            .get_mut(&area.0)
            .and_then(|area| area.member_owned.as_mut())
        {
            record.owners.clear();
            record.frozen = true;
        }
    }
}
