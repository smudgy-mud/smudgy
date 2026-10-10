//! A Clan Secret's grants: the `/secrets/{s}/grants` routes for a Secret
//! that belongs to a clan. Fidelity reference: the smudgy-cloudflare service
//! (`src/library/secrets/grants.ts`, `readRecipient` in
//! `src/sharing/routes.ts`, docs/clans.md §8.4).
//!
//! A grant names a recipient, an active member or one of the clan's groups
//! (built-ins included), and gives `read` plus any of `add`, `edit`,
//! `remove`, `manage_access` and `copy` (which no preset holds, so a grant
//! carries it only when it names it); there is one grant per recipient, and a
//! second POST for the same recipient replaces its actions under the rules
//! for changing it (keeping its grantor). Ownership authority writes any
//! grant; a holder of `manage_access` grants only actions it holds, never
//! `manage_access`, and changes or revokes only grants that do not carry
//! it. Holders of `manage_access` list every grant; another reader lists
//! the grants to them and to their groups. Every refusal is the uniform
//! 404, and every write moves the Secret's access revision, which ends
//! pending ownership offers.

use std::collections::BTreeSet;

use axum::response::Response;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use super::clan_secrets::{ClanSecretGrantRecord, ClanSecretRecord, actions};
use super::clans::ClanRecipient;
use super::http::{bad_request, created, not_found, ok};
use super::state::MockState;

const MANAGE_ACCESS: &str = "manage_access";
const MANAGE_OWNERSHIP: &str = "manage_ownership";

/// Grant actions in the order responses list them.
const ORDER: [&str; 6] = ["read", "add", "edit", "remove", MANAGE_ACCESS, "copy"];

/// A POST body's recipient: `grantee_id` names a member, or `recipient`
/// names exactly one of `user_id` and `group_id`. Both at once is a 400.
pub fn read_recipient(body: &Value) -> Result<ClanRecipient, Response> {
    let uuid = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .and_then(|raw| Uuid::parse_str(raw).ok())
    };
    let Some(fields) = body.get("recipient") else {
        return uuid(body.get("grantee_id"))
            .map(ClanRecipient::User)
            .ok_or_else(|| bad_request("`grantee_id` must be a UUID"));
    };
    if body.get("grantee_id").is_some() {
        return Err(bad_request("give one of `recipient` and `grantee_id`"));
    }
    let Some(fields) = fields.as_object() else {
        return Err(bad_request("`recipient` must be an object"));
    };
    match (uuid(fields.get("user_id")), uuid(fields.get("group_id"))) {
        (Some(user), None) if fields.get("group_id").is_none() => Ok(ClanRecipient::User(user)),
        (None, Some(group)) if fields.get("user_id").is_none() => Ok(ClanRecipient::Group(group)),
        _ => Err(bad_request(
            "`recipient` names exactly one of `user_id` and `group_id`",
        )),
    }
}

fn with_read(extra: &BTreeSet<&'static str>) -> Vec<&'static str> {
    ORDER
        .into_iter()
        .filter(|action| *action == "read" || extra.contains(action))
        .collect()
}

fn recipient_json(recipient: ClanRecipient) -> Value {
    match recipient {
        ClanRecipient::User(id) => json!({ "user_id": id }),
        ClanRecipient::Group(id) => json!({ "group_id": id }),
    }
}

fn view(st: &MockState, at: (Uuid, usize), grant: &ClanSecretGrantRecord) -> Value {
    let area = &st.areas[&at.0];
    let secret = &area.secrets[at.1];
    let record = secret.clan.as_ref().expect("a Clan Secret");
    let mut row = json!({
        "id": grant.id,
        "secret_id": secret.id,
        "area_id": area.id,
        "clan_id": record.clan_id,
        "recipient": recipient_json(grant.recipient),
        "actions": with_read(&grant.actions),
        "grantor_id": grant.grantor_id,
        "created_at": grant.created_at,
        "updated_at": grant.updated_at,
    });
    let nickname = |user: Uuid| st.user(user).and_then(|record| record.nickname.clone());
    if let ClanRecipient::User(user) = grant.recipient
        && let Some(name) = nickname(user)
    {
        row["nickname"] = json!(name);
    }
    if let Some(name) = nickname(grant.grantor_id) {
        row["grantor_nickname"] = json!(name);
    }
    row
}

fn record_at(st: &MockState, at: (Uuid, usize)) -> &ClanSecretRecord {
    st.areas[&at.0].secrets[at.1]
        .clan
        .as_ref()
        .expect("a Clan Secret")
}

fn record_mut(st: &mut MockState, at: (Uuid, usize)) -> &mut ClanSecretRecord {
    st.areas.get_mut(&at.0).expect("found").secrets[at.1]
        .clan
        .as_mut()
        .expect("a Clan Secret")
}

fn mine(st: &MockState, viewer: Uuid, at: (Uuid, usize)) -> Vec<&'static str> {
    let area = &st.areas[&at.0];
    actions(st, viewer, area, record_at(st, at))
}

