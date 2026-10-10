//! Translate source-qualified wire anchors to the document appliers' keys.
//! A stand-in holds data for a stable identity; it is never an owned room.
use super::*;
use crate::support::source_refs::{self as refs, Source};

fn written(st: &MockState, ctx: &ApplyCtx) -> Source {
    let area = &st.areas[&ctx.area_id];
    ctx.secret.map_or(Source::Map, |id| {
        if let Some(index) = area.secrets.iter().position(|s| s.id == id) {
            Source::Secret(index)
        } else {
            Source::Private(
                *area
                    .private_sources
                    .iter()
                    .find(|(_, s)| s.id == id)
                    .unwrap()
                    .0,
            )
        }
    })
}
fn address(st: &MockState, doc: &AreaRecord, ctx: &ApplyCtx, key: i32) -> Option<(String, i32)> {
    let held = doc.rooms.get(&key)?;
    if let Some(anchor) = held.anchor {
        let (source, room) = refs::find(&st.areas[&ctx.area_id], anchor)?;
        Some((
            refs::wire(&st.areas[&ctx.area_id], source),
            room.room_number,
        ))
    } else {
        Some((refs::wire(&st.areas[&ctx.area_id], written(st, ctx)), key))
    }
}
fn readable(st: &MockState, doc: &AreaRecord, ctx: &ApplyCtx, key: i32) -> bool {
    let Some(held) = doc.rooms.get(&key) else {
        return false;
    };
    held.anchor.is_none()
        || refs::find(&st.areas[&ctx.area_id], held.anchor.unwrap())
            .is_some_and(|(s, _)| refs::readable(st, ctx.viewer, &st.areas[&ctx.area_id], s))
}
fn key(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    named: Option<&str>,
    number: i32,
) -> Result<i32, Response> {
    let area = &st.areas[&ctx.area_id];
    let name = named.unwrap_or("map");
    if same_source(name, &refs::wire(area, written(st, ctx))) {
        return Ok(number);
    }
    let source = refs::source_for(area, name, ctx.viewer).ok_or_else(not_found)?;
    if !refs::readable(st, ctx.viewer, area, source) {
        return Err(not_found());
    }
    let room = refs::rooms(area, source)
        .get(&number)
        .filter(|r| refs::own(source, number, r))
        .ok_or_else(not_found)?;
    let doc = working.get_mut(&ctx.area_id).unwrap();
    if let Some((key, _)) = doc
        .rooms
        .iter()
        .find(|(_, r)| r.anchor == Some(room.identity))
    {
        return Ok(*key);
    }
    let mut key = if source == Source::Map {
        stand_in(number).unwrap_or(STAND_IN)
    } else {
        STAND_IN
    };
    while doc.rooms.contains_key(&key) {
        key += 1;
    }
    let mut data = RoomRecord::placeholder(key);
    data.anchor = Some(room.identity);
    data.level = room.level;
    data.x = room.x;
    data.y = room.y;
    doc.rooms.insert(key, data);
    Ok(key)
}
fn check_ids(
    st: &MockState,
    working: &Working,
    ctx: &ApplyCtx,
    value: &Value,
) -> Result<(), Response> {
    let doc = &working[&ctx.area_id];
    let exit_visible = |e: &ExitRecord| {
        readable(st, doc, ctx, e.from_room_number)
            && (e.to_area_id != Some(ctx.area_id)
                || e.to_secret.is_some()
                || e.to_room_number.is_none_or(|k| readable(st, doc, ctx, k)))
    };
    for field in ["exit_id", "exit_a_id", "exit_b_id"] {
        if let Some(id) = value[field].as_str().and_then(|s| Uuid::parse_str(s).ok())
            && let Some(exit) = doc.exits.iter().find(|e| e.id == id)
            && !exit_visible(exit)
        {
            return Err(not_found());
        }
    }
    if let Some(id) = value["connection_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        && let Some(link) = doc.connections.iter().find(|c| c.id == id)
        && (!readable(st, doc, ctx, link.endpoint_a.room_number)
            || link
                .endpoint_b
                .as_ref()
                .is_some_and(|b| !readable(st, doc, ctx, b.room_number))
            || doc
                .exits
                .iter()
                .filter(|e| e.connection_id == id)
                .any(|e| !exit_visible(e)))
    {
        return Err(not_found());
    }
    Ok(())
}
pub(super) fn input(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    op: AreaOp,
) -> Result<AreaOp, Response> {
    let retarget = match &op {
        AreaOp::CreateExit { .. } => true,
        AreaOp::UpdateExit { body, .. } => {
            body.to_area_id.is_some() || body.to_room_number.is_some() || body.to_source.is_some()
        }
        _ => false,
    };
    let mut value = serde_json::to_value(op).unwrap();
    check_ids(st, working, ctx, &value)?;
    if let Some(number) = value["room_number"]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
    {
        value["room_number"] = json!(key(
            st,
            working,
            ctx,
            value["room_source"].as_str(),
            number
        )?);
    }
    let operation = value["op"].as_str().unwrap_or("").to_owned();
    if matches!(operation.as_str(), "create_exit" | "update_exit") {
        let current = value["exit_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .and_then(|id| working[&ctx.area_id].exits.iter().find(|e| e.id == id))
            .cloned();
        let body = value["body"].as_object_mut().unwrap();
        if retarget && body.get("clear_to") != Some(&json!(true)) {
            let target = body
                .get("to_area_id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
                .or_else(|| current.as_ref().and_then(|e| e.to_area_id));
            if let Some(target) = target.filter(|id| *id != ctx.area_id) {
                if body.get("to_source").and_then(Value::as_str) == Some("private") {
                    let area = st.areas.get(&target).ok_or_else(not_found)?;
                    let private = area
                        .private_sources
                        .get(&ctx.viewer)
                        .ok_or_else(not_found)?;
                    if !st.reads_secret(ctx.viewer, target, private.id) {
                        return Err(not_found());
                    }
                    body.insert("to_source".into(), json!(private.id));
                    body.insert("to_area_id".into(), json!(target));
                }
            } else if target == Some(ctx.area_id) {
                if current.as_ref().is_some_and(|e| e.to_secret.is_some())
                    && !body.contains_key("to_source")
                {
                    return Err(bad_request(
                        "invalid mutation envelope: an exit into another map's Secret room changes map with `to_source`",
                    ));
                }
                let previous = current
                    .as_ref()
                    .and_then(|e| e.to_room_number)
                    .and_then(|n| address(st, &working[&ctx.area_id], ctx, n));
                let number = body
                    .get("to_room_number")
                    .and_then(Value::as_i64)
                    .and_then(|n| i32::try_from(n).ok())
                    .or_else(|| previous.as_ref().map(|(_, n)| *n));
                if let Some(number) = number {
                    let named = match body.get("to_source") {
                        Some(value) => value.as_str().unwrap_or("map"),
                        None => previous.as_ref().map_or("map", |(s, _)| s.as_str()),
                    };
                    let room = key(st, working, ctx, Some(named), number)?;
                    body.insert("to_room_number".into(), json!(room));
                    body.insert("to_area_id".into(), json!(ctx.area_id));
                    // Same-map destinations use document keys. The applier's
                    // to_secret field is reserved for cross-map references.
                    body.insert("to_source".into(), Value::Null);
                }
            }
        }
    }
    if matches!(
        operation.as_str(),
        "create_connection" | "update_connection"
    ) {
        for field in ["endpoint_a", "endpoint_b"] {
            if let Some(endpoint) = value["body"].get_mut(field).filter(|v| v.is_object()) {
                let number = endpoint["room_number"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .unwrap();
                endpoint["room_number"] =
                    json!(key(st, working, ctx, endpoint["source"].as_str(), number)?);
                endpoint.as_object_mut().unwrap().remove("source");
            }
        }
    }
    serde_json::from_value(value).map_err(|e| bad_request(&format!("invalid mutation: {e}")))
}
pub(super) fn output(st: &MockState, doc: &AreaRecord, ctx: &ApplyCtx, mut value: Value) -> Value {
    fn room(
        st: &MockState,
        doc: &AreaRecord,
        ctx: &ApplyCtx,
        value: &mut Value,
        number: &str,
        source: &str,
    ) {
        if let Some(key) = value[number].as_i64().and_then(|n| i32::try_from(n).ok())
            && let Some((owner, n)) = address(st, doc, ctx, key)
        {
            value[number] = json!(n);
            if owner == "map" {
                value.as_object_mut().unwrap().remove(source);
            } else {
                value[source] = json!(owner);
            }
        }
    }
    fn exit(st: &MockState, doc: &AreaRecord, ctx: &ApplyCtx, value: &mut Value) {
        room(st, doc, ctx, value, "from_room_number", "room_source");
        if value["to_area_id"] == json!(ctx.area_id) {
            room(st, doc, ctx, value, "to_room_number", "to_source");
        }
    }
    room(st, doc, ctx, &mut value, "room_number", "room_source");
    if let Some(e) = value.get_mut("exit") {
        exit(st, doc, ctx, e);
    }
    if let Some(r) = value.get_mut("room") {
        room(st, doc, ctx, r, "room_number", "room_source");
        if let Some(exits) = r["exits"].as_array_mut() {
            for e in exits {
                exit(st, doc, ctx, e);
            }
        }
    }
    let connection = |c: &mut Value| {
        for field in ["endpoint_a", "endpoint_b"] {
            if let Some(endpoint) = c.get_mut(field).filter(|e| e.is_object()) {
                room(st, doc, ctx, endpoint, "room_number", "source");
            }
        }
    };
    if let Some(c) = value.get_mut("connection") {
        connection(c);
    }
    if let Some(list) = value.get_mut("connections").and_then(Value::as_array_mut) {
        for c in list {
            connection(c);
        }
    }
    value
}
