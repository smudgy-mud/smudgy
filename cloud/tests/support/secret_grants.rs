//! Per-Secret sharing: GET/POST /secrets/{id}/grants, PATCH/DELETE
//! /secrets/{id}/grants/{grant}, and who reads which Secret. Fidelity
//! reference: the smudgy-cloudflare service
//! (`src/library/sharing/secret-grants.ts`, `src/sharing/routes.ts`, and
//! `sourceActions` in `src/library/authz/evaluator.ts`).
//!
//! A grant gives its grantee `read` and any of `add`, `edit`, `remove`,
//! `manage_access` and `copy`, which takes the Secret along in a copy of
//! the map and which no preset holds. The map's owner holds every action,
//! `copy` included. A holder of `manage_access` grants only actions it
//! holds, never `manage_access`; only the owner grants that, on grants the
//! owner issued, and only the owner changes or revokes a grant carrying
//! it. Grants are keyed by (Secret, grantor, grantee); a grantee's actions
//! are the union of theirs. A grant confers nothing while its grantee
//! cannot read the map, and stays in place until then. Unfriending and
//! blocking delete grants as they delete map shares; deleting a Secret, or
//! its map, deletes its grants.
//!
//! Every route needs a verified email (403). A body of the wrong shape or
//! an unknown action is a 400; every other refusal is the uniform 404.
//! A Clan Secret's grants follow the clan's rules instead
//! (`clan_secret_grants.rs`); a group recipient on an owner Secret is a 400
//! for someone who manages it and the uniform 404 for anyone else.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use parking_lot::Mutex;
use serde_json::{Value, json};
use uuid::Uuid;

use super::clans::ClanRecipient;
use super::http::{authenticate, bad_request, created, gate_verified, not_found, ok, parse_body};
use super::state::{AreaRecord, MockState, SecretGrantRecord, SecretRecord};

pub type Shared = Arc<Mutex<MockState>>;

pub const READ: &str = "read";
pub const ADD: &str = "add";
pub const EDIT: &str = "edit";
pub const REMOVE: &str = "remove";
pub const MANAGE_ACCESS: &str = "manage_access";
pub const COPY: &str = "copy";
pub const RENAME: &str = "rename";
pub const DELETE: &str = "delete";

/// The map owner's actions on its owner Secrets, in the order the server
/// lists them.
pub const OWNER_ACTIONS: [&str; 8] = [READ, ADD, EDIT, REMOVE, MANAGE_ACCESS, COPY, RENAME, DELETE];

/// The actions a grant may carry beyond `read`.
const GRANTABLE: [&str; 5] = [ADD, EDIT, REMOVE, MANAGE_ACCESS, COPY];

