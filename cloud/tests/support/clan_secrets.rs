//! Clan Secrets: creation (`POST /areas/{a}/secrets` with an `ownership`),
//! who has access (`GET /secrets/{s}/access`), recorded
//! owners, ownership offers (joint ones included), departure and frozen
//! Secrets. Fidelity reference: the smudgy-cloudflare service
//! (`src/library/secrets/*.ts`, `src/secrets/routes.ts`, docs/clans.md §8).
//!
//! Non-disclosure as the server's: a Secret the caller cannot read, an
//! action they do not hold, and an offer that does not name them are the
//! same 404 as something that does not exist. A Clan Secret's grants are
//! written at its creation (a Clan-owned Secret's creator starts as a
//! Contributor), by a transfer into the clan, and through the
//! `/secrets/{s}/grants` routes (`clan_secret_grants.rs`).

use std::collections::BTreeSet;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{delete, get, post};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::clan_maps::placement;
use super::clans::{ClanRecipient, ClanRecord};
use super::http::{
    authenticate, bad_request, conflict, created, email_not_verified, not_found, ok,
};
use super::secrets::Shared;
use super::state::{AreaRecord, MockState, SecretRecord};

/// Every action on a Secret, in the order responses list them.
pub const SECRET_ACTIONS: [&str; 9] = [
    "read",
    "add",
    "edit",
    "remove",
    "manage_access",
    "copy",
    "rename",
    "delete",
    "manage_ownership",
];

/// The clan actions that reach Clan-owned Secrets on a map, with the Secret
/// action each gives.
const COLLECTION: [(&str, &str); 6] = [
    ("secret.read", "read"),
    ("secret.add", "add"),
    ("secret.edit", "edit"),
    ("secret.remove_content", "remove"),
    ("secret.manage_access", "manage_access"),
    ("secret.copy", "copy"),
];

/// An offer names at most this many users.
const MAX_RECIPIENTS: usize = 16;

/// What makes a Secret a Clan Secret: its clan, who holds authority over
/// it, its grants and its pending ownership offers.
#[derive(Debug, Clone)]
pub struct ClanSecretRecord {
    pub clan_id: Uuid,
    /// `"members"` or `"clan"`.
    pub ownership: &'static str,
    pub created_by: Uuid,
    /// Recorded owners of a Member-owned Secret, oldest first.
    pub owners: Vec<(Uuid, DateTime<Utc>)>,
    /// The Secret's grants, oldest first.
    pub grants: Vec<ClanSecretGrantRecord>,
    /// Moves with every change to the grants or owners; offers stand only
    /// at the revision they were made at.
    pub access_rev: u64,
    /// Read-only for everyone, with nobody holding ownership authority.
    pub frozen: bool,
    pub offers: Vec<OfferRecord>,
}

