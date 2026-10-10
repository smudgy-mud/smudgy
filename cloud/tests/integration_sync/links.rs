//! The link editor's contract against the mock (format-3 §2.1, §4.1, §4.3,
//! §4.4): doors, connection operations, links moved on their own, and exits
//! into another map's Secret rooms, which only that Secret's readers ever
//! see. Through the real client: doors round-trip, an exit into another
//! map's Secret room reads under the Secret's own area and routes through
//! it, and a link edit lands as one envelope in the order the server needs.

use super::*;
use serde_json::{Value, json};
use smudgy_cloud::mapper::AreaMutationBatch;
use smudgy_cloud::{Door, DoorState, ExitArgs, ExitDirection, ExitUpdates, SourceId};
use support::TestUser;

/// One request with a bearer credential: status and JSON body.
async fn call(
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let client = reqwest::Client::new();
    let mut request = client
        .request(method, url)
        .header("authorization", format!("Bearer {token}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("request sends");
    let status = response.status().as_u16();
    let text = response.text().await.expect("a body");
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn get(server: &support::MockHandle, user: &TestUser, path: &str) -> (u16, Value) {
    call(
        reqwest::Method::GET,
        &format!("{}{path}", server.base_url),
        &user.api_key,
        None,
    )
    .await
}

/// One envelope to `area` writing `source` (the map when `None`) at its
/// current revision.
async fn mutate(
    server: &support::MockHandle,
    user: &TestUser,
    area: AreaId,
    source: Option<Uuid>,
    payload: Value,
) -> (u16, Value) {
    let rev = match source {
        None => server.area_rev(area),
        Some(secret) => server.secret_rev(area, secret),
    };
    let wire = source.map_or_else(|| "map".to_string(), |secret| secret.to_string());
    let envelope = json!({
        "operation_id": Uuid::new_v4(),
        "source": wire,
        "preconditions": [{ "resource": "source", "id": area, "source": wire, "expected_rev": rev }],
        "payload": payload,
    });
    call(
        reqwest::Method::POST,
        &format!("{}/areas/{area}/mutations", server.base_url),
        &user.api_key,
        Some(envelope),
    )
    .await
}

/// A move of `connections` from `from` to `to` of `area`, at both sources'
/// current revisions.
async fn move_links(
    server: &support::MockHandle,
    user: &TestUser,
    area: AreaId,
    (from, to): (Option<Uuid>, Option<Uuid>),
    connections: &[Uuid],
) -> (u16, Value) {
    let side = |source: Option<Uuid>| {
        (
            source.map_or_else(|| "map".to_string(), |secret| secret.to_string()),
            source.map_or_else(
                || server.area_rev(area),
                |secret| server.secret_rev(area, secret),
            ),
        )
    };
    let ((from_wire, from_rev), (to_wire, to_rev)) = (side(from), side(to));
    call(
        reqwest::Method::POST,
        &format!("{}/areas/{area}/moves", server.base_url),
        &user.api_key,
        Some(json!({
            "operation_id": Uuid::new_v4(),
            "from": from_wire,
            "to": to_wire,
            "preconditions": [
                { "resource": "source", "id": area, "source": from_wire, "expected_rev": from_rev },
                { "resource": "source", "id": area, "source": to_wire, "expected_rev": to_rev },
            ],
            "connections": connections,
        })),
    )
    .await
}

fn data(body: &Value) -> &Value {
    &body["data"]
}

/// Every exit `user` is shown leaving room `room` of `area`'s map bundle.
async fn map_exits(
    server: &support::MockHandle,
    user: &TestUser,
    area: AreaId,
    room: i32,
) -> Vec<Value> {
    let (_, body) = get(server, user, &format!("/areas/{area}")).await;
    data(&body)["sources"][0]["rooms"]
        .as_array()
        .expect("rooms")
        .iter()
        .find(|candidate| candidate["room_number"] == room)
        .map(|found| found["exits"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default()
}

/// The ids of the connections `user` is shown in `area`'s map bundle.
async fn map_connections(
    server: &support::MockHandle,
    user: &TestUser,
    area: AreaId,
) -> Vec<String> {
    let (_, body) = get(server, user, &format!("/areas/{area}")).await;
    data(&body)["sources"][0]["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .map(|connection| connection["id"].as_str().unwrap().to_string())
        .collect()
}

/// `user`'s `/sync` token for `area`.
async fn token(server: &support::MockHandle, user: &TestUser, area: AreaId) -> String {
    let (_, body) = get(server, user, "/sync").await;
    data(&body)
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row["area_id"] == json!(area))
        .and_then(|row| row["projection_token"].as_str())
        .expect("a row for the map")
        .to_string()
}

fn create_exit(room: i32, body: Value) -> Value {
    let mut op = json!({ "op": "create_exit", "room_number": room });
    op["body"] = body;
    op
}

fn exit_body(id: Uuid, direction: &str, to: Option<(AreaId, i32)>) -> Value {
    json!({
        "id": id,
        "from_direction": direction,
        "to_area_id": to.map(|(area, _)| area),
        "to_room_number": to.map(|(_, room)| room),
        "to_direction": null,
        "is_hidden": false,
        "weight": 1.0,
    })
}

/// Olwen's Road (rooms 1 and 2) and Manor (rooms 1, 2), the Manor's Secret
/// "Bookcase" (its own room 1 "Hidden Library", room 2 "Reading Nook"), and
/// two friends reading both maps: Rhea also reads the Bookcase, Tam does
/// not.
struct Places {
    server: support::MockHandle,
    owner: TestUser,
    reader: TestUser,
    traveler: TestUser,
    road: AreaId,
    manor: AreaId,
    secret: Uuid,
}

impl Places {
    async fn new(tag: &str) -> Self {
        let server = MockServer::spawn().await;
        let user = |role: &str| {
            server.create_user(
                &format!("{tag}-{role}@example.com"),
                &format!("{tag}{role}"),
                true,
            )
        };
        let (owner, reader, traveler) = (user("o"), user("r"), user("t"));
        let road = server.create_area(&owner, "Road");
        let manor = server.create_area(&owner, "Manor");
        for area in [road, manor] {
            server.add_room(area, 1, "One");
            server.add_room(area, 2, "Two");
            for user in [&reader, &traveler] {
                server.grant(&owner, user, GrantScope::Area(area), GrantFlags::default());
            }
        }
        let secret = server.add_secret(manor, "Bookcase");
        server.add_secret_room(manor, secret, 1, "Hidden Library");
        server.add_secret_room(manor, secret, 2, "Reading Nook");
        server.grant_secret(manor, secret, &owner, &reader, &[]);
        Self {
            server,
            owner,
            reader,
            traveler,
            road,
            manor,
            secret,
        }
    }

    /// An exit from Road room 1 into the Bookcase's room `room`.
    fn exit_into_secret(&self, id: Uuid, room: i32) -> Value {
        let mut body = exit_body(id, "North", Some((self.manor, room)));
        body["to_source"] = json!(self.secret);
        create_exit(1, body)
    }
}

// ---------------------------------------------------------------------------
// Doors
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_door_is_created_kept_replaced_whole_and_removed() {
    let w = Places::new("doors").await;
    let exit = Uuid::new_v4();
    let mut body = exit_body(exit, "East", Some((w.road, 2)));
    body["door"] = json!({ "state": "locked", "name": "gate", "opens_with": "unlock gate;open gate", "colour": "ignored" });
    let (status, echo) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        json!([create_exit(1, body)]),
    )
    .await;
    assert_eq!(status, 200, "{echo}");
    assert_eq!(
        data(&echo)["data"][0]["exit"]["door"],
        json!({ "state": "locked", "name": "gate", "opens_with": "unlock gate;open gate" })
    );
    let door = || async { map_exits(&w.server, &w.owner, w.road, 1).await[0]["door"].clone() };
    assert_eq!(door().await["state"], "locked");

    let update = |body: Value| json!([{ "op": "update_exit", "exit_id": exit, "body": body }]);
    // Absent keeps the door.
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        update(json!({ "weight": 2.0 })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(door().await["opens_with"], "unlock gate;open gate");
    // An object replaces it whole: what it leaves out is null.
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        update(json!({ "door": { "state": "open" } })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        door().await,
        json!({ "state": "open", "name": null, "opens_with": null })
    );
    // Null removes it.
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        update(json!({ "door": null })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(door().await, Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_door_refusal_reads_as_the_server_words_it() {
    let w = Places::new("doorrefusals").await;
    let exit = Uuid::new_v4();
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        json!([create_exit(1, exit_body(exit, "East", None))]),
    )
    .await;
    assert_eq!(status, 200);
    let long = |n: usize| "x".repeat(n);
    let cases: Vec<(Value, String)> = vec![
        (
            json!({ "state": "ajar" }),
            "unknown variant `ajar` for `state`".to_string(),
        ),
        (
            json!({ "name": "gate" }),
            "missing field `state`".to_string(),
        ),
        (
            json!({ "state": 1 }),
            "expected a string for `state`".to_string(),
        ),
        (
            json!({ "state": "closed", "name": "" }),
            "`name` must not be empty; send null for none".to_string(),
        ),
        (
            json!({ "state": "closed", "opens_with": "" }),
            "`opens_with` must not be empty; send null for none".to_string(),
        ),
        (
            json!({ "state": "closed", "name": long(65) }),
            "`name` must be at most 64 characters".to_string(),
        ),
        (
            json!({ "state": "closed", "opens_with": long(256) }),
            "`opens_with` must be at most 255 characters".to_string(),
        ),
        (
            json!({ "state": "closed", "name": 7 }),
            "expected a string for `name`".to_string(),
        ),
        (
            json!("closed"),
            "expected `door` to be an object".to_string(),
        ),
    ];
    for (door, reason) in cases {
        let mut create = exit_body(Uuid::new_v4(), "West", None);
        create["door"] = door.clone();
        let update = json!({ "op": "update_exit", "exit_id": exit, "body": { "door": door } });
        for op in [create_exit(1, create), update] {
            let (status, body) = mutate(&w.server, &w.owner, w.road, None, json!([op])).await;
            assert_eq!(status, 400, "{op}");
            assert_eq!(
                body["error"],
                format!("invalid mutation envelope: {reason}"),
                "{op}"
            );
        }
    }
    // The longest name and command, counted as code points, are accepted.
    let mut create = exit_body(Uuid::new_v4(), "West", None);
    create["door"] =
        json!({ "state": "closed", "name": "é".repeat(64), "opens_with": "é".repeat(255) });
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        json!([create_exit(1, create)]),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    // The flags door replaces are refused anywhere in an exit body.
    for flag in ["is_closed", "is_locked"] {
        let mut create = exit_body(Uuid::new_v4(), "West", None);
        create[flag] = json!(false);
        let update = json!({ "op": "update_exit", "exit_id": exit, "body": { flag: true } });
        for op in [create_exit(1, create), update] {
            let (status, body) = mutate(&w.server, &w.owner, w.road, None, json!([op])).await;
            assert_eq!(status, 400);
            assert_eq!(
                body["error"],
                "invalid mutation envelope: `is_closed` and `is_locked` are replaced by `door`"
            );
        }
    }
}

/// The client writes doors whole and reads them back from the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doors_round_trip_through_the_client() {
    let w = Places::new("clientdoors").await;
    let cache = TempCacheDir::new("client-doors");
    let mapper = new_synced_mapper(&w.server.base_url, &w.owner.api_key, cache.path()).await;
    let gate = Door {
        state: DoorState::Locked,
        name: Some("gate".to_string()),
        opens_with: Some("unlock gate".to_string()),
    };
    let (exit, submission) = mapper
        .create_exit_tracked(
            RoomKey::new(w.road, RoomNumber(1)),
            ExitArgs {
                from_direction: ExitDirection::East,
                to_area_id: Some(w.road),
                to_room_number: Some(RoomNumber(2)),
                door: Some(gate.clone()),
                weight: 1.0,
                ..ExitArgs::default()
            },
        )
        .expect("queued");
    mapper
        .wait_for_mutation(submission.operation_id().unwrap())
        .await
        .expect("accepted");
    assert_eq!(
        map_exits(&w.server, &w.owner, w.road, 1).await[0]["door"]["opens_with"],
        "unlock gate"
    );
    let submission = mapper
        .update_exit(
            RoomKey::new(w.road, RoomNumber(1)),
            exit,
            ExitUpdates {
                door: Some(None),
                ..ExitUpdates::default()
            },
        )
        .expect("queued");
    mapper
        .wait_for_mutation(submission.operation_id().unwrap())
        .await
        .expect("accepted");
    assert_eq!(
        map_exits(&w.server, &w.owner, w.road, 1).await[0]["door"],
        Value::Null
    );
    tick(&mapper).await;
    let shown = mapper
        .get_current_atlas()
        .get_room(&RoomKey::new(w.road, RoomNumber(1)))
        .unwrap();
    assert_eq!(shown.get_exits()[0].door, None);
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

fn connection(id: Uuid, a: i32, b: Option<i32>) -> Value {
    let endpoint = |room: i32, side: &str| json!({ "room_number": room, "side": side, "port_offset": 0.5, "port_mode": "AutoPinned" });
    let mut body = json!({
        "id": id, "endpoint_a": endpoint(a, "East"), "routing": "Simple",
        "segment_shape": "Direct", "corner": "Sharp", "route_points": [], "dash": "Solid",
        "color": "#A4A4A4", "thickness": 1.0,
    });
    if let Some(b) = b {
        body["endpoint_b"] = endpoint(b, "West");
    }
    json!({ "op": "create_connection", "body": body })
}

fn joined(room: i32, id: Uuid, direction: &str, to: (AreaId, i32), connection: Uuid) -> Value {
    let mut body = exit_body(id, direction, Some(to));
    body["connection_id"] = json!(connection);
    create_exit(room, body)
}

/// `create_connection`, `pair`, `unlink`, `update_connection` and
/// `delete_link` follow the server; the last exit of a connection takes it
/// along.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_operations_follow_the_server() {
    let w = Places::new("connections").await;
    let (road, (link, east, west)) = (w.road, (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()));
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([
            connection(link, 1, Some(2)),
            joined(1, east, "East", (road, 2), link),
            joined(2, west, "West", (road, 1), link),
        ]),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(data(&body)["data"][0]["connection"]["kind"], "Internal");
    assert_eq!(
        map_connections(&w.server, &w.owner, road).await,
        [link.to_string()]
    );

    // A connection whose members do not run between its rooms is refused.
    let stray = Uuid::new_v4();
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([
            connection(stray, 1, Some(2)),
            joined(2, Uuid::new_v4(), "North", (road, 2), stray)
        ]),
    )
    .await;
    assert_eq!(
        (status, body["details"]["reason"].clone()),
        (422, json!("invalid_membership")),
        "{body}"
    );
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([connection(Uuid::new_v4(), 1, Some(2))]),
    )
    .await;
    assert_eq!(
        (status, body["details"]["reason"].clone()),
        (422, json!("no_members"))
    );

    // Unlink, then pair back.
    let split = Uuid::new_v4();
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([{ "op": "unlink", "exit_id": west, "new_connection_id": split }]),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(data(&body)["data"][0]["entity"], "connections");
    assert_eq!(map_connections(&w.server, &w.owner, road).await.len(), 2);
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([
            { "op": "pair", "keep_connection_id": link, "merge_connection_id": split },
            { "op": "update_connection", "connection_id": link, "body": { "dash": "Dashed" } },
        ]),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(data(&body)["data"][1]["connection"]["dash"], "Dashed");
    assert_eq!(
        map_connections(&w.server, &w.owner, road).await,
        [link.to_string()]
    );

    // The last exit of a connection takes it along.
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([{ "op": "delete_exit", "exit_id": east }]),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        map_connections(&w.server, &w.owner, road).await,
        [link.to_string()]
    );
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([{ "op": "delete_exit", "exit_id": west }]),
    )
    .await;
    assert_eq!(status, 200);
    assert!(map_connections(&w.server, &w.owner, road).await.is_empty());
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([{ "op": "delete_link", "connection_id": link }]),
    )
    .await;
    assert_eq!(
        (status, body["details"]["reason"].clone()),
        (422, json!("connection_not_found"))
    );
}

/// A swap creates the reverse exit first, joining the link's connection,
/// then deletes the old one; the other order loses the connection with its
/// last member.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_swap_lands_only_reverse_first() {
    let w = Places::new("swap").await;
    let road = w.road;
    let (link, old) = (Uuid::new_v4(), Uuid::new_v4());
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([
            connection(link, 1, Some(2)),
            joined(1, old, "East", (road, 2), link)
        ]),
    )
    .await;
    assert_eq!(status, 200);
    let reverse = joined(2, Uuid::new_v4(), "West", (road, 1), link);
    let delete = json!({ "op": "delete_exit", "exit_id": old });
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([delete.clone(), reverse.clone()]),
    )
    .await;
    assert_eq!(
        (status, body["details"]["reason"].clone()),
        (422, json!("connection_not_found"))
    );

    // Through the client: the cloud crate's swap, as the editor sends it.
    let cache = TempCacheDir::new("swap");
    let mapper = new_synced_mapper(&w.server.base_url, &w.owner.api_key, cache.path()).await;
    let shown = mapper
        .get_current_atlas()
        .get_room(&RoomKey::new(road, RoomNumber(1)))
        .unwrap();
    let exit = shown.get_exits()[0].clone();
    let args =
        smudgy_cloud::link_edits::reverse_of(road, RoomNumber(1).into(), &exit).expect("a reverse");
    let ops =
        smudgy_cloud::link_edits::swap(exit.id, exit.connection_id, RoomNumber(2).into(), args);
    let submissions = mapper
        .mutate_batches(vec![AreaMutationBatch::strict(road, ops, "Swap")])
        .expect("queued");
    for submission in submissions {
        mapper
            .wait_for_mutation(submission.operation_id().unwrap())
            .await
            .expect("accepted");
    }
    assert!(map_exits(&w.server, &w.owner, road, 1).await.is_empty());
    let back = map_exits(&w.server, &w.owner, road, 2).await;
    assert_eq!(back.len(), 1);
    assert_eq!(back[0]["connection_id"], json!(link));
    assert_eq!(
        map_connections(&w.server, &w.owner, road).await,
        [link.to_string()]
    );
}