/// `read` and `held`, in the server's order.
fn with_read(held: &BTreeSet<&'static str>) -> Vec<&'static str> {
    OWNER_ACTIONS
        .into_iter()
        .filter(|action| *action == READ || held.contains(action))
        .collect()
}

impl MockState {
    /// `viewer`'s actions on `secret` of `area`: all of them for the map's
    /// owner; for anyone else the union of the grants naming them, and
    /// nothing while they cannot read the map.
    pub fn secret_actions(
        &self,
        viewer: Uuid,
        area: &AreaRecord,
        secret: &SecretRecord,
    ) -> Vec<&'static str> {
        if let Some(record) = &secret.clan {
            return super::clan_secrets::actions(self, viewer, area, record);
        }
        if area.user_id == viewer {
            return OWNER_ACTIONS.to_vec();
        }
        if !self.caps(viewer, area.id).is_some_and(|caps| caps.can_view) {
            return Vec::new();
        }
        let mut granted = false;
        let mut held = BTreeSet::new();
        for grant in secret.grants.iter().filter(|g| g.grantee_id == viewer) {
            granted = true;
            held.extend(grant.actions.iter().copied());
        }
        if granted {
            with_read(&held)
        } else {
            Vec::new()
        }
    }

    /// The Secrets of `area` that `viewer` reads, in creation order, each
    /// with the viewer's actions on it.
    /// Whether `viewer` reads Secret `secret` on map `map`. A Secret that
    /// is missing, or not on that map, reads as unreadable.
    pub fn reads_secret(&self, viewer: Uuid, map: Uuid, secret: Uuid) -> bool {
        self.areas.get(&map).is_some_and(|area| {
            area.secrets
                .iter()
                .find(|candidate| candidate.id == secret)
                .is_some_and(|secret| !self.secret_actions(viewer, area, secret).is_empty())
                || area.private_sources.get(&viewer).is_some_and(|private| {
                    private.id == secret
                        && super::source_refs::readable(
                            self,
                            viewer,
                            area,
                            super::source_refs::Source::Private(viewer),
                        )
                })
        })
    }

    /// Private uses an internal author-specific identity in stored references,
    /// but the wire spelling is always `private` and only reaches its author.
    pub fn destination_source(&self, map: Uuid, source: Uuid) -> String {
        if self.areas.get(&map).is_some_and(|area| {
            area.private_sources
                .values()
                .any(|private| private.id == source)
        }) {
            "private".into()
        } else {
            source.to_string()
        }
    }

    /// Exit content is already scoped to its readable owning source.
    #[allow(clippy::unused_self)] // Keep the common projection interface; destination access only redacts.
    pub fn shows_exit(&self, _viewer: Uuid, _exit: &super::state::ExitRecord) -> bool {
        true
    }

    /// Resolve a cross-map destination by stable room identity, then apply
    /// the current room source's Read gate. These facts never enter the wire.
    pub fn resolved_exit(
        &self,
        viewer: Uuid,
        origin: Uuid,
        exit: &super::state::ExitRecord,
    ) -> (super::state::ExitRecord, bool) {
        let mut resolved = exit.clone();
        let Some(target) = exit.to_area_id else {
            return (resolved, true);
        };
        if target == origin {
            return (resolved, true);
        }
        let Some(area) = self.areas.get(&target) else {
            return (resolved, false);
        };
        if let Some(identity) = exit.to_room_identity {
            let location = super::source_refs::find(area, identity).map(|(source, room)| {
                (
                    super::source_refs::record(area, source).map(|record| record.id),
                    room.room_number,
                )
            });
            let Some((source, number)) = location else {
                return (resolved, false);
            };
            resolved.to_secret = source;
            resolved.to_room_number = Some(number);
        }
        let readable = match resolved.to_secret {
            Some(secret) => self.reads_secret(viewer, target, secret),
            None => self.caps(viewer, target).is_some_and(|caps| caps.can_view),
        };
        (resolved, readable)
    }

    /// Bind legacy coordinates before a mutation can move or delete the room.
    pub fn bind_room_references(&mut self) {
        for area in self.areas.values_mut() {
            super::source_refs::bind(area);
        }
        let mut identities = std::collections::HashMap::new();
        for area in self.areas.values() {
            for room in area.rooms.values() {
                if room.anchor.is_none() {
                    identities.insert((area.id, None, room.room_number), room.identity);
                }
            }
            for secret in area.secrets.iter().chain(area.private_sources.values()) {
                for (number, room) in &secret.rooms {
                    if room.anchor.is_none() && super::state::map_room_of(*number).is_none() {
                        identities.insert((area.id, Some(secret.id), *number), room.identity);
                    }
                }
            }
        }
        for area in self.areas.values_mut() {
            for exit in area
                .exits
                .iter_mut()
                .chain(area.secrets.iter_mut().flat_map(|secret| &mut secret.exits))
                .chain(
                    area.private_sources
                        .values_mut()
                        .flat_map(|private| &mut private.exits),
                )
            {
                if exit.to_room_identity.is_none()
                    && exit.to_area_id.is_some_and(|target| target != area.id)
                {
                    exit.to_room_identity =
                        exit.to_area_id
                            .zip(exit.to_room_number)
                            .and_then(|(target, number)| {
                                identities.get(&(target, exit.to_secret, number)).copied()
                            });
                }
            }
        }
    }

    pub fn readable_secrets<'a>(
        &self,
        viewer: Uuid,
        area: &'a AreaRecord,
    ) -> Vec<(&'a SecretRecord, Vec<&'static str>)> {
        area.secrets
            .iter()
            .map(|secret| (secret, self.secret_actions(viewer, area, secret)))
            .filter(|(_, actions)| !actions.is_empty())
            .collect()
    }

    /// Deletes the Secret grants between `a` and `b`, either way, as
    /// unfriending does. A block (`owner_wide`) also deletes the grants on
    /// either one's Secrets to the other, whoever issued them.
    pub fn revoke_secret_grants(&mut self, a: Uuid, b: Uuid, owner_wide: bool) {
        for area in self.areas.values_mut() {
            let owner = area.user_id;
            for secret in &mut area.secrets {
                secret.grants.retain(|grant| {
                    let pair = (grant.grantor_id == a && grant.grantee_id == b)
                        || (grant.grantor_id == b && grant.grantee_id == a);
                    let owned = owner_wide
                        && ((owner == a && grant.grantee_id == b)
                            || (owner == b && grant.grantee_id == a));
                    !(pair || owned)
                });
            }
        }
    }

    /// Whether `grantor` may give `grantee` something of `owner`'s: friends,
    /// with no block between them or between the owner and the grantee.
    fn socially_allowed(&self, grantor: Uuid, grantee: Uuid, owner: Uuid) -> bool {
        self.are_friends(grantor, grantee)
            && !self.blocked_pair(grantor, grantee)
            && !self.blocked_pair(owner, grantee)
    }
}