/// Whether a caller holding `mine` may take a grant from `before` to
/// `after`: ownership authority any; a holder of `manage_access` only
/// within its own actions, and never to or from `manage_access`.
fn may_change(
    mine: &[&str],
    before: &BTreeSet<&'static str>,
    after: &BTreeSet<&'static str>,
) -> bool {
    if !mine.contains(&MANAGE_ACCESS) {
        return false;
    }
    if mine.contains(&MANAGE_OWNERSHIP) {
        return true;
    }
    if before.contains(MANAGE_ACCESS) || after.contains(MANAGE_ACCESS) {
        return false;
    }
    after
        .iter()
        .all(|action| before.contains(action) || mine.contains(action))
}

/// An active member of the clan, or one of its groups.
fn recipient_exists(st: &MockState, clan: Uuid, recipient: ClanRecipient) -> bool {
    let Some(clan) = st.clans.clans.get(&clan).filter(|clan| !clan.dissolved) else {
        return false;
    };
    match recipient {
        ClanRecipient::User(user) => clan.has_member(user),
        ClanRecipient::Group(group) => clan.groups.iter().any(|known| known.id == group),
    }
}

/// `GET /secrets/{s}/grants` on a Clan Secret.
pub fn list(st: &MockState, viewer: Uuid, at: (Uuid, usize)) -> Response {
    let mine = mine(st, viewer, at);
    if !mine.contains(&"read") {
        return not_found();
    }
    let record = record_at(st, at);
    let manager = mine.contains(&MANAGE_ACCESS);
    let groups = st
        .clans
        .clans
        .get(&record.clan_id)
        .map(|clan| clan.member_groups(viewer))
        .unwrap_or_default();
    let rows: Vec<Value> = record
        .grants
        .iter()
        .filter(|grant| {
            manager
                || match grant.recipient {
                    ClanRecipient::User(user) => user == viewer,
                    ClanRecipient::Group(group) => groups.contains(&group),
                }
        })
        .map(|grant| view(st, at, grant))
        .collect();
    ok(json!(rows))
}

/// `POST /secrets/{s}/grants` on a Clan Secret. 201 with the grant, a
/// replaced one included.
pub fn create(
    st: &mut MockState,
    viewer: Uuid,
    at: (Uuid, usize),
    recipient: ClanRecipient,
    actions: BTreeSet<&'static str>,
) -> Response {
    let mine = mine(st, viewer, at);
    let record = record_at(st, at);
    if !mine.contains(&MANAGE_ACCESS) || !recipient_exists(st, record.clan_id, recipient) {
        return not_found();
    }
    let existing = record
        .grants
        .iter()
        .position(|grant| grant.recipient == recipient);
    let before = existing.map_or_else(BTreeSet::new, |index| record.grants[index].actions.clone());
    if !may_change(&mine, &before, &actions) {
        return not_found();
    }
    let now = Utc::now();
    let record = record_mut(st, at);
    let index = if let Some(index) = existing {
        let grant = &mut record.grants[index];
        grant.actions = actions;
        grant.updated_at = now;
        index
    } else {
        record.grants.push(ClanSecretGrantRecord {
            id: Uuid::new_v4(),
            recipient,
            actions,
            grantor_id: viewer,
            created_at: now,
            updated_at: now,
        });
        record.grants.len() - 1
    };
    record.access_rev += 1;
    let grant = record.grants[index].clone();
    created(view(st, at, &grant))
}

/// `PATCH /secrets/{s}/grants/{g}` on a Clan Secret.
pub fn update(
    st: &mut MockState,
    viewer: Uuid,
    at: (Uuid, usize),
    grant_id: Uuid,
    actions: BTreeSet<&'static str>,
) -> Response {
    let mine = mine(st, viewer, at);
    let record = record_at(st, at);
    let Some(index) = record.grants.iter().position(|grant| grant.id == grant_id) else {
        return not_found();
    };
    if !may_change(&mine, &record.grants[index].actions, &actions) {
        return not_found();
    }
    let record = record_mut(st, at);
    let grant = &mut record.grants[index];
    grant.actions = actions;
    grant.updated_at = Utc::now();
    let grant = grant.clone();
    record.access_rev += 1;
    ok(view(st, at, &grant))
}

/// `DELETE /secrets/{s}/grants/{g}` on a Clan Secret.
pub fn revoke(st: &mut MockState, viewer: Uuid, at: (Uuid, usize), grant_id: Uuid) -> Response {
    let mine = mine(st, viewer, at);
    let record = record_at(st, at);
    let Some(index) = record.grants.iter().position(|grant| grant.id == grant_id) else {
        return not_found();
    };
    if !mine.contains(&MANAGE_ACCESS)
        || (record.grants[index].actions.contains(MANAGE_ACCESS)
            && !mine.contains(&MANAGE_OWNERSHIP))
    {
        return not_found();
    }
    let record = record_mut(st, at);
    record.grants.remove(index);
    record.access_rev += 1;
    ok(Value::Null)
}
