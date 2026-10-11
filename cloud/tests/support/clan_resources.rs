//! The clan's access index: `GET /clans/{c}/resources?kind=areas|atlases|
//! secrets|packages`. Fidelity reference: docs/clans.md §5.4 and the
//! service's `src/library/clans/resources.ts`.
//!
//! Each kind lists, by name, what the caller holds an action on: never a
//! row, name or count of anything else. A map, folder or package row carries
//! `grants`, with the actions each gives there, the delegation each of those
//! came through, and where it reaches from, only for a caller who may
//! inspect the grants covering it (a package's: the whole clan's), and only
//! the grants they may inspect. A Clan-owned map's row lists its live
//! outside shares to those who read it and hold `area.share_external` on
//! it, clan owners among them.

use std::collections::{BTreeSet, HashMap};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use serde_json::{Value, json};
use uuid::Uuid;

use super::clan_maps::placement;
use super::clans::{
    ClanGrantScope, ClanRecipient, ClanRecord, ClanResource, covers, delegated_view, ordered,
};
use super::http::{authenticate, bad_request, email_not_verified, not_found, ok};
use super::secrets::Shared;
use super::state::MockState;

pub fn routes() -> Router<Shared> {
    Router::new().route("/clans/:clan_id/resources", get(resources))
}

fn recipient_view(recipient: ClanRecipient) -> Value {
    match recipient {
        ClanRecipient::User(id) => json!({ "user_id": id }),
        ClanRecipient::Group(id) => json!({ "group_id": id }),
    }
}

/// The grants covering `resource` that the caller may inspect, each with
/// what it gives there, for a caller who inspects grants there: clan owners,
/// and holders of `grant.inspect` on it. `None` for anyone else.
fn grants_on(
    st: &MockState,
    clan: &ClanRecord,
    viewer: Uuid,
    resource: ClanResource,
) -> Option<Vec<Value>> {
    let inspects =
        clan.has_owner(viewer) || clan.actions_on(viewer, resource).contains("grant.inspect");
    if !inspects {
        return None;
    }
    let placed = |area: Uuid| placement(st, clan.id, area).and_then(|(atlas, _)| atlas);
    let mut grants: Vec<_> = clan
        .grants
        .iter()
        .filter(|grant| covers(&grant.scope, resource) && clan.may_inspect(viewer, grant, &placed))
        .collect();
    grants.sort_by_key(|grant| (grant.created_at, grant.seq));
    let mut rows = Vec::new();
    for grant in grants {
        // What it gives on the resource itself: on a folder, its folder
        // actions alone.
        let given = clan.contribution(grant, resource);
        if given.is_empty() {
            continue;
        }
        let mut row = json!({
            "grant_id": grant.id,
            "recipient": recipient_view(grant.recipient),
            "actions": ordered(&given),
            "delegated": delegated_view(grant, &given),
        });
        match (&grant.scope, resource) {
            (ClanGrantScope::Clan, _) => row["through"] = json!("clan"),
            (ClanGrantScope::Atlases(_), ClanResource::Area { atlas, .. }) => {
                row["through"] = json!("atlas");
                row["atlas_id"] = json!(atlas);
            }
            _ => row["through"] = json!("direct"),
        }
        rows.push(row);
    }
    Some(rows)
}

fn verified(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (viewer, _) = authenticate(st, headers)?;
    if !st.email_verified(viewer) {
        return Err(email_not_verified());
    }
    Ok(viewer)
}

fn areas(st: &MockState, clan: &ClanRecord, viewer: Uuid) -> Vec<(String, Value)> {
    let mut rows = Vec::new();
    for area in st.areas.values() {
        let Some((atlas, member_owned)) = placement(st, clan.id, area.id) else {
            continue;
        };
        let resource = super::clan_maps::resource(area);
        // A Member-owned map shows only to those who read it; its grants
        // only to its active owners while it is not frozen, all naming it
        // alone.
        let actions = if member_owned {
            super::clan_maps::area_actions(st, viewer, area).unwrap_or_default()
        } else {
            clan.actions_on(viewer, resource)
        };
        if actions.is_empty() {
            continue;
        }
        let mut row = json!({
            "kind": "area",
            "id": area.id,
            "name": area.name,
            "atlas_id": atlas,
            "owner": { "kind": "clan" },
            "ownership": if member_owned { "members" } else { "clan" },
            "actions": ordered(&actions),
        });
        if member_owned {
            row["owned_by_me"] = json!(super::clan_maps::active_owner(st, viewer, area));
            if super::clan_maps::has_authority(st, viewer, area) {
                row["grants"] = json!(own_grants(clan, area.id, resource));
            }
        } else {
            if let Some(grants) = grants_on(st, clan, viewer, resource) {
                row["grants"] = json!(grants);
            }
            if super::shares::sees_outside_shares(st, viewer, area) {
                let nickname =
                    |user: Uuid| st.user(user).and_then(|record| record.nickname.clone());
                let shares: Vec<Value> = super::shares::live_outside_shares(st, area)
                    .into_iter()
                    .map(|share| {
                        json!({
                            "id": share.id,
                            "grantor_id": share.grantor_id,
                            "grantor_nickname": nickname(share.grantor_id),
                            "grantee_id": share.grantee_id,
                            "grantee_nickname": nickname(share.grantee_id),
                            "created_at": share.created_at,
                        })
                    })
                    .collect();
                row["outside_shares"] = json!(shares);
            }
        }
        rows.push((area.name.clone(), row));
    }
    rows
}

