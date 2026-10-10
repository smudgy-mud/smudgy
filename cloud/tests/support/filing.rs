//! Cloudflare maps/filing-review.ts: live Secret access needs bounded sharing
//! authority. Copy is never a substitute for that authority when refiling.
use super::{
    areas::double_option,
    http::{authenticate, bad_request, not_found, ok, parse_area_id, parse_body},
    reviewed_moves::{changes, review_token},
    secrets::Shared,
    source_refs::{self as refs, Source},
    state::MockState,
    transfer_policy as policy,
};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn review(
    st: &MockState,
    viewer: Uuid,
    id: Uuid,
    atlas: Option<Uuid>,
    supplied: Option<&str>,
) -> Result<Value, Response> {
    let area = st.areas.get(&id).ok_or_else(not_found)?;
    if let Some(clan_id) = area.clan_id {
        super::clan_maps::may_update_clan_area(st, viewer, area, false, Some(atlas))?;
        let clan = &st.clans.clans[&clan_id];
        if !super::clan_maps::area_actions(st, viewer, area)
            .is_some_and(|held| held.contains("area.refile"))
            || !atlas.is_some_and(|id| {
                clan.actions_on(viewer, super::clans::ClanResource::Atlas(id))
                    .contains("atlas.accept_filing")
            })
        {
            return Err(not_found());
        }
    } else if area.user_id != viewer
        || atlas.is_some_and(|target| {
            !st.atlases
                .get(&target)
                .is_some_and(|a| a.clan_id.is_none() && a.user_id == area.user_id)
        })
    {
        return Err(not_found());
    }
    if let Some(refusal) = super::clan_maps::refused_while_disposing(st, id) {
        return Err(refusal);
    }
    let mut visible = Vec::new();
    for source in refs::sources(area).filter(|s| !matches!(s, Source::Private(_))) {
        let secret = refs::record(area, source);
        let before = policy::policy(st, area, secret, area.atlas_id);
        let after = policy::policy(st, area, secret, atlas);
        if secret.is_some()
            && !policy::may_disclose(st, area, secret, viewer, &before, &after, false)
        {
            return Err(not_found());
        }
        if policy::inspects(st, area, secret, viewer, area.atlas_id)
            && policy::inspects(st, area, secret, viewer, atlas)
        {
            visible.extend(changes(st, area, source, &before, &after));
        }
    }
    let destination = atlas.and_then(|id| st.atlases.get(&id)).map(|a| &a.name);
    Ok(review_token(
        viewer,
        id,
        json!(["filing", area.atlas_id, atlas, area.name, destination]),
        &visible,
        true,
        supplied,
    ))
}

#[derive(Deserialize)]
struct Request {
    #[serde(default, deserialize_with = "double_option")]
    atlas_id: Option<Option<Uuid>>,
}
pub async fn preview(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let st = state.lock();
    let result = (|| {
        let id = parse_area_id(&raw)?;
        let (viewer, _) = authenticate(&st, &headers)?;
        let request: Request = parse_body(&body)?;
        let atlas = request
            .atlas_id
            .ok_or_else(|| bad_request("missing field `atlas_id`"))?;
        review(&st, viewer, id, atlas, None)
    })();
    match result {
        Ok(value) => ok(value),
        Err(response) => response,
    }
}