#[derive(Debug, Clone)]
pub struct ClanSecretGrantRecord {
    pub id: Uuid,
    pub recipient: ClanRecipient,
    /// Beyond `read`.
    pub actions: BTreeSet<&'static str>,
    pub grantor_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct OfferRecord {
    pub id: Uuid,
    /// In the order the offer lists them, with when each accepted.
    pub recipients: Vec<(Uuid, Option<DateTime<Utc>>)>,
    pub ownership: &'static str,
    pub replace: bool,
    pub initiator_id: Uuid,
    pub access_rev: u64,
    pub created_at: DateTime<Utc>,
}

fn ordered(actions: &BTreeSet<&'static str>) -> Vec<&'static str> {
    SECRET_ACTIONS
        .iter()
        .copied()
        .filter(|action| actions.contains(action))
        .collect()
}

fn flagged(extra: &BTreeSet<&'static str>) -> BTreeSet<&'static str> {
    let mut all = extra.clone();
    all.insert("read");
    all
}

fn active_clan(st: &MockState, clan: Uuid) -> Option<&ClanRecord> {
    st.clans.clans.get(&clan).filter(|record| !record.dissolved)
}

/// `user`'s clan actions on `area`; empty when the map is not the clan's.
fn map_actions(st: &MockState, clan: &ClanRecord, user: Uuid, area: Uuid) -> BTreeSet<String> {
    if placement(st, clan.id, area).is_none() {
        return BTreeSet::new();
    }
    st.areas
        .get(&area)
        .and_then(|record| super::clan_maps::area_actions(st, user, record))
        .unwrap_or_default()
}

/// Whether `user` holds ownership authority over the Secret, given that
/// they are an active member who reads the map.
fn owns(clan: &ClanRecord, record: &ClanSecretRecord, user: Uuid) -> bool {
    if !clan.has_member(user) {
        return false;
    }
    if record.ownership == "members" {
        record.owners.iter().any(|(owner, _)| *owner == user)
    } else {
        clan.has_owner(user)
    }
}

/// `user`'s actions on a Clan Secret of `area` (the server's
/// `SecretAuthority.actions`): every action with ownership authority;
/// otherwise the Secret's grants and, on a Clan-owned one, the clan's
/// collection grants; `read` alone on a frozen one; nothing without `read`
/// or to anyone who is not an active member reading the map as filed.
pub fn actions(
    st: &MockState,
    user: Uuid,
    area: &AreaRecord,
    record: &ClanSecretRecord,
) -> Vec<&'static str> {
    let Some(clan) = active_clan(st, record.clan_id) else {
        return Vec::new();
    };
    let map = map_actions(st, clan, user, area.id);
    if !clan.has_member(user) || !map.contains("area.read") {
        return Vec::new();
    }
    if owns(clan, record, user) {
        return SECRET_ACTIONS.to_vec();
    }
    let groups = clan.member_groups(user);
    let mut held = BTreeSet::new();
    for grant in &record.grants {
        let reaches = match grant.recipient {
            ClanRecipient::User(id) => id == user,
            ClanRecipient::Group(id) => groups.contains(&id),
        };
        if reaches {
            held.extend(flagged(&grant.actions));
        }
    }
    if record.ownership == "clan" {
        held.extend(collection(&map));
    }
    if !held.contains("read") {
        return Vec::new();
    }
    if record.frozen {
        return vec!["read"];
    }
    ordered(&held)
}

fn collection(map: &BTreeSet<String>) -> BTreeSet<&'static str> {
    COLLECTION
        .iter()
        .filter(|(clan_action, _)| map.contains(*clan_action))
        .map(|(_, action)| *action)
        .collect()
}

/// A Clan Secret by ID: its map, its index there, and its clan record.
fn find(st: &MockState, id: Uuid) -> Option<(Uuid, usize)> {
    st.areas.values().find_map(|area| {
        let index = area
            .secrets
            .iter()
            .position(|secret| secret.id == id && secret.clan.is_some())?;
        Some((area.id, index))
    })
}

fn secret_at(st: &MockState, (area, index): (Uuid, usize)) -> (&AreaRecord, &SecretRecord) {
    let area = &st.areas[&area];
    (area, &area.secrets[index])
}

fn clan_of(secret: &SecretRecord) -> &ClanSecretRecord {
    secret.clan.as_ref().expect("found as a Clan Secret")
}

fn clan_of_mut(st: &mut MockState, (area, index): (Uuid, usize)) -> &mut ClanSecretRecord {
    st.areas.get_mut(&area).expect("found").secrets[index]
        .clan
        .as_mut()
        .expect("found as a Clan Secret")
}

/// The caller's actions on the Clan Secret `raw`, or the uniform 404 when
/// it is not one they read.
fn readable(
    st: &MockState,
    viewer: Uuid,
    raw: &str,
) -> Result<((Uuid, usize), Vec<&'static str>), Response> {
    let id = Uuid::parse_str(raw).map_err(|_| bad_request("Invalid Secret ID"))?;
    let at = find(st, id).ok_or_else(not_found)?;
    let (area, secret) = secret_at(st, at);
    let mine = actions(st, viewer, area, clan_of(secret));
    if mine.contains(&"read") {
        Ok((at, mine))
    } else {
        Err(not_found())
    }
}

fn verified(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (viewer, _) = authenticate(st, headers)?;
    if !st.email_verified(viewer) {
        return Err(email_not_verified());
    }
    Ok(viewer)
}

fn nickname(st: &MockState, user: Uuid) -> Option<String> {
    st.user(user).and_then(|record| record.nickname.clone())
}

fn respond(result: Result<Response, Response>) -> Response {
    result.unwrap_or_else(|response| response)
}

// ---------------------------------------------------------------------------
// Creating
// ---------------------------------------------------------------------------

/// `POST /areas/{a}/secrets` with `ownership` of `members` or `clan`. The
/// name and color are already validated.
pub fn create(
    st: &mut MockState,
    viewer: Uuid,
    area_id: Uuid,
    mut secret: SecretRecord,
    fields: &Value,
) -> Response {
    let ownership: &'static str = match fields.get("ownership").and_then(Value::as_str) {
        Some("members") => "members",
        Some("clan") => "clan",
        _ => return bad_request("`ownership` is `owner`, `members` or `clan`"),
    };
    let named = match fields.get("clan_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => return bad_request("Invalid clan ID"),
        },
        Some(_) => return bad_request("Invalid clan ID"),
    };
    let Some(area) = st.areas.get(&area_id) else {
        return not_found();
    };
    // A clan's own map takes only its clan; a user's map needs the clan
    // that files it by link.
    let clan_id = match (area.clan_id, named) {
        (Some(own), None) => own,
        (Some(own), Some(named)) if named == own => own,
        (None, Some(named)) => named,
        _ => return not_found(),
    };
    let Some(clan) = active_clan(st, clan_id) else {
        return not_found();
    };
    let map = map_actions(st, clan, viewer, area_id);
    let needed = if ownership == "members" {
        "secret.create_member_owned"
    } else {
        "secret.create_clan_owned"
    };
    if !clan.has_member(viewer) || !map.contains("area.read") || !map.contains(needed) {
        return not_found();
    }
    if let Some(refusal) = super::clan_maps::refused_while_disposing(st, area_id) {
        return refusal;
    }
    let now = Utc::now();
    let mut record = ClanSecretRecord {
        clan_id,
        ownership,
        created_by: viewer,
        owners: Vec::new(),
        grants: Vec::new(),
        access_rev: 0,
        frozen: false,
        offers: Vec::new(),
    };
    if ownership == "members" {
        record.owners.push((viewer, now));
    } else {
        // The creator starts as a Contributor; who else reads it comes from
        // the clan's `secret.*` grants on its map and its own grants.
        record.grants.push(ClanSecretGrantRecord {
            id: Uuid::new_v4(),
            recipient: ClanRecipient::User(viewer),
            actions: BTreeSet::from(["add", "edit"]),
            grantor_id: viewer,
            created_at: now,
            updated_at: now,
        });
    }
    let area = st.areas.get(&area_id).expect("found above");
    let mine = actions(st, viewer, area, &record);
    secret.clan = Some(record);
    let out = super::secrets::summary(&secret, &mine);
    st.areas
        .get_mut(&area_id)
        .expect("found above")
        .secrets
        .push(secret);
    created(out)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/secrets/:secret_id/access", get(access))
        .route("/secrets/:secret_id/owners", get(list_owners))
        .route("/secrets/:secret_id/owners/:user_id", delete(remove_owner))
        .route("/secrets/:secret_id/transfer", get(list_offers).post(offer))
        .route(
            "/secrets/:secret_id/transfer/:offer_id/accept",
            post(accept),
        )
        .route(
            "/secrets/:secret_id/transfer/:offer_id/decline",
            post(decline),
        )
        .route("/secrets/:secret_id/transfer/:offer_id", delete(withdraw))
        .route("/me/secret-offers", get(received))
}

