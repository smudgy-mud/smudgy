//! Source-owned data uses stable anchors. A move changes the room's address,
//! never the ownership of another source's data or links.

use super::state::{AreaRecord, MockState, RoomRecord, STAND_IN, map_room_of, stand_in};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Map,
    Secret(usize),
    Private(Uuid),
}
pub fn sources(area: &AreaRecord) -> impl Iterator<Item = Source> + '_ {
    std::iter::once(Source::Map)
        .chain((0..area.secrets.len()).map(Source::Secret))
        .chain(area.private_sources.keys().copied().map(Source::Private))
}
pub fn wire(area: &AreaRecord, source: Source) -> String {
    match source {
        Source::Map => "map".into(),
        Source::Secret(i) => area.secrets[i].id.to_string(),
        Source::Private(_) => "private".into(),
    }
}
pub fn source(area: &AreaRecord, wire: &str) -> Option<Source> {
    if wire == "map" {
        Some(Source::Map)
    } else {
        let id = Uuid::parse_str(wire).ok()?;
        area.secrets
            .iter()
            .position(|s| s.id == id)
            .map(Source::Secret)
    }
}
pub fn source_for(area: &AreaRecord, wire: &str, viewer: Uuid) -> Option<Source> {
    if wire == "private" {
        area.private_sources
            .contains_key(&viewer)
            .then_some(Source::Private(viewer))
    } else {
        source(area, wire)
    }
}
pub fn record(area: &AreaRecord, source: Source) -> Option<&super::state::SecretRecord> {
    match source {
        Source::Map => None,
        Source::Secret(i) => Some(&area.secrets[i]),
        Source::Private(user) => area.private_sources.get(&user),
    }
}
pub fn record_mut(
    area: &mut AreaRecord,
    source: Source,
) -> Option<&mut super::state::SecretRecord> {
    match source {
        Source::Map => None,
        Source::Secret(i) => Some(&mut area.secrets[i]),
        Source::Private(user) => area.private_sources.get_mut(&user),
    }
}
pub fn rooms(area: &AreaRecord, source: Source) -> &BTreeMap<i32, RoomRecord> {
    match source {
        Source::Map => &area.rooms,
        Source::Secret(i) => &area.secrets[i].rooms,
        Source::Private(user) => &area.private_sources[&user].rooms,
    }
}
pub fn rooms_mut(area: &mut AreaRecord, source: Source) -> &mut BTreeMap<i32, RoomRecord> {
    match source {
        Source::Map => &mut area.rooms,
        Source::Secret(i) => &mut area.secrets[i].rooms,
        Source::Private(user) => &mut area.private_sources.get_mut(&user).unwrap().rooms,
    }
}
pub fn own(source: Source, key: i32, room: &RoomRecord) -> bool {
    room.anchor.is_none() && (source == Source::Map || map_room_of(key).is_none())
}
pub fn find(area: &AreaRecord, identity: Uuid) -> Option<(Source, &RoomRecord)> {
    sources(area).find_map(|source| {
        rooms(area, source)
            .iter()
            .find(|(key, room)| own(source, **key, room) && room.identity == identity)
            .map(|(_, room)| (source, room))
    })
}
pub fn resolve(area: &AreaRecord, source: Source, key: i32) -> Option<(Source, &RoomRecord)> {
    let held = rooms(area, source).get(&key)?;
    if let Some(anchor) = held.anchor {
        return find(area, anchor);
    }
    if source != Source::Map
        && let Some(number) = map_room_of(key)
    {
        return area
            .rooms
            .get(&number)
            .filter(|r| r.anchor.is_none())
            .map(|r| (Source::Map, r));
    }
    Some((source, held))
}
pub fn readable(st: &MockState, viewer: Uuid, area: &AreaRecord, source: Source) -> bool {
    match source {
        Source::Map => st.caps(viewer, area.id).is_some_and(|c| c.can_view),
        Source::Secret(i) => st
            .secret_actions(viewer, area, &area.secrets[i])
            .contains(&"read"),
        Source::Private(user) => {
            user == viewer
                && super::transfer_policy::holds(
                    st,
                    area,
                    viewer,
                    &super::transfer_policy::private(st, area, user, area.atlas_id),
                    "read",
                )
        }
    }
}
pub fn reads_anchor(
    st: &MockState,
    viewer: Uuid,
    area: &AreaRecord,
    source: Source,
    key: i32,
) -> bool {
    resolve(area, source, key).is_some_and(|(owner, _)| readable(st, viewer, area, owner))
}
pub fn bind(area: &mut AreaRecord) {
    let map: BTreeMap<_, _> = area
        .rooms
        .iter()
        .filter(|(_, r)| r.anchor.is_none())
        .map(|(k, r)| (*k, r.identity))
        .collect();
    for secret in area
        .secrets
        .iter_mut()
        .chain(area.private_sources.values_mut())
    {
        for (key, room) in &mut secret.rooms {
            if room.anchor.is_none()
                && let Some(number) = map_room_of(*key)
            {
                // A missing legacy target stays unresolved; it cannot acquire
                // a future room merely because that number is reused.
                room.anchor = Some(map.get(&number).copied().unwrap_or_else(Uuid::new_v4));
            }
        }
    }
}
pub fn keys(area: &AreaRecord, source: Source) -> BTreeMap<i32, Uuid> {
    rooms(area, source)
        .iter()
        .filter_map(|(key, room)| {
            room.anchor
                .map(|id| (*key, id))
                .or_else(|| resolve(area, source, *key).map(|(_, r)| (*key, r.identity)))
        })
        .collect()
}
/// Find/create only the source's data holder, not a physical room.
pub fn key_for(area: &mut AreaRecord, source: Source, identity: Uuid) -> Option<i32> {
    if let Some((owner, room)) = find(area, identity)
        && owner == source
    {
        return Some(room.room_number);
    }
    if let Some((key, _)) = rooms(area, source)
        .iter()
        .find(|(_, r)| r.anchor == Some(identity))
    {
        return Some(*key);
    }
    let (owner, anchor) = find(area, identity)?;
    let mut data = RoomRecord::placeholder(0);
    data.anchor = Some(identity);
    data.x = anchor.x;
    data.y = anchor.y;
    data.level = anchor.level;
    let preferred = (owner == Source::Map)
        .then(|| stand_in(anchor.room_number))
        .flatten();
    let held = rooms_mut(area, source);
    let key = preferred
        .filter(|n| !held.contains_key(n))
        .unwrap_or_else(|| {
            let mut number = STAND_IN;
            while held.contains_key(&number) {
                number += 1;
            }
            number
        });
    data.room_number = key;
    held.insert(key, data);
    Some(key)
}
pub fn key_of(area: &AreaRecord, source: Source, identity: Uuid) -> Option<i32> {
    rooms(area, source)
        .iter()
        .find(|(key, room)| {
            room.anchor == Some(identity) || (own(source, **key, room) && room.identity == identity)
        })
        .map(|(key, _)| *key)
}
pub fn prune(area: &mut AreaRecord, source: Source) {
    let mut used = std::collections::BTreeSet::new();
    for exit in super::reviewed_moves::exits(area, source) {
        used.insert(exit.from_room_number);
        if exit.to_area_id == Some(area.id) && exit.to_secret.is_none() {
            used.extend(exit.to_room_number);
        }
    }
    for link in super::reviewed_moves::connections(area, source) {
        used.insert(link.endpoint_a.room_number);
        used.extend(link.endpoint_b.as_ref().map(|b| b.room_number));
    }
    rooms_mut(area, source).retain(|key, room| {
        own(source, *key, room)
            || !room.properties.is_empty()
            || !room.tags.is_empty()
            || used.contains(key)
    });
}