// ---------------------------------------------------------------------------
// Moves
// ---------------------------------------------------------------------------

/// A link moves on its own with its exits, ids and connection; one naming a
/// room staying behind is `move_splits_links`; one the mover does not hold
/// is the uniform 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn links_move_on_their_own() {
    let w = Places::new("movelinks").await;
    let manor = w.manor;
    let (link, east, west) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        manor,
        None,
        json!([
            connection(link, 1, Some(2)),
            joined(1, east, "East", (manor, 2), link),
            joined(2, west, "West", (manor, 1), link),
        ]),
    )
    .await;
    assert_eq!(status, 200);
    let (map_rev, secret_rev) = (
        w.server.area_rev(manor),
        w.server.secret_rev(manor, w.secret),
    );

    let (status, body) = move_links(
        &w.server,
        &w.owner,
        manor,
        (None, Some(w.secret)),
        &[link, link],
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(w.server.area_rev(manor), map_rev + 1);
    assert_eq!(w.server.secret_rev(manor, w.secret), secret_rev + 1);
    assert!(map_connections(&w.server, &w.owner, manor).await.is_empty());
    assert!(map_exits(&w.server, &w.owner, manor, 1).await.is_empty());
    let (_, projection) = get(&w.server, &w.owner, &format!("/areas/{manor}")).await;
    let bundle = &data(&projection)["sources"][1];
    assert_eq!(bundle["connections"][0]["id"], json!(link));
    let kept: Vec<&Value> = bundle["room_data"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["exits"].as_array().unwrap())
        .collect();
    assert_eq!(kept.len(), 2, "both exits move with their ids");
    assert!(
        kept.iter()
            .any(|exit| exit["id"] == json!(east) && exit["to_room_number"] == 2)
    );

    // A source-owned link can leave its room behind, retaining a qualified anchor.
    let (own, out) = (Uuid::new_v4(), Uuid::new_v4());
    let mut body = exit_body(out, "South", Some((manor, 1)));
    body["new_connection_id"] = json!(own);
    let (status, reply) = mutate(
        &w.server,
        &w.owner,
        manor,
        Some(w.secret),
        json!([{ "op": "create_exit", "room_number": 1, "room_source": w.secret, "body": body }]),
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    let (status, body) =
        move_links(&w.server, &w.owner, manor, (Some(w.secret), None), &[own]).await;
    assert_eq!(status, 200, "{body}");
    let (_, projected) = get(&w.server, &w.owner, &format!("/areas/{manor}")).await;
    let retained = data(&projected)["sources"][0]["room_data"]
        .as_array()
        .unwrap();
    assert!(retained.iter().any(|r| {
        r["room_source"] == json!(w.secret)
            && r["exits"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["id"] == json!(out))
    }));
    let (status, _) = move_links(
        &w.server,
        &w.owner,
        manor,
        (Some(w.secret), None),
        &[Uuid::new_v4()],
    )
    .await;
    assert_eq!(status, 404);
    // The map-only link moves back out.
    let (status, body) =
        move_links(&w.server, &w.owner, manor, (Some(w.secret), None), &[link]).await;
    assert_eq!(status, 200, "{body}");
    let held: std::collections::BTreeSet<_> = map_connections(&w.server, &w.owner, manor)
        .await
        .into_iter()
        .collect();
    assert_eq!(
        held,
        [link.to_string(), own.to_string()].into_iter().collect()
    );
}

/// Moving a link through the client sends `connections`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_moves_a_link_alone() {
    let w = Places::new("clientmove").await;
    let manor = w.manor;
    let link = Uuid::new_v4();
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        manor,
        None,
        json!([
            connection(link, 1, Some(2)),
            joined(1, Uuid::new_v4(), "East", (manor, 2), link)
        ]),
    )
    .await;
    assert_eq!(status, 200);
    let cache = TempCacheDir::new("client-move");
    let mapper = new_synced_mapper(&w.server.base_url, &w.owner.api_key, cache.path()).await;
    mapper
        .move_content(
            manor,
            SourceId::Map,
            SourceId::Secret(w.secret),
            smudgy_cloud::MovedContent {
                connections: vec![smudgy_cloud::ConnectionId(link)],
                ..smudgy_cloud::MovedContent::default()
            },
        )
        .await
        .expect("the link moves");
    assert!(map_connections(&w.server, &w.owner, manor).await.is_empty());
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&manor).unwrap();
    let layer = &area.source_layers()[0];
    assert!(
        layer
            .all_anchored_exits()
            .any(|(_, exits)| exits.iter().any(|exit| exit.connection_id.0 == link)),
        "the link is the Secret's now"
    );
}