/// One member's entry in the access list, or `None` when they do not read
/// the Secret.
fn entry(
    st: &MockState,
    clan: &ClanRecord,
    area: &AreaRecord,
    record: &ClanSecretRecord,
    user: Uuid,
) -> Option<(Vec<&'static str>, Vec<Value>)> {
    let held = actions(st, user, area, record);
    if !held.contains(&"read") {
        return None;
    }
    let given = |carried: &BTreeSet<&'static str>| -> Vec<&'static str> {
        ordered(carried)
            .into_iter()
            .filter(|action| held.contains(action))
            .collect()
    };
    let mut reasons = Vec::new();
    if owns(clan, record, user) {
        let kind = if record.ownership == "members" {
            "owner"
        } else {
            "clan_owner"
        };
        reasons.push(json!({ "kind": kind }));
    }
    let groups = clan.member_groups(user);
    for grant in &record.grants {
        match grant.recipient {
            ClanRecipient::User(id) if id == user => reasons.push(json!({
                "kind": "grant",
                "grant_id": grant.id,
                "actions": given(&flagged(&grant.actions)),
            })),
            ClanRecipient::Group(group) if groups.contains(&group) => reasons.push(json!({
                "kind": "group",
                "grant_id": grant.id,
                "group_id": group,
                "actions": given(&flagged(&grant.actions)),
            })),
            _ => {}
        }
    }
    if record.ownership == "clan" && !clan.has_owner(user) {
        let through = given(&collection(&map_actions(st, clan, user, area.id)));
        if !through.is_empty() {
            reasons.push(json!({ "kind": "clan_grants", "actions": through }));
        }
    }
    Some((held, reasons))
}