/// The map holding Secret `id` and the Secret's index in it.
fn find_secret(st: &MockState, id: Uuid) -> Option<(Uuid, usize)> {
    st.areas.values().find_map(|area| {
        area.secrets
            .iter()
            .position(|secret| secret.id == id)
            .map(|index| (area.id, index))
    })
}

/// Whether a caller holding `mine` may leave a grant issued by `grantor`
/// with `after`, where it held `before`: within the caller's own actions,
/// and `manage_access` only by the owner on grants the owner issued.
fn may_change(
    mine: &[&str],
    caller_is_owner: bool,
    before: &BTreeSet<&'static str>,
    after: &BTreeSet<&'static str>,
    grantor: Uuid,
    owner: Uuid,
) -> bool {
    if !mine.contains(&MANAGE_ACCESS) {
        return false;
    }
    if caller_is_owner {
        return !after.contains(MANAGE_ACCESS) || grantor == owner;
    }
    if before.contains(MANAGE_ACCESS) || after.contains(MANAGE_ACCESS) {
        return false;
    }
    after
        .iter()
        .all(|action| before.contains(action) || mine.contains(action))
}

fn view(area: &AreaRecord, secret: &SecretRecord, grant: &SecretGrantRecord) -> Value {
    json!({
        "id": grant.id,
        "secret_id": secret.id,
        "area_id": area.id,
        "owner_id": area.user_id,
        "grantor_id": grant.grantor_id,
        "grantee_id": grant.grantee_id,
        "actions": with_read(&grant.actions),
        "created_at": grant.created_at,
        "updated_at": grant.updated_at,
    })
}

fn parse_secret_id(raw: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(raw).map_err(|_| bad_request("Invalid Secret ID"))
}

/// A grant id in a path: a malformed one is the same 404 as a missing
/// grant, before authentication.
fn parse_grant_id(raw: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(raw).map_err(|_| not_found())
}

fn grant_gate(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (viewer, _) = authenticate(st, headers)?;
    gate_verified(st, viewer)?;
    Ok(viewer)
}