// ---------------------------------------------------------------------------
// Exits into another map's Secret rooms
// ---------------------------------------------------------------------------

/// The owning source supplies exit visibility and write authority. Only its
/// destination is gated by the target's current source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exit_into_another_maps_secret_keeps_its_own_source_permissions() {
    let w = Places::new("foreign").await;
    let road = w.road;
    let rev = w.server.area_rev(road);
    let traveler_token = token(&w.server, &w.traveler, road).await;
    let exit = Uuid::new_v4();
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([w.exit_into_secret(exit, 1)]),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(data(&body)["versions"][0]["rev"], rev + 1);
    assert_eq!(w.server.area_rev(road), rev + 1);
    let shown = map_exits(&w.server, &w.reader, road, 1).await;
    assert_eq!(shown[0]["to_source"], json!(w.secret));
    assert_eq!(shown[0]["to_area_id"], json!(w.manor));
    let redacted = map_exits(&w.server, &w.traveler, road, 1).await;
    assert_eq!(redacted.len(), 1);
    assert_eq!(redacted[0]["id"], json!(exit));
    assert_eq!(redacted[0]["to_unknown"], true);
    assert!(redacted[0]["to_area_id"].is_null());
    assert!(redacted[0].get("to_source").is_none());
    assert_ne!(token(&w.server, &w.traveler, road).await, traveler_token);
    w.server.grant(
        &w.owner,
        &w.traveler,
        GrantScope::Area(road),
        GrantFlags {
            can_edit: true,
            ..GrantFlags::default()
        },
    );
    let (status, body) = mutate(&w.server, &w.traveler, road, None, json!([
        { "op": "update_exit", "exit_id": exit, "body": { "weight": 3.0 } },
        { "op": "update_connection", "connection_id": shown[0]["connection_id"], "body": { "dash": "Dotted" } }
    ])).await;
    assert_eq!(status, 200, "{body}");
    let echo = &data(&body)["data"][0]["exit"];
    assert!(echo["to_area_id"].is_null());
    assert!(echo.get("to_source").is_none());
    assert_eq!(
        map_exits(&w.server, &w.reader, road, 1).await[0]["to_source"],
        json!(w.secret)
    );
    let (status, _) = mutate(&w.server, &w.traveler, road, None, json!([
        { "op": "update_exit", "exit_id": exit, "body": { "to_area_id": w.manor, "to_source": w.secret, "to_room_number": 2 } }
    ])).await;
    assert_eq!(status, 404);
    let (status, _) = mutate(
        &w.server,
        &w.traveler,
        road,
        None,
        json!([
            { "op": "delete_exit", "exit_id": exit }
        ]),
    )
    .await;
    assert_eq!(status, 200);
    assert!(w.server.area_rev(road) > rev + 1);
    assert!(map_connections(&w.server, &w.owner, road).await.is_empty());
}