/// `GET /secrets/{s}/access`.
pub async fn access(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let (at, mine) = readable(&st, viewer, &raw)?;
        let (area, secret) = secret_at(&st, at);
        let record = clan_of(secret);
        let clan = active_clan(&st, record.clan_id).ok_or_else(not_found)?;
        let people = if mine.contains(&"manage_access") {
            clan.directory()
        } else {
            vec![viewer]
        };
        let mut members = Vec::new();
        for user in people {
            let Some((held, reasons)) = entry(&st, clan, area, record, user) else {
                continue;
            };
            let shown: Vec<Value> = if user == viewer {
                reasons
            } else {
                reasons
                    .into_iter()
                    .filter(|reason| match reason["kind"].as_str() {
                        Some("group") => reason["group_id"]
                            .as_str()
                            .and_then(|raw| Uuid::parse_str(raw).ok())
                            .is_some_and(|group| {
                                clan.group_actions_of(viewer, group)
                                    .contains("group.inspect_assignments")
                            }),
                        Some("clan_grants") => {
                            map_actions(&st, clan, viewer, area.id).contains("grant.inspect")
                        }
                        _ => true,
                    })
                    .collect()
            };
            if shown.is_empty() {
                continue;
            }
            let owner = shown
                .iter()
                .any(|reason| matches!(reason["kind"].as_str(), Some("owner" | "clan_owner")));
            let listed: Vec<&'static str> = if owner || user == viewer {
                held
            } else {
                let union: BTreeSet<&'static str> = shown
                    .iter()
                    .flat_map(|reason| {
                        reason["actions"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .filter_map(|action| {
                                SECRET_ACTIONS
                                    .iter()
                                    .copied()
                                    .find(|known| *known == action)
                            })
                    })
                    .collect();
                ordered(&union)
            };
            let mut row = json!({ "user_id": user, "actions": listed, "reasons": shown });
            if let Some(nickname) = nickname(&st, user) {
                row["nickname"] = Value::String(nickname);
            }
            members.push(row);
        }
        Ok(ok(json!({
            "secret_id": secret.id,
            "area_id": area.id,
            "clan_id": record.clan_id,
            "ownership": record.ownership,
            "members": members,
        })))
    })())
}

/// `GET /secrets/{s}/owners`: needs `manage_access`.
pub async fn list_owners(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let (at, mine) = readable(&st, viewer, &raw)?;
        if !mine.contains(&"manage_access") {
            return Err(not_found());
        }
        let (_, secret) = secret_at(&st, at);
        let record = clan_of(secret);
        let clan = active_clan(&st, record.clan_id).ok_or_else(not_found)?;
        // Oldest first, ties by user ID, as the server's query orders them:
        // a joint offer's recipients become owners at the same moment.
        let mut owners = record.owners.clone();
        owners.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        Ok(ok(json!(
            owners
                .iter()
                .map(|(user, added_at)| json!({
                    "user_id": user,
                    "nickname": nickname(&st, *user),
                    "active": clan.has_member(*user),
                    "added_at": added_at,
                }))
                .collect::<Vec<_>>()
        )))
    })())
}