pub fn bundle(
    st: &MockState,
    viewer: Uuid,
    area: &AreaRecord,
    source: Source,
    actions: &[&str],
) -> serde_json::Value {
    use serde_json::json;
    let held_exits = super::reviewed_moves::exits(area, source);
    let mut links: Vec<_> = super::reviewed_moves::connections(area, source)
        .iter()
        .filter(|c| super::reviewed_moves::link_readable(st, viewer, area, source, c.id))
        .collect();
    links.sort_by_key(|c| c.id);
    let visible_links: std::collections::BTreeSet<_> = links.iter().map(|c| c.id).collect();
    let own_wire = wire(area, source);
    let exits_from = |key| {
        let mut held: Vec<_> = held_exits
            .iter()
            .filter(|e| e.from_room_number == key && visible_links.contains(&e.connection_id))
            .collect();
        held.sort_by(|a, b| (&a.from_direction, a.id).cmp(&(&b.from_direction, b.id)));
        held.into_iter()
            .map(|exit| {
                let mut out =
                    super::secrets::secret_exit_json(st, viewer, area.id, &own_wire, exit);
                if let Some((_, room)) = resolve(area, source, exit.from_room_number) {
                    out["from_room_number"] = json!(room.room_number);
                }
                if exit.to_area_id == Some(area.id)
                    && exit.to_secret.is_none()
                    && let Some(key) = exit.to_room_number
                    && let Some((owner, room)) = resolve(area, source, key)
                {
                    out["to_room_number"] = json!(room.room_number);
                    out.as_object_mut().unwrap().remove("to_source");
                    if owner != Source::Map {
                        out["to_source"] = json!(wire(area, owner));
                    }
                }
                out
            })
            .collect::<Vec<_>>()
    };
    let mut own_rooms = Vec::new();
    let mut data = Vec::new();
    for (key, room) in rooms(area, source) {
        let Some((owner, anchor)) = resolve(area, source, *key) else {
            continue;
        };
        if !readable(st, viewer, area, owner) {
            continue;
        }
        let props: Vec<_> = room
            .properties
            .iter()
            .map(|(n, p)| json!({"name":n,"value":p.value}))
            .collect();
        let exits = exits_from(*key);
        if own(source, *key, room) {
            own_rooms.push(json!({"room_number":room.room_number,"title":room.title,"description":room.description,
                "color":room.color,"level":room.level,"x":room.x,"y":room.y,"properties":props,
                "exits":exits,"tags":room.tags,"external_id":room.external_id}));
        } else if !props.is_empty() || !room.tags.is_empty() || !exits.is_empty() {
            let mut entry = json!({"room_number":anchor.room_number,"properties":props,"tags":room.tags,"exits":exits});
            if owner != Source::Map {
                entry["room_source"] = json!(wire(area, owner));
            }
            data.push(entry);
        }
    }
    let connections: Vec<_> = links.into_iter().map(|c| {
        let endpoint = |e: &super::state::EndpointRecord| {
            let (owner,r) = resolve(area,source,e.room_number).expect("visible endpoint");
            let mut out = super::projection::endpoint_json(e); out["room_number"] = json!(r.room_number);
            if owner != Source::Map { out["source"] = json!(wire(area,owner)); }
            out
        };
        let a = resolve(area,source,c.endpoint_a.room_number).unwrap();
        let b = c.endpoint_b.as_ref().and_then(|b| resolve(area,source,b.room_number));
        let kind = match b {
            None if held_exits.iter().any(|e| e.connection_id == c.id && e.to_area_id.is_some_and(|id| id != area.id)) => "External",
            None => "Dangling", Some(b) if a.1.identity == b.1.identity => "SelfLoop",
            Some(b) if a.1.level == b.1.level => "Internal", Some(_) => "CrossLevel",
        };
        let mut out = json!({"id":c.id,"endpoint_a":endpoint(&c.endpoint_a),"kind":kind,"routing":c.routing,
            "segment_shape":c.segment_shape,"corner":c.corner,"route_points":c.route_points.iter().map(|(x,y)| json!({"x":x,"y":y})).collect::<Vec<_>>(),
            "dash":c.dash,"color":c.color,"thickness":c.thickness});
        if let Some(b) = &c.endpoint_b { out["endpoint_b"] = endpoint(b); } out
    }).collect();
    let (props, labels, shapes) = match source {
        Source::Map => (&area.properties, &area.labels, &area.shapes),
        Source::Secret(i) => {
            let s = &area.secrets[i];
            (&s.properties, &s.labels, &s.shapes)
        }
        Source::Private(user) => {
            let s = &area.private_sources[&user];
            (&s.properties, &s.labels, &s.shapes)
        }
    };
    let mut out = json!({"source":own_wire,"rev":super::reviewed_moves::rev(area,source),"actions":actions,
        "properties":props.iter().map(|(n,p)| json!({"name":n,"value":p.value})).collect::<Vec<_>>(),
        "rooms":own_rooms,"room_data":data,"connections":connections,
        "labels":labels.iter().map(super::secrets::label_json).collect::<Vec<_>>(),
        "shapes":shapes.iter().map(super::secrets::shape_json).collect::<Vec<_>>()});
    if let Source::Secret(i) = source {
        let s = &area.secrets[i];
        out["name"] = json!(s.name);
        out["color"] = json!(s.color);
        out["ownership"] = json!(s.clan.as_ref().map_or("owner", |c| c.ownership));
        if let Some(c) = &s.clan {
            out["clan_id"] = json!(c.clan_id);
        }
    }
    out
}
pub fn bundles(st: &MockState, viewer: Uuid, area: &AreaRecord) -> Vec<serde_json::Value> {
    let caps = st.caps(viewer, area.id).expect("area exists");
    let actions = super::clan_maps::area_actions(st, viewer, area);
    let mut map_actions = vec!["read"];
    for (a, clan) in [
        ("add", "area.add"),
        ("edit", "area.edit"),
        ("remove", "area.remove_content"),
    ] {
        if actions
            .as_ref()
            .map_or(caps.can_edit, |set| set.contains(clan))
        {
            map_actions.push(a);
        }
    }
    let mut out = vec![bundle(st, viewer, area, Source::Map, &map_actions)];
    for (i, secret) in area.secrets.iter().enumerate() {
        let actions = st.secret_actions(viewer, area, secret);
        if actions.contains(&"read") {
            out.push(bundle(st, viewer, area, Source::Secret(i), &actions));
        }
    }
    if area.private_sources.contains_key(&viewer)
        && readable(st, viewer, area, Source::Private(viewer))
    {
        out.push(bundle(
            st,
            viewer,
            area,
            Source::Private(viewer),
            &super::clan_secrets::SECRET_ACTIONS,
        ));
    }
    out
}
pub fn linked(st: &MockState, area: Uuid, bundles: &[serde_json::Value]) -> Vec<serde_json::Value> {
    use serde_json::{Value, json};
    let mut visible = BTreeMap::new();
    let mut hidden = std::collections::BTreeSet::new();
    for bundle in bundles {
        for room in ["rooms", "room_data"]
            .iter()
            .flat_map(|k| bundle[*k].as_array().into_iter().flatten())
        {
            for exit in room["exits"].as_array().into_iter().flatten() {
                if let Some(token) = exit["to_area_token"].as_str() {
                    hidden.insert(token.to_owned());
                }
                if let Some(id) = exit["to_area_id"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .filter(|id| *id != area)
                {
                    let mut value = json!({"to_area_id":id,"visible":true});
                    if let Some(target) = st.areas.get(&id) {
                        value["name"] = json!(target.name);
                    }
                    visible.insert(id, value);
                }
            }
        }
    }
    visible
        .into_values()
        .chain(
            hidden
                .into_iter()
                .map(|t| json!({"to_area_token":t,"visible":false})),
        )
        .collect::<Vec<Value>>()
}