/// A Member-owned map's own grants, those naming it alone, for its owners.
fn own_grants(clan: &ClanRecord, area: Uuid, resource: ClanResource) -> Vec<Value> {
    let mut grants: Vec<_> = clan
        .grants
        .iter()
        .filter(|grant| matches!(&grant.scope, ClanGrantScope::Areas(ids) if ids.len() == 1 && ids.contains(&area)))
        .collect();
    grants.sort_by_key(|grant| (grant.created_at, grant.seq));
    grants
        .into_iter()
        .map(|grant| {
            let given = clan.contribution(grant, resource);
            json!({
                "grant_id": grant.id,
                "recipient": recipient_view(grant.recipient),
                "actions": ordered(&given),
                "delegated": delegated_view(grant, &given),
                "through": "direct",
            })
        })
        .collect()
}

fn atlases(st: &MockState, clan: &ClanRecord, viewer: Uuid) -> Vec<(String, Value)> {
    let mut rows = Vec::new();
    for atlas in st.atlases.values() {
        if atlas.clan_id != Some(clan.id) {
            continue;
        }
        let resource = ClanResource::Atlas(atlas.id);
        let actions = clan.actions_on(viewer, resource);
        if actions.is_empty() {
            continue;
        }
        let mut row = json!({
            "kind": "atlas",
            "id": atlas.id,
            "name": atlas.name,
            "owner": { "kind": "clan" },
            "actions": ordered(&actions),
        });
        if let Some(grants) = grants_on(st, clan, viewer, resource) {
            row["grants"] = json!(grants);
        }
        rows.push((atlas.name.clone(), row));
    }
    rows
}

fn secrets(st: &MockState, clan: &ClanRecord, viewer: Uuid) -> Vec<(String, Value)> {
    let mut rows = Vec::new();
    for area in st.areas.values() {
        for secret in &area.secrets {
            let Some(record) = secret
                .clan
                .as_ref()
                .filter(|record| record.clan_id == clan.id)
            else {
                continue;
            };
            let actions = super::clan_secrets::actions(st, viewer, area, record);
            if !actions.contains(&"read") {
                continue;
            }
            let atlas = placement(st, clan.id, area.id).and_then(|(atlas, _)| atlas);
            rows.push((
                secret.name.clone(),
                json!({
                    "kind": "secret",
                    "id": secret.id,
                    "name": secret.name,
                    "color": secret.color,
                    "area_id": area.id,
                    "atlas_id": atlas,
                    "ownership": record.ownership,
                    "actions": actions,
                }),
            ));
        }
    }
    rows
}

fn packages(st: &MockState, clan: &ClanRecord, viewer: Uuid) -> Vec<(String, Value)> {
    let mut rows = Vec::new();
    for package in &clan.packages {
        let actions: BTreeSet<String> = clan
            .actions_on(viewer, ClanResource::Package(package.id))
            .into_iter()
            .collect();
        if actions.is_empty() {
            continue;
        }
        let mut row = json!({
            "kind": "package",
            "id": package.id,
            "name": package.name,
            "is_public": package.is_public,
            "owner": { "kind": "clan" },
            "actions": ordered(&actions),
        });
        // Its grants show to those who inspect this package's access.
        if clan.has_owner(viewer)
            || clan
                .actions_on(viewer, ClanResource::Package(package.id))
                .contains("grant.inspect")
        {
            row["grants"] = json!(
                grants_on(st, clan, viewer, ClanResource::Package(package.id)).unwrap_or_default()
            );
        }
        rows.push((package.name.clone(), row));
    }
    rows
}

/// `GET /clans/{c}/resources`.
pub async fn resources(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    (|| -> Result<Response, Response> {
        let viewer = verified(&st, &headers)?;
        let clan_id = Uuid::parse_str(&raw_clan).map_err(|_| not_found())?;
        let clan = st
            .clans
            .clans
            .get(&clan_id)
            .filter(|clan| !clan.dissolved && clan.has_member(viewer))
            .ok_or_else(not_found)?;
        let mut rows = match query.get("kind").map(String::as_str) {
            None | Some("areas") => areas(&st, clan, viewer),
            Some("atlases") => atlases(&st, clan, viewer),
            Some("secrets") => secrets(&st, clan, viewer),
            Some("packages") => packages(&st, clan, viewer),
            Some(_) => return Err(bad_request("unknown kind")),
        };
        rows.sort_by_key(|(name, _)| name.to_lowercase());
        Ok(ok(json!(
            rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>()
        )))
    })()
    .unwrap_or_else(|response| response)
}