/// `DELETE /secrets/{s}/owners/{u}`: needs `manage_ownership`.
pub async fn remove_owner(
    State(state): State<Shared>,
    Path((raw, raw_user)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let user = Uuid::parse_str(&raw_user).map_err(|_| not_found())?;
        let (at, mine) = readable(&st, viewer, &raw)?;
        let (_, secret) = secret_at(&st, at);
        let record = clan_of(secret);
        if !mine.contains(&"manage_ownership") || record.ownership != "members" {
            return Err(not_found());
        }
        if !record.owners.iter().any(|(owner, _)| *owner == user) {
            return Err(not_found());
        }
        if record.owners.len() == 1 {
            return Err(conflict("last_owner"));
        }
        let record = clan_of_mut(&mut st, at);
        record.owners.retain(|(owner, _)| *owner != user);
        record.access_rev += 1;
        Ok(ok(Value::Null))
    })())
}

fn offer_view(
    st: &MockState,
    area: &AreaRecord,
    secret: &SecretRecord,
    offer: &OfferRecord,
) -> Value {
    let record = clan_of(secret);
    let mut out = json!({
        "id": offer.id,
        "secret_id": secret.id,
        "secret_name": secret.name,
        "secret_color": secret.color,
        "area_id": area.id,
        "clan_id": record.clan_id,
        "recipients": offer.recipients.iter().map(|(user, accepted)| {
            let mut row = json!({ "user_id": user, "accepted": accepted.is_some() });
            if let Some(nickname) = nickname(st, *user) {
                row["nickname"] = Value::String(nickname);
            }
            row
        }).collect::<Vec<_>>(),
        "ownership": offer.ownership,
        "replace": offer.replace,
        "initiator_id": offer.initiator_id,
        "created_at": offer.created_at,
    });
    if let Some(nickname) = nickname(st, offer.initiator_id) {
        out["initiator_nickname"] = Value::String(nickname);
    }
    out
}

/// Whether an offer from `initiator` may still stand.
fn initiator_may(
    st: &MockState,
    area: &AreaRecord,
    record: &ClanSecretRecord,
    ownership: &str,
    initiator: Uuid,
) -> bool {
    if !actions(st, initiator, area, record).contains(&"manage_ownership") {
        return false;
    }
    // A Member-owned map takes no Clan-owned Secret.
    if ownership == "clan" && area.member_owned.is_some() {
        return false;
    }
    record.ownership == "members"
        || ownership == "clan"
        || active_clan(st, record.clan_id).is_some_and(|clan| {
            map_actions(st, clan, initiator, area.id).contains("secret.create_member_owned")
        })
}

/// Whether `recipient` may receive an offer: an active member who reads the
/// Secret and is not yet an owner; a clan owner, to make it Clan-owned.
fn recipient_may(
    st: &MockState,
    area: &AreaRecord,
    record: &ClanSecretRecord,
    ownership: &str,
    recipient: Uuid,
) -> bool {
    let Some(clan) = active_clan(st, record.clan_id) else {
        return false;
    };
    if !clan.has_member(recipient) || !actions(st, recipient, area, record).contains(&"read") {
        return false;
    }
    if ownership == "clan" {
        return clan.has_owner(recipient);
    }
    !record.owners.iter().any(|(owner, _)| *owner == recipient)
}

