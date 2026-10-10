//! Atomic moves and previews. Fidelity: Cloudflare graph/moves.ts,
//! move-review.ts, move-properties.ts and authz/access-review.ts.
use super::{
    http::{
        authenticate, bad_request, err, err_with_details, not_found, ok, parse_area_id, parse_body,
    },
    secrets::Shared,
    source_refs::{self as refs, Source},
    state::{
        AreaPropRecord, AreaRecord, ConnectionRecord, ExitRecord, MockState, MutationReceipt,
        RoomPropRecord,
    },
    transfer_policy as policy,
};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use smudgy_cloud::{
    SourceId,
    mutation::{MoveRequest, PropertyAddress, PropertyChoice},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn structural(reason: &str) -> Response {
    err_with_details(409, "structural_conflict", json!({"reason": reason}))
}
fn secret(area: &AreaRecord, source: Source) -> Option<&super::state::SecretRecord> {
    refs::record(area, source)
}
fn source_policy(st: &MockState, area: &AreaRecord, source: Source) -> policy::Policy {
    if let Source::Private(author) = source {
        policy::private(st, area, author, area.atlas_id)
    } else {
        policy::policy(st, area, secret(area, source), area.atlas_id)
    }
}
fn inspects(st: &MockState, area: &AreaRecord, source: Source, user: Uuid) -> bool {
    if let Source::Private(author) = source {
        author == user && refs::readable(st, user, area, source)
    } else {
        policy::inspects(st, area, secret(area, source), user, area.atlas_id)
    }
}
pub fn rev(area: &AreaRecord, source: Source) -> i64 {
    secret(area, source).map_or(area.rev, |s| s.rev)
}
fn wire(source: SourceId) -> String {
    serde_json::to_value(source)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}
fn source(area: &AreaRecord, id: SourceId, viewer: Uuid) -> Result<Source, Response> {
    refs::source_for(area, &wire(id), viewer).ok_or_else(not_found)
}
pub fn changes(
    st: &MockState,
    area: &AreaRecord,
    source: Source,
    before: &policy::Policy,
    after: &policy::Policy,
) -> Vec<Value> {
    let describe = |audience: policy::Audience| -> Vec<Value> {
        audience
            .iter()
            .map(|clause| {
                let has_group = clause.iter().any(|a| matches!(a, policy::Atom::Group(_)));
                json!(
                    clause
                        .iter()
                        .filter_map(|atom| match atom {
                            policy::Atom::Member if has_group => None,
                            policy::Atom::Member => Some(json!({"kind":"members"})),
                            policy::Atom::User(id) => Some(json!({"kind":"user", "id":id})),
                            policy::Atom::Group(id) => {
                                let group = area
                                    .clan_id
                                    .and_then(|c| st.clans.clans.get(&c))?
                                    .groups
                                    .iter()
                                    .find(|g| g.id == *id)?;
                                Some(json!({"kind":"group", "id":id, "name":group.name}))
                            }
                        })
                        .collect::<Vec<_>>()
                )
            })
            .collect()
    };
    policy::ACTIONS
        .into_iter()
        .filter_map(|action| {
            let b = policy::audience(before, action);
            let a = policy::audience(after, action);
            let gain = policy::increases(&b, &a);
            let loss = policy::increases(&a, &b);
            (!gain.is_empty() || !loss.is_empty()).then(|| {
                json!({"source":refs::wire(area,source),
            "name":secret(area, source).map_or("Map", |s| s.name.as_str()), "action":action,
            "may_gain":describe(gain), "may_lose":describe(loss)})
            })
        })
        .collect()
}
pub fn review_token(
    viewer: Uuid,
    area: Uuid,
    binding: Value,
    changes: &[Value],
    notice: bool,
    supplied: Option<&str>,
) -> Value {
    let nonce = supplied
        .and_then(|t| t.split('.').next())
        .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned);
    // A test-only keyed digest. Bind the observable review and exact operation;
    // hidden policy is freshly authorized, never exposed through token changes.
    let signature = Sha256::digest(
        json!([
            super::state::REDACTION_KEY,
            viewer,
            area,
            nonce,
            binding,
            changes,
            notice
        ])
        .to_string()
        .as_bytes(),
    );
    json!({"token":format!("{nonce}.{}", hex::encode(signature)), "changes":changes,
        "destination_notice":notice, "requires_confirmation":notice || !changes.is_empty()})
}
pub fn check_review(review: &Value, supplied: Option<&str>) -> Result<(), Response> {
    if (review["requires_confirmation"] == true || supplied.is_some())
        && supplied != review["token"].as_str()
    {
        return Err(structural(if supplied.is_some() {
            "stale_access_review"
        } else {
            "access_review_required"
        }));
    }
    Ok(())
}
fn hash(area: Uuid, request: &MoveRequest) -> String {
    hex::encode(Sha256::digest(
        json!([
            area,
            request.from,
            request.to,
            request.rooms,
            request.connections,
            request.labels,
            request.shapes,
            request.properties,
            request.property_resolutions
        ])
        .to_string()
        .as_bytes(),
    ))
}
#[derive(Serialize)]
struct PropertyMove {
    property: PropertyAddress,
    anchor: Option<Uuid>,
    value: String,
    destination: Option<String>,
    keep: Option<PropertyChoice>,
}
struct PropertyPlan {
    items: Vec<PropertyMove>,
    conflicts: Vec<Value>,
    unresolved: bool,
}
fn property_value(
    area: &AreaRecord,
    source: Source,
    anchor: Option<Uuid>,
    name: &str,
) -> Option<String> {
    match anchor {
        Some(id) => refs::key_of(area, source, id)
            .and_then(|k| refs::rooms(area, source).get(&k))?
            .properties
            .get(name)
            .map(|p| p.value.clone()),
        None => match source {
            Source::Map => &area.properties,
            Source::Secret(i) => &area.secrets[i].properties,
            Source::Private(user) => &area.private_sources[&user].properties,
        }
        .get(name)
        .map(|p| p.value.clone()),
    }
}
fn properties(
    st: &MockState,
    user: Uuid,
    area: &AreaRecord,
    from: Source,
    to: Source,
    request: &MoveRequest,
) -> Result<PropertyPlan, Response> {
    let mut selected: Vec<(PropertyAddress, Option<Uuid>)> = Vec::new();
    for entry in &request.rooms {
        let room = refs::rooms(area, from)
            .get(&entry.room_number.0)
            .filter(|r| refs::own(from, entry.room_number.0, r))
            .ok_or_else(not_found)?;
        for name in room.properties.keys() {
            let address = PropertyAddress {
                name: name.clone(),
                room_number: Some(entry.room_number),
                room_source: request.from,
            };
            if !selected.iter().any(|(a, _)| *a == address) {
                selected.push((address, Some(room.identity)));
            }
        }
    }
    for address in &request.properties {
        if address.name.is_empty()
            || (address.room_number.is_none() && address.room_source != SourceId::Map)
        {
            return Err(bad_request("invalid property address"));
        }
        let identity = if let Some(number) = address.room_number {
            let owner = source(area, address.room_source, user)?;
            if !refs::readable(st, user, area, owner) {
                return Err(not_found());
            }
            Some(
                refs::rooms(area, owner)
                    .get(&number.0)
                    .filter(|r| refs::own(owner, number.0, r))
                    .ok_or_else(not_found)?
                    .identity,
            )
        } else {
            None
        };
        if !selected.iter().any(|(a, _)| a == address) {
            selected.push((address.clone(), identity));
        }
    }
    let can_replace = policy::holds(st, area, user, &source_policy(st, area, to), "edit");
    let mut plan = PropertyPlan {
        items: Vec::new(),
        conflicts: Vec::new(),
        unresolved: false,
    };
    let mut used = BTreeSet::new();
    for (property, anchor) in selected {
        let value = property_value(area, from, anchor, &property.name).ok_or_else(not_found)?;
        let destination = property_value(area, to, anchor, &property.name);
        let choices: Vec<_> = request
            .property_resolutions
            .iter()
            .enumerate()
            .filter(|(_, c)| c.property == property)
            .collect();
        if choices.len() > 1 {
            return Err(structural("invalid_property_resolution"));
        }
        let keep = choices.first().map(|(i, c)| {
            used.insert(*i);
            c.keep
        });
        if destination.as_ref().is_some_and(|d| *d != value) {
            plan.conflicts.push(json!({"property":property,"source_value":value,"destination_value":destination,"can_replace":can_replace}));
            if keep == Some(PropertyChoice::Source) && !can_replace {
                return Err(not_found());
            }
            plan.unresolved |= keep.is_none();
        } else if keep.is_some() {
            return Err(structural("invalid_property_resolution"));
        }
        plan.items.push(PropertyMove {
            property,
            anchor,
            value,
            destination,
            keep,
        });
    }
    if used.len() != request.property_resolutions.len() {
        return Err(structural("invalid_property_resolution"));
    }
    Ok(plan)
}
fn set_property(area: &mut AreaRecord, source: Source, item: &PropertyMove, value: Option<&str>) {
    if let Some(anchor) = item.anchor {
        let key = refs::key_for(area, source, anchor).expect("validated property room");
        let props = &mut refs::rooms_mut(area, source)
            .get_mut(&key)
            .unwrap()
            .properties;
        if let Some(value) = value {
            props.insert(
                item.property.name.clone(),
                RoomPropRecord {
                    value: value.into(),
                },
            );
        } else {
            props.remove(&item.property.name);
        }
    } else {
        let props = match source {
            Source::Map => &mut area.properties,
            Source::Secret(i) => &mut area.secrets[i].properties,
            Source::Private(user) => &mut area.private_sources.get_mut(&user).unwrap().properties,
        };
        if let Some(value) = value {
            props.insert(
                item.property.name.clone(),
                AreaPropRecord {
                    value: value.into(),
                    created_at: chrono::Utc::now(),
                },
            );
        } else {
            props.remove(&item.property.name);
        }
    }
}
pub fn exits(area: &AreaRecord, source: Source) -> &[ExitRecord] {
    match source {
        Source::Map => &area.exits,
        Source::Secret(i) => &area.secrets[i].exits,
        Source::Private(user) => &area.private_sources[&user].exits,
    }
}
pub fn connections(area: &AreaRecord, source: Source) -> &[ConnectionRecord] {
    match source {
        Source::Map => &area.connections,
        Source::Secret(i) => &area.secrets[i].connections,
        Source::Private(user) => &area.private_sources[&user].connections,
    }
}
fn links_mut(
    area: &mut AreaRecord,
    source: Source,
) -> (&mut Vec<ExitRecord>, &mut Vec<ConnectionRecord>) {
    match source {
        Source::Map => (&mut area.exits, &mut area.connections),
        Source::Secret(i) => {
            let s = &mut area.secrets[i];
            (&mut s.exits, &mut s.connections)
        }
        Source::Private(user) => {
            let s = area.private_sources.get_mut(&user).unwrap();
            (&mut s.exits, &mut s.connections)
        }
    }
}
pub fn link_readable(
    st: &MockState,
    viewer: Uuid,
    area: &AreaRecord,
    source: Source,
    id: Uuid,
) -> bool {
    let Some(connection) = connections(area, source).iter().find(|c| c.id == id) else {
        return false;
    };
    std::iter::once(connection.endpoint_a.room_number)
        .chain(connection.endpoint_b.as_ref().map(|b| b.room_number))
        .all(|key| refs::reads_anchor(st, viewer, area, source, key))
        && exits(area, source)
            .iter()
            .filter(|e| e.connection_id == id)
            .all(|e| {
                refs::reads_anchor(st, viewer, area, source, e.from_room_number)
                    && (e.to_area_id != Some(area.id)
                        || e.to_room_number
                            .is_none_or(|key| refs::reads_anchor(st, viewer, area, source, key)))
            })
}
fn validate(
    st: &MockState,
    user: Uuid,
    area: &AreaRecord,
    request: &MoveRequest,
    from: Source,
    to: Source,
    revisions: bool,
) -> Result<(), Response> {
    for (id, side, action) in [(request.from, from, "remove"), (request.to, to, "add")] {
        let p = source_policy(st, area, side);
        if !["read", action]
            .iter()
            .all(|a| policy::holds(st, area, user, &p, a))
        {
            return Err(not_found());
        }
        if revisions {
            let expected = request
                .preconditions
                .iter()
                .find(|p| p.source == id)
                .unwrap()
                .expected_rev;
            if expected != rev(area, side) {
                return Err(err_with_details(
                    409,
                    "revision_conflict",
                    json!({
                "resource":"source","id":area.id,"source":id,"operation_id":request.operation_id,"expected_rev":expected,"current_rev":rev(area,side)}),
                ));
            }
        }
    }
    if let Some(refusal) = super::clan_maps::refused_while_disposing(st, area.id) {
        return Err(refusal);
    }
    Ok(())
}
fn review(
    st: &MockState,
    user: Uuid,
    area: &AreaRecord,
    request: &MoveRequest,
    from: Source,
    to: Source,
) -> Result<(Value, PropertyPlan), Response> {
    validate(st, user, area, request, from, to, true)?;
    for id in &request.connections {
        if !link_readable(st, user, area, from, id.0) {
            return Err(not_found());
        }
    }
    let labels = match from {
        Source::Map => &area.labels,
        Source::Secret(i) => &area.secrets[i].labels,
        Source::Private(user) => &area.private_sources[&user].labels,
    };
    let shapes = match from {
        Source::Map => &area.shapes,
        Source::Secret(i) => &area.secrets[i].shapes,
        Source::Private(user) => &area.private_sources[&user].shapes,
    };
    if request
        .labels
        .iter()
        .any(|id| !labels.iter().any(|r| r.id == id.0))
        || request
            .shapes
            .iter()
            .any(|id| !shapes.iter().any(|r| r.id == id.0))
    {
        return Err(not_found());
    }
    let plan = properties(st, user, area, from, to, request)?;
    let before = source_policy(st, area, from);
    let after = source_policy(st, area, to);
    if !policy::may_disclose(st, area, secret(area, from), user, &before, &after, true) {
        return Err(not_found());
    }
    let inspect = inspects(st, area, from, user) && inspects(st, area, to, user);
    let changes = if inspect {
        changes(st, area, from, &before, &after)
    } else {
        Vec::new()
    };
    let mut review = review_token(
        user,
        area.id,
        json!([
            "move",
            request.operation_id,
            hash(area.id, request),
            request.preconditions,
            plan.items
        ]),
        &changes,
        !inspect,
        request.access_review.as_deref(),
    );
    let merged_tags = request.rooms.iter().any(|entry| {
        let room = &refs::rooms(area, from)[&entry.room_number.0];
        refs::key_of(area, to, room.identity)
            .and_then(|k| refs::rooms(area, to).get(&k))
            .is_some_and(|kept| !kept.tags.is_disjoint(&room.tags))
    });
    let undo = !merged_tags && plan.items.iter().all(|p| p.destination.is_none());
    review["preserves_undo"] = json!(undo);
    review["property_conflicts"] = json!(plan.conflicts);
    review["requires_confirmation"] =
        json!(review["requires_confirmation"] == true || !plan.conflicts.is_empty() || !undo);
    Ok((review, plan))
}