/// Every write refusal is the uniform 404 or a request-only 400.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_into_another_maps_secret_refuse_alike() {
    let w = Places::new("foreignrefusals").await;
    let road = w.road;
    w.server.grant(
        &w.owner,
        &w.reader,
        GrantScope::Area(road),
        GrantFlags {
            can_edit: true,
            ..GrantFlags::default()
        },
    );
    w.server.grant(
        &w.owner,
        &w.traveler,
        GrantScope::Area(road),
        GrantFlags {
            can_edit: true,
            ..GrantFlags::default()
        },
    );
    let mut missing = w.exit_into_secret(Uuid::new_v4(), 1);
    missing["body"]["to_source"] = json!(Uuid::new_v4());
    let mut wrong_map = w.exit_into_secret(Uuid::new_v4(), 1);
    wrong_map["body"]["to_area_id"] = json!(road.0);
    wrong_map["body"]["to_area_id"] = json!(w.server.create_area(&w.owner, "Elsewhere"));
    let unheld = w.exit_into_secret(Uuid::new_v4(), 9);
    let mut answers = Vec::new();
    for (user, op) in [
        (&w.reader, missing),
        (&w.reader, wrong_map),
        (&w.reader, unheld),
        (&w.traveler, w.exit_into_secret(Uuid::new_v4(), 1)),
    ] {
        let (status, body) = mutate(&w.server, user, road, None, json!([op])).await;
        answers.push((status, body));
    }
    for answer in &answers {
        assert_eq!(answer, &answers[0]);
    }
    assert_eq!(answers[0].0, 404);

    // The request alone decides the 400s.
    let mut roomless = w.exit_into_secret(Uuid::new_v4(), 1);
    roomless["body"]["to_room_number"] = Value::Null;
    let mut named_map = w.exit_into_secret(Uuid::new_v4(), 1);
    named_map["body"]["to_source"] = json!("map");
    let exit = Uuid::new_v4();
    let (status, _) = mutate(
        &w.server,
        &w.reader,
        road,
        None,
        json!([w.exit_into_secret(exit, 1)]),
    )
    .await;
    assert_eq!(status, 200);
    for (op, reason) in [
        (
            roomless,
            "a `to_source` naming another map's Secret needs `to_area_id` and `to_room_number`",
        ),
        (
            named_map,
            "`to_source` with another map's `to_area_id` names a Secret on that map",
        ),
        (
            json!({ "op": "update_exit", "exit_id": exit, "body": { "to_source": w.secret, "to_room_number": 2 } }),
            "a `to_source` naming another map's Secret needs `to_area_id` and `to_room_number`",
        ),
        (
            json!({ "op": "update_exit", "exit_id": exit, "body": { "to_area_id": road, "to_room_number": 2 } }),
            "an exit into another map's Secret room changes map with `to_source`",
        ),
    ] {
        let (status, body) = mutate(&w.server, &w.reader, road, None, json!([op])).await;
        assert_eq!(status, 400, "{op}");
        assert_eq!(
            body["error"],
            format!("invalid mutation envelope: {reason}")
        );
    }
    // Retargeting to another room of the Secret, or back to the map, works.
    let (status, body) = mutate(
        &w.server,
        &w.reader,
        road,
        None,
        json!([{ "op": "update_exit", "exit_id": exit, "body": { "to_area_id": w.manor, "to_room_number": 2, "to_source": w.secret } }]),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let rev = w.server.area_rev(road);
    let (status, _) = mutate(
        &w.server,
        &w.reader,
        road,
        None,
        json!([{ "op": "update_exit", "exit_id": exit, "body": { "to_area_id": road, "to_room_number": 2, "to_source": null } }]),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        w.server.area_rev(road),
        rev + 1,
        "leaving the Secret is a change the map's readers see"
    );
}