/// `POST /secrets/{s}/transfer`.
pub async fn offer(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let (at, mine) = readable(&st, viewer, &raw)?;
        if !mine.contains(&"manage_ownership") {
            return Err(not_found());
        }
        let fields: Map<String, Value> =
            serde_json::from_str(&body).map_err(|_| bad_request("expected a JSON object"))?;
        let ownership: &'static str = match fields.get("ownership").and_then(Value::as_str) {
            Some("members") => "members",
            Some("clan") => "clan",
            _ => return Err(bad_request("`ownership` is `members` or `clan`")),
        };
        if fields.contains_key("user_ids") && fields.contains_key("user_id") {
            return Err(bad_request("give one of `user_ids` and `user_id`"));
        }
        let raw_ids: Vec<Value> = match (fields.get("user_ids"), fields.get("user_id")) {
            (Some(Value::Array(ids)), None) => ids.clone(),
            (None, Some(id)) => vec![id.clone()],
            _ => return Err(bad_request("missing field `user_ids`")),
        };
        let mut recipients = Vec::new();
        for raw in raw_ids {
            let id = raw
                .as_str()
                .and_then(|raw| Uuid::parse_str(raw).ok())
                .ok_or_else(|| bad_request("expected UUIDs in `user_ids`"))?;
            recipients.push(id);
        }
        if recipients.is_empty() || recipients.len() > MAX_RECIPIENTS {
            return Err(bad_request("`user_ids` names 1 to 16 users"));
        }
        if recipients.iter().collect::<BTreeSet<_>>().len() != recipients.len() {
            return Err(bad_request("`user_ids` names each user once"));
        }
        if ownership == "clan" && recipients.len() != 1 {
            return Err(bad_request(
                "an offer to make a Secret Clan-owned names one user",
            ));
        }
        let replace = fields
            .get("replace")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (area, secret) = secret_at(&st, at);
        let record = clan_of(secret);
        if record.ownership == "clan" && ownership == "clan" {
            return Err(bad_request("The Secret is already Clan-owned"));
        }
        if replace && (record.ownership != "members" || ownership != "members") {
            return Err(bad_request(
                "replace applies only to offers between members",
            ));
        }
        if !initiator_may(&st, area, record, ownership, viewer)
            || !recipients
                .iter()
                .all(|user| recipient_may(&st, area, record, ownership, *user))
        {
            return Err(not_found());
        }
        let offer = OfferRecord {
            id: Uuid::new_v4(),
            recipients: recipients.iter().map(|user| (*user, None)).collect(),
            ownership,
            replace,
            initiator_id: viewer,
            access_rev: record.access_rev,
            created_at: Utc::now(),
        };
        let record = clan_of_mut(&mut st, at);
        record.offers.retain(|other| {
            !other
                .recipients
                .iter()
                .any(|(user, _)| recipients.contains(user))
        });
        record.offers.push(offer.clone());
        let (area, secret) = secret_at(&st, at);
        Ok(created(offer_view(&st, area, secret, &offer)))
    })())
}

/// Offers still standing: made at the Secret's current access revision.
fn standing(record: &ClanSecretRecord) -> impl Iterator<Item = &OfferRecord> {
    record
        .offers
        .iter()
        .filter(|offer| offer.access_rev == record.access_rev)
}

/// `GET /secrets/{s}/transfer`.
pub async fn list_offers(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let (at, mine) = readable(&st, viewer, &raw)?;
        let all = mine.contains(&"manage_ownership");
        let (area, secret) = secret_at(&st, at);
        Ok(ok(json!(
            standing(clan_of(secret))
                .filter(|offer| all || offer.recipients.iter().any(|(user, _)| *user == viewer))
                .map(|offer| offer_view(&st, area, secret, offer))
                .collect::<Vec<_>>()
        )))
    })())
}

/// `GET /me/secret-offers`.
pub async fn received(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let mut offers: Vec<(DateTime<Utc>, Uuid, Value)> = Vec::new();
        for area in st.areas.values() {
            for secret in &area.secrets {
                let Some(record) = &secret.clan else { continue };
                if !actions(&st, viewer, area, record).contains(&"read") {
                    continue;
                }
                for offer in standing(record) {
                    if offer.recipients.iter().any(|(user, _)| *user == viewer) {
                        offers.push((
                            offer.created_at,
                            offer.id,
                            offer_view(&st, area, secret, offer),
                        ));
                    }
                }
            }
        }
        offers.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        Ok(ok(json!(
            offers
                .into_iter()
                .map(|(_, _, view)| view)
                .collect::<Vec<_>>()
        )))
    })())
}

/// The offer `raw_offer` on the Clan Secret `raw`, when it names the caller.
fn named_offer(
    st: &MockState,
    viewer: Uuid,
    raw: &str,
    raw_offer: &str,
) -> Result<((Uuid, usize), Uuid), Response> {
    let id = Uuid::parse_str(raw).map_err(|_| not_found())?;
    let offer_id = Uuid::parse_str(raw_offer).map_err(|_| not_found())?;
    let at = find(st, id).ok_or_else(not_found)?;
    let (_, secret) = secret_at(st, at);
    let named = clan_of(secret).offers.iter().any(|offer| {
        offer.id == offer_id && offer.recipients.iter().any(|(user, _)| *user == viewer)
    });
    if named {
        Ok((at, offer_id))
    } else {
        Err(not_found())
    }
}