pub async fn preview(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    handle(state, raw, headers, body, false)
}
pub async fn commit(
    State(state): State<Shared>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    handle(state, raw, headers, body, true)
}
fn handle(state: Shared, raw: String, headers: HeaderMap, body: String, commit: bool) -> Response {
    let id = match parse_area_id(&raw) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let mut st = state.lock();
    let (user, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let request: MoveRequest = match parse_body(&body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if request.from == request.to {
        return bad_request("`from` and `to` must name different sources");
    }
    if request.rooms.is_empty()
        && request.connections.is_empty()
        && request.labels.is_empty()
        && request.shapes.is_empty()
        && request.properties.is_empty()
    {
        return bad_request("nothing to move");
    }
    if request.preconditions.len() != 2
        || request.preconditions.iter().any(|p| {
            p.id != id
                || p.resource != smudgy_cloud::mutation::ResourceKind::Source
                || p.expected_rev < 0
        })
        || ![request.from, request.to]
            .iter()
            .all(|s| request.preconditions.iter().any(|p| p.source == *s))
    {
        return bad_request("expected one precondition naming each of the two sources");
    }
    st.bind_room_references();
    let Some(mut area) = st.areas.get(&id).cloned() else {
        return not_found();
    };
    refs::bind(&mut area);
    if request.to == SourceId::Private && !area.private_sources.contains_key(&user) {
        let mut private = super::state::SecretRecord::new(Uuid::new_v4(), "Private".into());
        private.rev = 0;
        area.private_sources.insert(user, private);
    }
    let from = match source(&area, request.from, user) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let to = match source(&area, request.to, user) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if let Err(e) = validate(&st, user, &area, &request, from, to, false) {
        return e;
    }
    let request_hash = hash(id, &request);
    if commit && let Some(receipt) = st.mutation_receipts.get(&(user, request.operation_id)) {
        return if receipt.request_hash == request_hash {
            ok(receipt.result.clone())
        } else {
            err(409, "operation_id_reused")
        };
    }
    let (review, plan) = match review(&st, user, &area, &request, from, to) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !commit {
        return ok(review);
    }
    if let Err(e) = check_review(&review, request.access_review.as_deref()) {
        return e;
    }
    if plan.unresolved {
        return structural("move_property_conflict");
    }
    let renumbered = match apply(&st, user, &mut area, &request, from, to, &plan) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut versions = Vec::new();
    if area.member_owned.is_none() && (from == Source::Map || to == Source::Map) {
        let old = &st.areas[&id];
        let before: BTreeSet<_> = old.exits.iter().map(|e| e.id).collect();
        let after: BTreeSet<_> = area.exits.iter().map(|e| e.id).collect();
        let changed: BTreeSet<_> = before.symmetric_difference(&after).copied().collect();
        let destinations: BTreeSet<_> = old
            .exits
            .iter()
            .chain(&area.exits)
            .filter(|e| changed.contains(&e.id) && e.to_secret.is_none())
            .filter_map(|e| e.to_area_id)
            .filter(|target| {
                *target != id
                    && st
                        .areas
                        .get(target)
                        .is_some_and(|a| a.user_id == area.user_id && a.clan_id == area.clan_id)
            })
            .collect();
        for target in destinations {
            let target = st.areas.get_mut(&target).unwrap();
            target.rev += 1;
            versions.push(json!({"resource":"source","id":target.id,"source":"map","rev":target.rev,"deleted":false}));
        }
    }
    for side in [from, to] {
        match side {
            Source::Map => area.rev += 1,
            Source::Secret(i) => area.secrets[i].rev += 1,
            Source::Private(user) => area.private_sources.get_mut(&user).unwrap().rev += 1,
        }
        versions.push(json!({"resource":"source","id":id,"source":refs::wire(&area,side),"rev":rev(&area,side),"deleted":false}));
    }
    st.areas.insert(id, area);
    let result = json!({"operation_id":request.operation_id,"versions":versions,"data":[],"renumbered":renumbered});
    st.mutation_receipts.insert(
        (user, request.operation_id),
        MutationReceipt {
            request_hash,
            result: result.clone(),
        },
    );
    ok(result)
}

fn apply(
    st: &MockState,
    user: Uuid,
    area: &mut AreaRecord,
    request: &MoveRequest,
    from: Source,
    to: Source,
    plan: &PropertyPlan,
) -> Result<Vec<Value>, Response> {
    let original = area.clone();
    let asked: BTreeMap<_, _> = request
        .rooms
        .iter()
        .map(|r| (r.room_number.0, r.asked().0))
        .collect();
    let identities: BTreeSet<_> = asked
        .keys()
        .map(|n| refs::rooms(area, from)[n].identity)
        .collect();
    let source_keys = refs::keys(area, from);
    let mut moving: BTreeSet<_> = request.connections.iter().map(|id| id.0).collect();
    for exit in exits(area, from) {
        let touches = source_keys
            .get(&exit.from_room_number)
            .is_some_and(|id| identities.contains(id))
            || (exit.to_area_id == Some(area.id)
                && exit
                    .to_room_number
                    .and_then(|n| source_keys.get(&n))
                    .is_some_and(|id| identities.contains(id)));
        if touches && link_readable(st, user, area, from, exit.connection_id) {
            moving.insert(exit.connection_id);
        }
    }
    for item in &plan.items {
        if item.keep != Some(PropertyChoice::Destination) {
            set_property(area, to, item, Some(&item.value));
        }
        set_property(area, from, item, None);
    }
    let target_keys: BTreeSet<_> = refs::rooms(area, to)
        .iter()
        .filter(|(k, r)| refs::own(to, **k, r))
        .map(|(k, _)| *k)
        .collect();
    let mut occupied = target_keys.clone();
    let mut numbered = BTreeMap::new();
    let mut clashes = Vec::new();
    let mut seen = BTreeSet::new();
    for r in &request.rooms {
        let n = r.room_number.0;
        if !seen.insert(n) {
            continue;
        }
        let wanted = r.asked().0;
        if occupied.insert(wanted) {
            numbered.insert(n, wanted);
        } else {
            clashes.push(n);
        }
    }
    let mut highest = occupied.iter().copied().max().unwrap_or(0);
    for n in clashes {
        highest = highest.checked_add(1).unwrap_or(1);
        while occupied.contains(&highest) {
            highest += 1;
        }
        numbered.insert(n, highest);
        occupied.insert(highest);
    }
    for (n, new) in &numbered {
        let mut room = refs::rooms_mut(area, from)
            .remove(n)
            .ok_or_else(not_found)?;
        if let Some(key) = refs::key_of(area, to, room.identity) {
            let held = refs::rooms_mut(area, to).remove(&key).unwrap();
            room.properties.extend(held.properties);
            room.tags.extend(held.tags);
        }
        room.room_number = *new;
        refs::rooms_mut(area, to).insert(*new, room);
    }
    // Snapshot each source's numeric addresses before changing any of them.
    // All later rewrites use identities, so swaps and number reuse cannot alias.
    let (mut moved_exits, mut moved_links) = {
        let (exits, links) = links_mut(area, from);
        let (moved_exits, b): (Vec<_>, Vec<_>) = std::mem::take(exits)
            .into_iter()
            .partition(|e| moving.contains(&e.connection_id));
        *exits = b;
        let (moved_links, b): (Vec<_>, Vec<_>) = std::mem::take(links)
            .into_iter()
            .partition(|c| moving.contains(&c.id));
        *links = b;
        (moved_exits, moved_links)
    };
    for side in refs::sources(&original) {
        let keys = refs::keys(&original, side);
        let (old_exits, old_links) = links_mut(area, side);
        let mut kept_exits = std::mem::take(old_exits);
        let mut kept_links = std::mem::take(old_links);
        readdress(area, side, &keys, &mut kept_exits, &mut kept_links);
        let (e, c) = links_mut(area, side);
        *e = kept_exits;
        *c = kept_links;
    }
    readdress(area, to, &source_keys, &mut moved_exits, &mut moved_links);
    let (e, c) = links_mut(area, to);
    e.extend(moved_exits);
    c.extend(moved_links);
    let labels = match from {
        Source::Map => &mut area.labels,
        Source::Secret(i) => &mut area.secrets[i].labels,
        Source::Private(user) => &mut area.private_sources.get_mut(&user).unwrap().labels,
    };
    let (moved, kept): (Vec<_>, Vec<_>) = std::mem::take(labels)
        .into_iter()
        .partition(|l| request.labels.iter().any(|id| id.0 == l.id));
    *labels = kept;
    match to {
        Source::Map => &mut area.labels,
        Source::Secret(i) => &mut area.secrets[i].labels,
        Source::Private(user) => &mut area.private_sources.get_mut(&user).unwrap().labels,
    }
    .extend(moved);
    let shapes = match from {
        Source::Map => &mut area.shapes,
        Source::Secret(i) => &mut area.secrets[i].shapes,
        Source::Private(user) => &mut area.private_sources.get_mut(&user).unwrap().shapes,
    };
    let (moved, kept): (Vec<_>, Vec<_>) = std::mem::take(shapes)
        .into_iter()
        .partition(|l| request.shapes.iter().any(|id| id.0 == l.id));
    *shapes = kept;
    match to {
        Source::Map => &mut area.shapes,
        Source::Secret(i) => &mut area.secrets[i].shapes,
        Source::Private(user) => &mut area.private_sources.get_mut(&user).unwrap().shapes,
    }
    .extend(moved);
    for side in refs::sources(&original) {
        refs::prune(area, side);
    }
    Ok(request
        .rooms
        .iter()
        .filter_map(|r| {
            numbered
                .get(&r.room_number.0)
                .filter(|n| **n != r.room_number.0)
                .map(|n| json!({"from":r.room_number,"to":n}))
        })
        .collect())
}
fn readdress(
    area: &mut AreaRecord,
    side: Source,
    keys: &BTreeMap<i32, Uuid>,
    exits: &mut [ExitRecord],
    links: &mut [ConnectionRecord],
) {
    let mut mapping = BTreeMap::new();
    for (key, id) in keys {
        if let Some(new) = refs::key_for(area, side, *id) {
            mapping.insert(*key, new);
        }
    }
    let translate = |key: i32| mapping.get(&key).copied().unwrap_or(key);
    for exit in exits {
        exit.from_room_number = translate(exit.from_room_number);
        if exit.to_area_id == Some(area.id) && exit.to_secret.is_none() {
            exit.to_room_number = exit.to_room_number.map(translate);
        }
    }
    for link in links {
        link.endpoint_a.room_number = translate(link.endpoint_a.room_number);
        if let Some(b) = &mut link.endpoint_b {
            b.room_number = translate(b.room_number);
        }
        if let Some(b) = &mut link.endpoint_b {
            let order = |key| {
                refs::resolve(area, side, key)
                    .map(|(s, r)| (s != Source::Map, refs::wire(area, s), r.room_number))
            };
            if order(b.room_number) < order(link.endpoint_a.room_number) {
                std::mem::swap(b, &mut link.endpoint_a);
                link.route_points.reverse();
            }
        }
    }
}