/// The body's `actions`: a list of action names. `read` is implicit and
/// may be listed; anything else unknown is a 400.
fn read_actions(body: &Value) -> Result<BTreeSet<&'static str>, Response> {
    let Some(list) = body.get("actions").and_then(Value::as_array) else {
        return Err(bad_request("`actions` must be a list of strings"));
    };
    let mut actions = BTreeSet::new();
    for item in list {
        let Some(name) = item.as_str() else {
            return Err(bad_request("`actions` must be a list of strings"));
        };
        if name == READ {
            continue;
        }
        let Some(action) = GRANTABLE.into_iter().find(|known| *known == name) else {
            return Err(bad_request(&format!("unknown action `{name}`")));
        };
        actions.insert(action);
    }
    Ok(actions)
}

// ---------------------------------------------------------------------------
// GET /secrets/{id}/grants
// ---------------------------------------------------------------------------

/// Every grant for the owner and holders of `manage_access`; the grants
/// naming the caller for any other reader. Oldest first.
pub async fn list_grants(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let secret_id = match parse_secret_id(&raw_id) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let st = state.lock();
    let viewer = match grant_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some((area_id, index)) = find_secret(&st, secret_id) else {
        return not_found();
    };
    if st.areas[&area_id].secrets[index].clan.is_some() {
        return super::clan_secret_grants::list(&st, viewer, (area_id, index));
    }
    let area = &st.areas[&area_id];
    let secret = &area.secrets[index];
    let mine = st.secret_actions(viewer, area, secret);
    if !mine.contains(&READ) {
        return not_found();
    }
    let manager = mine.contains(&MANAGE_ACCESS);
    let rows: Vec<Value> = secret
        .grants
        .iter()
        .filter(|grant| manager || grant.grantee_id == viewer)
        .map(|grant| {
            let mut row = view(area, secret, grant);
            if let Some(nickname) = st.user(grant.grantee_id).and_then(|u| u.nickname.clone()) {
                row["grantee_nickname"] = json!(nickname);
            }
            row
        })
        .collect();
    ok(json!(rows))
}

// ---------------------------------------------------------------------------
// POST /secrets/{id}/grants
// ---------------------------------------------------------------------------

/// Shares the Secret with a friend. A second grant from the same grantor to
/// the same grantee replaces the first one's actions and keeps its id.
pub async fn create_grant(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let secret_id = match parse_secret_id(&raw_id) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let mut st = state.lock();
    let viewer = match grant_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // The body's shape is judged before the Secret is looked up.
    let body: Value = match parse_body(&body) {
        Ok(body) => body,
        Err(e) => return e,
    };
    let recipient = match super::clan_secret_grants::read_recipient(&body) {
        Ok(recipient) => recipient,
        Err(e) => return e,
    };
    let actions = match read_actions(&body) {
        Ok(actions) => actions,
        Err(e) => return e,
    };
    let Some((area_id, index)) = find_secret(&st, secret_id) else {
        return not_found();
    };
    if st.areas[&area_id].secrets[index].clan.is_some() {
        return super::clan_secret_grants::create(
            &mut st,
            viewer,
            (area_id, index),
            recipient,
            actions,
        );
    }
    let grantee = match recipient {
        ClanRecipient::User(user) => user,
        ClanRecipient::Group(_) => {
            let area = &st.areas[&area_id];
            let mine = st.secret_actions(viewer, area, &area.secrets[index]);
            return if mine.contains(&MANAGE_ACCESS) {
                bad_request("An owner Secret is shared with users, not groups")
            } else {
                not_found()
            };
        }
    };

    let area = &st.areas[&area_id];
    let owner = area.user_id;
    let secret = &area.secrets[index];
    let mine = st.secret_actions(viewer, area, secret);
    let existing = secret
        .grants
        .iter()
        .position(|grant| grant.grantor_id == viewer && grant.grantee_id == grantee);
    let before = existing.map_or_else(BTreeSet::new, |at| secret.grants[at].actions.clone());
    if grantee == owner
        || grantee == viewer
        || !may_change(&mine, viewer == owner, &before, &actions, viewer, owner)
        || !st.socially_allowed(viewer, grantee, owner)
    {
        return not_found();
    }

    let now = Utc::now();
    let area = st.areas.get_mut(&area_id).expect("found above");
    let secret = &mut area.secrets[index];
    let at = if let Some(at) = existing {
        let grant = &mut secret.grants[at];
        grant.actions = actions;
        grant.updated_at = now;
        at
    } else {
        secret.grants.push(SecretGrantRecord {
            id: Uuid::new_v4(),
            grantor_id: viewer,
            grantee_id: grantee,
            actions,
            created_at: now,
            updated_at: now,
        });
        secret.grants.len() - 1
    };
    let area = &st.areas[&area_id];
    let secret = &area.secrets[index];
    created(view(area, secret, &secret.grants[at]))
}