/// `POST /secrets/{s}/transfer/{o}/accept`.
pub async fn accept(
    State(state): State<Shared>,
    Path((raw, raw_offer)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let (at, offer_id) = named_offer(&st, viewer, &raw, &raw_offer)?;
        let (area, secret) = secret_at(&st, at);
        let record = clan_of(secret);
        let offer = record
            .offers
            .iter()
            .find(|offer| offer.id == offer_id)
            .expect("named above")
            .clone();
        let others: Vec<(Uuid, Option<DateTime<Utc>>)> = offer
            .recipients
            .iter()
            .filter(|(user, _)| *user != viewer)
            .copied()
            .collect();
        let complete = others.iter().all(|(_, accepted)| accepted.is_some());
        let valid = offer.access_rev == record.access_rev
            && recipient_may(&st, area, record, offer.ownership, viewer)
            && initiator_may(&st, area, record, offer.ownership, offer.initiator_id)
            && (!complete
                || others
                    .iter()
                    .all(|(user, _)| recipient_may(&st, area, record, offer.ownership, *user)));
        if !valid {
            clan_of_mut(&mut st, at)
                .offers
                .retain(|other| other.id != offer_id);
            return Err(not_found());
        }
        let now = Utc::now();
        let record = clan_of_mut(&mut st, at);
        if complete {
            if record.ownership != offer.ownership {
                record.ownership = offer.ownership;
                record.access_rev += 1;
            }
            if offer.ownership == "clan" || offer.replace {
                record.owners.clear();
                record.access_rev += 1;
            }
            if offer.ownership == "members" {
                for (user, _) in &offer.recipients {
                    record.owners.push((*user, now));
                }
                record.access_rev += 1;
            }
            record.offers.clear();
        } else if let Some(stored) = record.offers.iter_mut().find(|other| other.id == offer_id) {
            for (user, accepted) in &mut stored.recipients {
                if *user == viewer && accepted.is_none() {
                    *accepted = Some(now);
                }
            }
        }
        let (area, secret) = secret_at(&st, at);
        let mine = actions(&st, viewer, area, clan_of(secret));
        Ok(ok(super::secrets::summary(secret, &mine)))
    })())
}

/// `POST /secrets/{s}/transfer/{o}/decline`: ends the offer for everyone.
pub async fn decline(
    State(state): State<Shared>,
    Path((raw, raw_offer)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let (at, offer_id) = named_offer(&st, viewer, &raw, &raw_offer)?;
        // The recipient declines only a Secret they still read, as the
        // offer lists only to them then.
        let (area, secret) = secret_at(&st, at);
        if !actions(&st, viewer, area, clan_of(secret)).contains(&"read") {
            return Err(not_found());
        }
        clan_of_mut(&mut st, at)
            .offers
            .retain(|offer| offer.id != offer_id);
        Ok(ok(Value::Null))
    })())
}

/// `DELETE /secrets/{s}/transfer/{o}`: needs `manage_ownership`.
pub async fn withdraw(
    State(state): State<Shared>,
    Path((raw, raw_offer)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| {
        let viewer = verified(&st, &headers)?;
        let offer_id = Uuid::parse_str(&raw_offer).map_err(|_| not_found())?;
        let (at, mine) = readable(&st, viewer, &raw)?;
        let (_, secret) = secret_at(&st, at);
        if !mine.contains(&"manage_ownership")
            || !clan_of(secret)
                .offers
                .iter()
                .any(|offer| offer.id == offer_id)
        {
            return Err(not_found());
        }
        clan_of_mut(&mut st, at)
            .offers
            .retain(|offer| offer.id != offer_id);
        Ok(ok(Value::Null))
    })())
}

// ---------------------------------------------------------------------------
// Departure, groups, account deletion
// ---------------------------------------------------------------------------

fn clan_secrets_mut(st: &mut MockState, clan: Uuid) -> impl Iterator<Item = &mut ClanSecretRecord> {
    st.areas
        .values_mut()
        .flat_map(|area| area.secrets.iter_mut())
        .filter_map(|secret| secret.clan.as_mut())
        .filter(move |record| record.clan_id == clan)
}