/// A Secret room that goes takes other maps' exits into it along, with
/// their connections, and moves no revision there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_secret_room_keeps_incoming_exit_content_with_an_unknown_destination() {
    let w = Places::new("foreigngone").await;
    let road = w.road;
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([w.exit_into_secret(Uuid::new_v4(), 1)]),
    )
    .await;
    assert_eq!(status, 200);
    let rev = w.server.area_rev(road);
    let (status, body) = mutate(
        &w.server,
        &w.owner,
        w.manor,
        Some(w.secret),
        json!([{ "op": "delete_room", "room_number": 1, "room_source": w.secret }]),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let exits = map_exits(&w.server, &w.reader, road, 1).await;
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0]["to_unknown"], true);
    assert_eq!(map_connections(&w.server, &w.owner, road).await.len(), 1);
    assert_eq!(w.server.area_rev(road), rev);
}

/// Through the client: a reader names the Secret's own area; the write goes
/// out under the map and the Secret, and the exit reads, and routes,
/// through the Secret's area. A traveler's client never meets it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_links_into_another_maps_secret_through_its_area() {
    let w = Places::new("clientforeign").await;
    let road = w.road;
    w.server.grant(
        &w.owner,
        &w.reader,
        GrantScope::Area(road),
        GrantFlags {
            can_edit: true,
            ..GrantFlags::default()
        },
    );
    let cache = TempCacheDir::new("client-foreign");
    let mapper = new_synced_mapper(&w.server.base_url, &w.reader.api_key, cache.path()).await;
    let secret_area = AreaId(w.secret);
    assert!(mapper.get_current_atlas().get_area(&secret_area).is_some());
    let (exit, submission) = mapper
        .create_exit_tracked(
            RoomKey::new(road, RoomNumber(1)),
            ExitArgs {
                from_direction: ExitDirection::North,
                to_area_id: Some(secret_area),
                to_room_number: Some(RoomNumber(2)),
                door: Some(Door::new(DoorState::Closed)),
                weight: 1.0,
                ..ExitArgs::default()
            },
        )
        .expect("queued");
    mapper
        .wait_for_mutation(submission.operation_id().unwrap())
        .await
        .expect("accepted");
    let served = map_exits(&w.server, &w.reader, road, 1).await;
    assert_eq!(served[0]["to_area_id"], json!(w.manor));
    assert_eq!(served[0]["to_source"], json!(w.secret));
    assert_eq!(served[0]["to_room_number"], 2);

    tick(&mapper).await;
    let atlas = mapper.get_current_atlas();
    let room = atlas.get_room(&RoomKey::new(road, RoomNumber(1))).unwrap();
    let cached = room.get_exits().iter().find(|e| e.id == exit).unwrap();
    assert_eq!(cached.to_area_id, Some(secret_area));
    assert_eq!(cached.to_secret_map, Some(w.manor));
    assert!(atlas.can_follow_exit(smudgy_cloud::mapper::places::Sources::Shown, cached));
    assert!(!atlas.can_follow_exit(smudgy_cloud::mapper::places::Sources::Hidden, cached));
    let path = atlas
        .get_path_between_rooms(
            &RoomKey::new(road, RoomNumber(1)),
            &RoomKey::new(secret_area, RoomNumber(2)),
        )
        .expect("a route through the Secret's area");
    assert_eq!(path.last(), Some(&RoomKey::new(secret_area, RoomNumber(2))));

    let traveler_cache = TempCacheDir::new("client-foreign-traveler");
    let traveler = new_synced_mapper(
        &w.server.base_url,
        &w.traveler.api_key,
        traveler_cache.path(),
    )
    .await;
    let room = traveler
        .get_current_atlas()
        .get_room(&RoomKey::new(road, RoomNumber(1)))
        .unwrap();
    assert_eq!(room.get_exits().len(), 1);
    assert!(room.get_exits()[0].to_unknown);
    assert_eq!(room.get_exits()[0].to_area_id, None);
}