// ---------------------------------------------------------------------------
// PATCH /secrets/{id}/grants/{grant}
// ---------------------------------------------------------------------------

/// Replaces a grant's actions. Adding one re-checks the grantor's and
/// grantee's friendship and blocks.
pub async fn update_grant(
    State(state): State<Shared>,
    Path((raw_secret, raw_grant)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let secret_id = match parse_secret_id(&raw_secret) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let grant_id = match parse_grant_id(&raw_grant) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let mut st = state.lock();
    let viewer = match grant_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let actions = match parse_body::<Value>(&body).and_then(|body| read_actions(&body)) {
        Ok(actions) => actions,
        Err(e) => return e,
    };
    let Some((area_id, index)) = find_secret(&st, secret_id) else {
        return not_found();
    };
    if st.areas[&area_id].secrets[index].clan.is_some() {
        return super::clan_secret_grants::update(
            &mut st,
            viewer,
            (area_id, index),
            grant_id,
            actions,
        );
    }

    let area = &st.areas[&area_id];
    let owner = area.user_id;
    let secret = &area.secrets[index];
    let Some(at) = secret.grants.iter().position(|grant| grant.id == grant_id) else {
        return not_found();
    };
    let current = &secret.grants[at];
    let mine = st.secret_actions(viewer, area, secret);
    if !may_change(
        &mine,
        viewer == owner,
        &current.actions,
        &actions,
        current.grantor_id,
        owner,
    ) {
        return not_found();
    }
    let raised = actions
        .iter()
        .any(|action| !current.actions.contains(action));
    if raised && !st.socially_allowed(current.grantor_id, current.grantee_id, owner) {
        return not_found();
    }

    let area = st.areas.get_mut(&area_id).expect("found above");
    let grant = &mut area.secrets[index].grants[at];
    grant.actions = actions;
    grant.updated_at = Utc::now();
    let area = &st.areas[&area_id];
    let secret = &area.secrets[index];
    ok(view(area, secret, &secret.grants[at]))
}

// ---------------------------------------------------------------------------
// DELETE /secrets/{id}/grants/{grant}
// ---------------------------------------------------------------------------

/// Revokes a grant: the owner any, a holder of `manage_access` any that
/// does not carry `manage_access`. 200 with `null`.
pub async fn revoke_grant(
    State(state): State<Shared>,
    Path((raw_secret, raw_grant)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let secret_id = match parse_secret_id(&raw_secret) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let grant_id = match parse_grant_id(&raw_grant) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let mut st = state.lock();
    let viewer = match grant_gate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some((area_id, index)) = find_secret(&st, secret_id) else {
        return not_found();
    };
    if st.areas[&area_id].secrets[index].clan.is_some() {
        return super::clan_secret_grants::revoke(&mut st, viewer, (area_id, index), grant_id);
    }
    let area = &st.areas[&area_id];
    let secret = &area.secrets[index];
    let Some(at) = secret.grants.iter().position(|grant| grant.id == grant_id) else {
        return not_found();
    };
    let mine = st.secret_actions(viewer, area, secret);
    if !mine.contains(&MANAGE_ACCESS)
        || (secret.grants[at].actions.contains(MANAGE_ACCESS) && viewer != area.user_id)
    {
        return not_found();
    }
    st.areas.get_mut(&area_id).expect("found above").secrets[index]
        .grants
        .remove(at);
    ok(Value::Null)
}