/// A member left or was removed: their grants on the clan's Secrets go, and
/// so do the offers they made or received. Their recorded ownership stays,
/// dormant.
pub fn forget_member(st: &mut MockState, clan: Uuid, user: Uuid) {
    for record in clan_secrets_mut(st, clan) {
        let before = record.grants.len();
        record
            .grants
            .retain(|grant| grant.recipient != ClanRecipient::User(user));
        if record.grants.len() != before {
            record.access_rev += 1;
        }
        drop_offers_of(record, user);
    }
    sweep_frozen(st, clan, Some(user));
}

/// A group was deleted: its Secret grants go with it.
pub fn forget_group(st: &mut MockState, clan: Uuid, group: Uuid) {
    for record in clan_secrets_mut(st, clan) {
        let before = record.grants.len();
        record
            .grants
            .retain(|grant| grant.recipient != ClanRecipient::Group(group));
        if record.grants.len() != before {
            record.access_rev += 1;
        }
    }
}

fn drop_offers_of(record: &mut ClanSecretRecord, user: Uuid) {
    record.offers.retain(|offer| {
        offer.initiator_id != user && !offer.recipients.iter().any(|(named, _)| *named == user)
    });
}

/// The account of `user` is deleted (§8.6): a Member-owned Secret whose last
/// recorded owner they are freezes; their ownership, grants and offers go;
/// frozen Secrets no grant reaches are deleted.
pub fn forget_account(st: &mut MockState, user: Uuid) {
    let clans: Vec<Uuid> = st.clans.clans.keys().copied().collect();
    for clan in clans {
        for record in clan_secrets_mut(st, clan) {
            if record.owners.len() == 1 && record.owners[0].0 == user {
                record.frozen = true;
            }
            let before = (record.owners.len(), record.grants.len());
            record.owners.retain(|(owner, _)| *owner != user);
            record
                .grants
                .retain(|grant| grant.recipient != ClanRecipient::User(user));
            if (record.owners.len(), record.grants.len()) != before {
                record.access_rev += 1;
            }
            drop_offers_of(record, user);
        }
        sweep_frozen(st, clan, Some(user));
    }
}

/// Deletes the clan's frozen Secrets that no grant reaches: none to a group,
/// and none to an active member other than `excluded`.
fn sweep_frozen(st: &mut MockState, clan: Uuid, excluded: Option<Uuid>) {
    let active: BTreeSet<Uuid> = st
        .clans
        .clans
        .get(&clan)
        .map(|record| record.directory().into_iter().collect())
        .unwrap_or_default();
    for area in st.areas.values_mut() {
        area.secrets.retain(|secret| {
            let Some(record) = &secret.clan else {
                return true;
            };
            if record.clan_id != clan || !record.frozen {
                return true;
            }
            record.grants.iter().any(|grant| match grant.recipient {
                ClanRecipient::Group(_) => true,
                ClanRecipient::User(id) => active.contains(&id) && Some(id) != excluded,
            })
        });
    }
}

// ---------------------------------------------------------------------------
// Test setup
// ---------------------------------------------------------------------------

impl super::mock_server::MockHandle {
    /// Deletes `user`'s account as far as Clan Secrets go (§8.6).
    pub fn forget_account_in_clan_secrets(&self, user: Uuid) {
        forget_account(&mut self.state.lock(), user);
    }

    /// Whether the Clan Secret `id` still exists, frozen or not.
    pub fn clan_secret_state(&self, id: Uuid) -> Option<(String, bool, usize)> {
        let st = self.state.lock();
        st.areas.values().find_map(|area| {
            area.secrets
                .iter()
                .find(|secret| secret.id == id)
                .and_then(|secret| {
                    secret.clan.as_ref().map(|record| {
                        (
                            record.ownership.to_string(),
                            record.frozen,
                            record.owners.len(),
                        )
                    })
                })
        })
    }
}

/// The clan of a Clan Secret on a map filed in that clan by link: such a
/// Secret holds only its own content (clans.md §8.7). `None` for owner
/// Secrets and for a clan's Secrets on its own maps.
pub fn linked_clan(area: &AreaRecord, secret: &SecretRecord) -> Option<Uuid> {
    secret
        .clan
        .as_ref()
        .map(|record| record.clan_id)
        .filter(|clan| area.clan_id != Some(*clan))
}