/// A copy carries an exit into another map's Secret room only for a copier
/// who reads the Secret, keeping its destination; for anyone else it is
/// left out with its connection, never dangled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copies_keep_exit_content_and_redact_unreadable_destinations() {
    let w = Places::new("foreigncopy").await;
    let road = w.road;
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        road,
        None,
        json!([w.exit_into_secret(Uuid::new_v4(), 1)]),
    )
    .await;
    assert_eq!(status, 200);
    for (user, kept) in [(&w.reader, true), (&w.traveler, false)] {
        w.server.grant(
            &w.owner,
            user,
            GrantScope::Area(road),
            GrantFlags {
                can_copy: true,
                ..GrantFlags::default()
            },
        );
        let (status, body) = call(
            reqwest::Method::POST,
            &format!("{}/areas/{road}/copy", w.server.base_url),
            &user.api_key,
            Some(json!({ "name": "Road copy" })),
        )
        .await;
        assert_eq!(status, 201, "{body}");
        let copy =
            AreaId(Uuid::parse_str(data(&body)["id"].as_str().expect("the copy's id")).unwrap());
        let exits = map_exits(&w.server, user, copy, 1).await;
        assert_eq!(exits.len(), 1, "{exits:?}");
        if kept {
            assert_eq!(exits[0]["to_area_id"], json!(w.manor));
            assert_eq!(exits[0]["to_source"], json!(w.secret));
        } else {
            assert_eq!(exits[0]["to_unknown"], true);
            assert!(exits[0].get("to_source").is_none());
        }
        assert_eq!(map_connections(&w.server, user, copy).await.len(), 1);
    }
}
