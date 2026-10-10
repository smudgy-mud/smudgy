//! The connection rule for Secrets: an exit or link that involves a
//! Secret's room, or that a Secret keeps, reaches only that Secret's
//! readers. Against the mock, a non-reader's projections, sync rows, write
//! answers and refusals read exactly as they would on a map without the
//! Secret, and no write joins two sources' rooms. Through the real client,
//! a reader who loses a Secret loses at once everything built from it: its
//! layer, doors, routes, lookups and cached bytes.

use super::*;
use serde_json::{Value, json};
use smudgy_cloud::mapper::AreaMutationBatch;
use smudgy_cloud::{ExitId, SourceId};
use support::{SecretPlace, TestUser};

/// One request with a bearer credential: status and body text.
async fn call(
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, String) {
    let client = reqwest::Client::new();
    let mut request = client
        .request(method, url)
        .header("authorization", format!("Bearer {token}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("request sends");
    let status = response.status().as_u16();
    (status, response.text().await.expect("a body"))
}

async fn get(server: &support::MockHandle, user: &TestUser, path: &str) -> (u16, String) {
    call(
        reqwest::Method::GET,
        &format!("{}{path}", server.base_url),
        &user.api_key,
        None,
    )
    .await
}

fn data(body: &str) -> Value {
    serde_json::from_str::<Value>(body).expect("a JSON body")["data"].clone()
}

/// The map's served revision, as `user` reads it.
async fn map_rev(server: &support::MockHandle, user: &TestUser, area: AreaId) -> i64 {
    let (_, body) = get(server, user, &format!("/areas/{area}")).await;
    data(&body)["sources"][0]["rev"]
        .as_i64()
        .expect("the map bundle's revision")
}

/// One envelope to `area`, writing `source` (the map when `None`) at
/// `expected_rev`, under `operation`.
async fn mutate(
    server: &support::MockHandle,
    user: &TestUser,
    area: AreaId,
    source: Option<&str>,
    expected_rev: i64,
    operation: Uuid,
    payload: Value,
) -> (u16, String) {
    let mut envelope = json!({
        "operation_id": operation,
        "preconditions": [{
            "resource": "source",
            "id": area,
            "source": source.unwrap_or("map"),
            "expected_rev": expected_rev,
        }],
        "payload": payload,
    });
    if let Some(source) = source {
        envelope["source"] = json!(source);
    }
    call(
        reqwest::Method::POST,
        &format!("{}/areas/{area}/mutations", server.base_url),
        &user.api_key,
        Some(envelope),
    )
    .await
}

/// `text` with every one of `ids` written as `as_`, so two answers about
/// different maps compare as answers.
fn normalized(text: &str, ids: &[(String, &str)]) -> String {
    ids.iter()
        .fold(text.to_string(), |text, (id, as_)| text.replace(id, as_))
}

fn exit_body(to_area: Option<Uuid>, to_room: Option<i32>) -> Value {
    json!({
        "from_direction": "Down",
        "to_area_id": to_area,
        "to_room_number": to_room,
        "to_direction": null,
        "is_hidden": true,
        "door": null,
        "weight": 1.0,
    })
}

/// Owner Olwen's manor, its twin without a Secret, and the catacombs.
///
/// - The manor and its twin: Hall 1 ⇄ Study 2 (map exits), Cellar 3,
///   Garden 4. An editor edits both.
/// - "Bookcase", a Secret of the manor: its own room 1 "Hidden Library",
///   a hidden door Study 2 → its room 1 and back, a passage it keeps
///   between Cellar 3 and Garden 4, a tunnel from Garden 4 into the
///   catacombs, and its notes on the Study.
/// - A reader reads the manor, the Secret and the catacombs; a traveler
///   reads the manor and the Secret but not the catacombs.
struct World {
    server: support::MockHandle,
    owner: TestUser,
    editor: TestUser,
    reader: TestUser,
    traveler: TestUser,
    map: AreaId,
    twin: AreaId,
    elsewhere: AreaId,
    secret: Uuid,
    door: Uuid,
    back: Uuid,
    passage: Uuid,
    tunnel: Uuid,
}

impl World {
    async fn new(tag: &str) -> Self {
        let server = MockServer::spawn().await;
        let owner = server.create_user(
            &format!("{tag}-owner@example.com"),
            &format!("{tag}o"),
            true,
        );
        let editor = server.create_user(
            &format!("{tag}-editor@example.com"),
            &format!("{tag}e"),
            true,
        );
        let reader = server.create_user(
            &format!("{tag}-reader@example.com"),
            &format!("{tag}r"),
            true,
        );
        let traveler = server.create_user(
            &format!("{tag}-traveler@example.com"),
            &format!("{tag}t"),
            true,
        );
        let map = server.create_area(&owner, "Manor");
        let twin = server.create_area(&owner, "Manor twin");
        let elsewhere = server.create_area(&owner, "Catacombs");
        for area in [map, twin] {
            for (number, title) in [(1, "Hall"), (2, "Study"), (3, "Cellar"), (4, "Garden")] {
                server.add_room(area, number, title);
            }
            server.add_exit(area, 1, "East", Some((area, 2)));
            server.add_exit(area, 2, "West", Some((area, 1)));
            server.grant(
                &owner,
                &editor,
                GrantScope::Area(area),
                GrantFlags {
                    can_edit: true,
                    ..GrantFlags::default()
                },
            );
        }
        server.add_room(elsewhere, 1, "Ossuary");
        for user in [&reader, &traveler] {
            server.grant(&owner, user, GrantScope::Area(map), GrantFlags::default());
        }
        server.grant(
            &owner,
            &reader,
            GrantScope::Area(elsewhere),
            GrantFlags::default(),
        );

        let secret = server.add_secret(map, "Bookcase");
        server.add_secret_room(map, secret, 1, "Hidden Library");
        let door = server.add_secret_exit(
            map,
            secret,
            SecretPlace::Map(2),
            "North",
            Some(SecretPlace::Own(1)),
        );
        let back = server.add_secret_exit(
            map,
            secret,
            SecretPlace::Own(1),
            "South",
            Some(SecretPlace::Map(2)),
        );
        let passage = server.add_secret_exit(
            map,
            secret,
            SecretPlace::Map(3),
            "Down",
            Some(SecretPlace::Map(4)),
        );
        let tunnel = server.add_secret_exit(
            map,
            secret,
            SecretPlace::Map(4),
            "Northeast",
            Some(SecretPlace::Elsewhere(elsewhere, 1)),
        );
        server.set_secret_room_property(
            map,
            secret,
            SecretPlace::Map(2),
            "notes",
            "Pull the red book",
        );
        for user in [&reader, &traveler] {
            server.grant_secret(map, secret, &owner, user, &[]);
        }
        Self {
            server,
            owner,
            editor,
            reader,
            traveler,
            map,
            twin,
            elsewhere,
            secret,
            door,
            back,
            passage,
            tunnel,
        }
    }

    /// Everything that would betray the Secret to someone who cannot read
    /// it: its id, name, room title and notes, its exits' and links' ids,
    /// and the map only it leads to.
    fn marks(&self) -> Vec<String> {
        let st = self.server.state.lock();
        let secret = st.areas[&self.map.0]
            .secrets
            .iter()
            .find(|secret| secret.id == self.secret)
            .expect("the Secret");
        let mut marks: Vec<String> = vec![
            self.secret.to_string(),
            "Bookcase".to_string(),
            "Hidden Library".to_string(),
            "Pull the red book".to_string(),
            self.elsewhere.to_string(),
        ];
        marks.extend(secret.exits.iter().map(|exit| exit.id.to_string()));
        marks.extend(secret.connections.iter().map(|c| c.id.to_string()));
        marks
    }
}

fn assert_no_marks(what: &str, text: &str, marks: &[String]) {
    for mark in marks {
        assert!(
            !text.contains(mark.as_str()),
            "{what} betrays {mark}: {text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The mock: reads
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_leaves_no_trace_in_what_a_non_reader_reads() {
    let w = World::new("trace").await;
    let marks = w.marks();
    let (status, projection) = get(&w.server, &w.editor, &format!("/areas/{}", w.map)).await;
    assert_eq!(status, 200);
    assert_no_marks("the editor's projection", &projection, &marks);
    let projection = data(&projection);
    assert_eq!(projection["sources"].as_array().map(Vec::len), Some(1));
    let exits_of = |room: i64| {
        projection["sources"][0]["rooms"]
            .as_array()
            .expect("rooms")
            .iter()
            .find(|candidate| candidate["room_number"] == room)
            .expect("the room")["exits"]
            .as_array()
            .expect("exits")
            .len()
    };
    assert_eq!(
        [exits_of(1), exits_of(2), exits_of(3), exits_of(4)],
        [1, 1, 0, 0]
    );
    assert_eq!(projection["linked_areas"], json!([]));
    for path in ["/areas", "/sync", &format!("/areas/{}/secrets", w.map)] {
        let (_, body) = get(&w.server, &w.editor, path).await;
        assert_no_marks(path, &body, &marks);
    }

    // The traveler reads the Secret, but not where its tunnel leads.
    let (_, text) = get(&w.server, &w.traveler, &format!("/areas/{}", w.map)).await;
    assert!(!text.contains(&w.elsewhere.to_string()), "{text}");
    let projection = data(&text);
    let bundle = &projection["sources"][1];
    assert_eq!(bundle["source"], json!(w.secret.to_string()));
    let room_data = |room: i64| {
        bundle["room_data"]
            .as_array()
            .expect("room data")
            .iter()
            .find(|data| data["room_number"] == room)
            .cloned()
            .unwrap_or_else(|| panic!("data on map room {room}"))
    };
    let study = room_data(2);
    assert_eq!(study["properties"][0]["value"], json!("Pull the red book"));
    assert_eq!(study["exits"][0]["id"], json!(w.door));
    assert_eq!(study["exits"][0]["to_source"], json!(w.secret.to_string()));
    assert_eq!(study["exits"][0]["to_room_number"], json!(1));
    let cellar = room_data(3);
    assert_eq!(cellar["exits"][0]["id"], json!(w.passage));
    assert_eq!(cellar["exits"][0]["to_room_number"], json!(4));
    assert!(cellar["exits"][0].get("to_source").is_none());
    let garden = room_data(4);
    assert_eq!(garden["exits"][0]["id"], json!(w.tunnel));
    assert_eq!(garden["exits"][0]["to_unknown"], json!(true));
    assert!(garden["exits"][0]["to_area_token"].is_string());
    let library = &bundle["rooms"][0];
    assert_eq!(library["title"], json!("Hidden Library"));
    assert_eq!(library["exits"][0]["id"], json!(w.back));
    assert_eq!(library["exits"][0]["to_room_number"], json!(2));
    assert!(library["exits"][0].get("to_source").is_none());
    let paired = bundle["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .find(|connection| {
            connection.get("endpoint_b").is_some() && {
                let ends = [&connection["endpoint_a"], &connection["endpoint_b"]];
                ends.iter().any(|end| end.get("source").is_some())
                    && ends.iter().any(|end| end.get("source").is_none())
            }
        })
        .cloned();
    assert!(
        paired.is_some(),
        "the door and its way back share a link: {bundle}"
    );

    // The reader reads the catacombs too, so the tunnel names them.
    let (_, text) = get(&w.server, &w.reader, &format!("/areas/{}", w.map)).await;
    assert!(text.contains(&w.elsewhere.to_string()));

    // Sync rows: the editor's covers the map alone, and a write to the
    // Secret moves nothing the editor sees.
    let row = |body: &str| {
        data(body)
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["area_id"] == json!(w.map))
            .cloned()
            .expect("the manor's row")
    };
    let (_, before) = get(&w.server, &w.editor, "/sync").await;
    let (_, traveler_before) = get(&w.server, &w.traveler, "/sync").await;
    assert_eq!(row(&before)["revisions"], json!({ "map": 1 }));
    let mut both_sources = serde_json::Map::new();
    both_sources.insert("map".to_string(), json!(1));
    both_sources.insert(w.secret.to_string(), json!(1));
    assert_eq!(
        row(&traveler_before)["revisions"],
        Value::Object(both_sources)
    );
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        w.map,
        Some(&w.secret.to_string()),
        1,
        Uuid::new_v4(),
        json!([{ "op": "upsert_room_property", "room_number": 4, "name": "notes", "value": "Mind the roots" }]),
    )
    .await;
    assert_eq!(status, 200);
    let (_, after) = get(&w.server, &w.editor, "/sync").await;
    let (_, traveler_after) = get(&w.server, &w.traveler, "/sync").await;
    assert_eq!(row(&after), row(&before));
    assert_ne!(
        row(&traveler_after)["projection_token"],
        row(&traveler_before)["projection_token"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_missing_an_endpoint_is_dropped() {
    let w = World::new("dropped").await;
    let stray = Uuid::new_v4();
    {
        let mut st = w.server.state.lock();
        let secret = st
            .areas
            .get_mut(&w.map.0)
            .expect("the manor")
            .secrets
            .iter_mut()
            .find(|secret| secret.id == w.secret)
            .expect("the Secret");
        let member = secret
            .exits
            .iter_mut()
            .find(|exit| exit.id == w.passage)
            .expect("the passage");
        member.connection_id = stray;
        secret
            .connections
            .push(support::state::ConnectionRecord::blank(
                stray,
                support::state::EndpointRecord {
                    room_number: 77,
                    side: "East".to_string(),
                    port_offset: 0.5,
                    port_mode: "AutoPinned".to_string(),
                },
                None,
            ));
    }
    let (_, text) = get(&w.server, &w.traveler, &format!("/areas/{}", w.map)).await;
    let bundle = data(&text)["sources"][1].clone();
    let served: Vec<Value> = bundle["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .map(|connection| connection["id"].clone())
        .collect();
    assert!(!served.contains(&json!(stray)), "{bundle}");
    assert!(
        !served.is_empty(),
        "the Secret's other links still ride: {bundle}"
    );
}

// ---------------------------------------------------------------------------
// The mock: writes by someone who cannot read the Secret
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_readers_writes_answer_as_on_a_map_without_the_secret() {
    let w = World::new("answers").await;
    let marks = w.marks();
    let ids = |extra: &[(Uuid, &'static str)]| {
        let mut ids = vec![(w.map.to_string(), "MAP"), (w.twin.to_string(), "MAP")];
        ids.extend(extra.iter().map(|(id, as_)| (id.to_string(), *as_)));
        ids
    };
    let both = |answers: [(u16, String); 2], extra: &[(Uuid, &'static str)]| {
        let [(status, text), (twin_status, twin_text)] = answers;
        assert_no_marks("an answer to the editor", &text, &marks);
        assert_eq!(status, twin_status, "{text} / {twin_text}");
        assert_eq!(
            normalized(&text, &ids(extra)),
            normalized(&twin_text, &ids(extra))
        );
        text
    };

    // Deleting the Study, which the Secret keeps a door, a way back and
    // notes on: the editor hears only of the map.
    let (on_map, on_twin) = (Uuid::new_v4(), Uuid::new_v4());
    let delete = json!([{ "op": "delete_room", "room_number": 2 }]);
    let answers = [
        mutate(&w.server, &w.editor, w.map, None, 1, on_map, delete.clone()).await,
        mutate(
            &w.server,
            &w.editor,
            w.twin,
            None,
            1,
            on_twin,
            delete.clone(),
        )
        .await,
    ];
    let first = both(answers, &[(on_map, "OP"), (on_twin, "OP")]);
    assert_eq!(
        data(&first)["versions"]
            .as_array()
            .expect("versions")
            .iter()
            .map(|version| version["source"].clone())
            .collect::<Vec<_>>(),
        [json!("map")]
    );
    // The replay is the same answer, byte for byte.
    let replay = mutate(&w.server, &w.editor, w.map, None, 1, on_map, delete).await;
    assert_eq!(replay, (200, first));

    // The Secret lost what it kept on the Study, and its readers hear so.
    assert_eq!(w.server.secret_rev(w.map, w.secret), 2);
    let (_, text) = get(&w.server, &w.traveler, &format!("/areas/{}", w.map)).await;
    let bundle = data(&text)["sources"][1].clone();
    assert!(
        bundle["room_data"]
            .as_array()
            .expect("room data")
            .iter()
            .all(|data| data["room_number"] != json!(2)),
        "{bundle}"
    );
    assert!(!text.contains(&w.door.to_string()));
    let back = &bundle["rooms"][0]["exits"][0];
    assert_eq!(back["id"], json!(w.back));
    assert_eq!(back["to_room_number"], Value::Null, "the way back dangles");

    // A new exit where the Secret keeps a passage.
    let (map_exit, twin_exit) = (Uuid::new_v4(), Uuid::new_v4());
    let (map_link, twin_link) = (Uuid::new_v4(), Uuid::new_v4());
    let create = |exit: Uuid, link: Uuid, area: AreaId| {
        let mut body = exit_body(Some(area.0), Some(4));
        body["id"] = json!(exit);
        body["new_connection_id"] = json!(link);
        json!([{ "op": "create_exit", "room_number": 3, "body": body }])
    };
    let (op_map, op_twin) = (Uuid::new_v4(), Uuid::new_v4());
    let answers = [
        mutate(
            &w.server,
            &w.editor,
            w.map,
            None,
            2,
            op_map,
            create(map_exit, map_link, w.map),
        )
        .await,
        mutate(
            &w.server,
            &w.editor,
            w.twin,
            None,
            2,
            op_twin,
            create(twin_exit, twin_link, w.twin),
        )
        .await,
    ];
    both(
        answers,
        &[
            (op_map, "OP"),
            (op_twin, "OP"),
            (map_exit, "EXIT"),
            (twin_exit, "EXIT"),
            (map_link, "LINK"),
            (twin_link, "LINK"),
        ],
    );

    // A stale revision names only the map.
    let (stale_map, stale_twin) = (Uuid::new_v4(), Uuid::new_v4());
    let tag = json!([{ "op": "add_room_tag", "room_number": 1, "tag": "lit" }]);
    let answers = [
        mutate(&w.server, &w.editor, w.map, None, 1, stale_map, tag.clone()).await,
        mutate(&w.server, &w.editor, w.twin, None, 1, stale_twin, tag).await,
    ];
    let conflict = both(answers, &[(stale_map, "OP"), (stale_twin, "OP")]);
    assert!(conflict.contains("revision_conflict"), "{conflict}");

    // The Secret's exits are no exits of the map's: touching one by id
    // reads as touching an id that names nothing.
    for (op, field) in [("delete_exit", None), ("update_exit", Some("path"))] {
        let body = |id: Uuid| {
            let mut operation = json!({ "op": op, "exit_id": id });
            if let Some(field) = field {
                operation["body"] = json!({ field: "push" });
            }
            json!([operation])
        };
        let (secret_op, random_op) = (Uuid::new_v4(), Uuid::new_v4());
        let nothing = Uuid::new_v4();
        let answers = [
            mutate(
                &w.server,
                &w.editor,
                w.map,
                None,
                3,
                secret_op,
                body(w.passage),
            )
            .await,
            mutate(
                &w.server,
                &w.editor,
                w.map,
                None,
                3,
                random_op,
                body(nothing),
            )
            .await,
        ];
        let [(status, text), (random_status, random_text)] = answers;
        assert_eq!((status, text), (random_status, random_text), "{op}");
        assert_eq!(status, 404, "{op}");
    }

    // Merging away the Garden, where the Secret keeps a tunnel into another
    // map, is as safe as on the twin.
    let (merge_map, merge_twin) = (Uuid::new_v4(), Uuid::new_v4());
    let merge =
        json!([{ "op": "assert_merge_safe", "keep_room_number": 3, "remove_room_number": 4 }]);
    let answers = [
        mutate(
            &w.server,
            &w.editor,
            w.map,
            None,
            3,
            merge_map,
            merge.clone(),
        )
        .await,
        mutate(&w.server, &w.editor, w.twin, None, 3, merge_twin, merge).await,
    ];
    let merged = both(answers, &[(merge_map, "OP"), (merge_twin, "OP")]);
    assert!(merged.contains("merge_safety_checked"), "{merged}");
}

// ---------------------------------------------------------------------------
// The mock: no write joins two sources' rooms
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_or_absent_room_anchors_refuse_alike() {
    let w = World::new("refusals").await;
    let wardrobe = w.server.add_secret(w.map, "Wardrobe");
    let far_secret = w.server.add_secret(w.elsewhere, "Crypt");
    let nothing = Uuid::new_v4();
    let rev = map_rev(&w.server, &w.editor, w.map).await;
    let marks = w.marks();

    // Asks `payload` of each, as `user` writing `source`; every answer must
    // be the first one.
    let alike = |answers: Vec<(u16, String)>, status: u16| {
        let first = answers[0].clone();
        assert_eq!(first.0, status, "{}", first.1);
        for answer in &answers {
            assert_eq!(answer, &first);
            assert_no_marks("a refusal", &answer.1, &marks);
        }
        first.1
    };

    // A hidden anchor, a room absent from another source, and a missing
    // source give the same refusal. Readable anchors are supported.
    let mut answers = Vec::new();
    for named in [
        w.secret.to_string(),
        wardrobe.to_string(),
        nothing.to_string(),
    ] {
        let mut body = exit_body(Some(w.map.0), Some(1));
        body["to_source"] = json!(named);
        answers.push(
            mutate(
                &w.server,
                &w.editor,
                w.map,
                None,
                rev,
                Uuid::new_v4(),
                json!([{ "op": "create_exit", "room_number": 3, "body": body }]),
            )
            .await,
        );
    }
    alike(answers, 404);
    let mut answers = Vec::new();
    for named in [w.secret, wardrobe, nothing] {
        answers.push(
            mutate(
                &w.server,
                &w.editor,
                w.map,
                None,
                rev,
                Uuid::new_v4(),
                json!([{ "op": "add_room_tag", "room_number": 1, "room_source": named, "tag": "x" }]),
            )
            .await,
        );
    }
    alike(answers, 404);

    // A Secret's write naming another Secret's room reads the same as one
    // naming nothing.
    let secret = w.secret.to_string();
    let mut answers = Vec::new();
    for named in [wardrobe, nothing] {
        let mut body = exit_body(Some(w.map.0), Some(1));
        body["to_source"] = json!(named);
        answers.push(
            mutate(
                &w.server,
                &w.owner,
                w.map,
                Some(&secret),
                1,
                Uuid::new_v4(),
                json!([{ "op": "create_exit", "room_number": 1, "room_source": secret, "body": body }]),
            )
            .await,
        );
    }
    alike(answers, 404);

    // An exit "into" a Secret by its id, of this map or another, leads
    // nowhere a map does: the uniform 404, from the map and from a Secret.
    for (user, source, expected_rev) in
        [(&w.editor, None, rev), (&w.owner, Some(secret.as_str()), 1)]
    {
        let mut answers = Vec::new();
        for target in [w.secret, wardrobe, far_secret, nothing] {
            answers.push(
                mutate(
                    &w.server,
                    user,
                    w.map,
                    source,
                    expected_rev,
                    Uuid::new_v4(),
                    json!([{ "op": "create_exit", "room_number": 3, "body": exit_body(Some(target), Some(1)) }]),
                )
                .await,
            );
        }
        alike(answers, 404);
    }

    // A Secret's write reaches nothing outside it: no placeholder in
    // another map, whether or not that map exists or can be read.
    let catacombs_rev = w.server.area_rev(w.elsewhere);
    let mut answers = Vec::new();
    for (target, room) in [(w.elsewhere.0, 99), (nothing, 1)] {
        answers.push(
            mutate(
                &w.server,
                &w.owner,
                w.map,
                Some(&secret),
                1,
                Uuid::new_v4(),
                json!([{ "op": "create_exit", "room_number": 3, "body": exit_body(Some(target), Some(room)) }]),
            )
            .await,
        );
    }
    alike(answers, 404);
    assert_eq!(w.server.area_rev(w.elsewhere), catacombs_rev);
    assert!(
        !w.server.state.lock().areas[&w.elsewhere.0]
            .rooms
            .contains_key(&99)
    );

    // What a Secret may do, it does, and only in itself.
    let before = get(&w.server, &w.editor, &format!("/areas/{}", w.map)).await;
    let (status, text) = mutate(
        &w.server,
        &w.owner,
        w.map,
        Some(&secret),
        1,
        Uuid::new_v4(),
        json!([{ "op": "create_exit", "room_number": 1, "body": exit_body(Some(w.map.0), Some(3)) }]),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(
        data(&text)["versions"],
        json!([{ "resource": "source", "id": w.map, "source": secret, "rev": 2, "deleted": false }])
    );
    assert_eq!(
        get(&w.server, &w.editor, &format!("/areas/{}", w.map)).await,
        before
    );
}

// ---------------------------------------------------------------------------
// The real client: a reader loses the Secret
// ---------------------------------------------------------------------------

fn secret_marks_on_disk(dir: &Path, area: AreaId, marks: &[&str]) -> Vec<String> {
    cache_files_for_area(dir, area)
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            marks
                .iter()
                .any(|mark| text.contains(mark))
                .then(|| path.display().to_string())
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn losing_a_secret_takes_its_doors_routes_lookups_and_cache_at_once() {
    let w = World::new("revoked").await;
    let cache = TempCacheDir::new("secret-revoked");
    let mapper = new_synced_mapper(&w.server.base_url, &w.traveler.api_key, cache.path()).await;
    let secret_area = AreaId(w.secret);
    let key = |area: AreaId, room: i32| RoomKey::new(area, RoomNumber(room));
    let secret_id = w.secret.to_string();
    let marks = ["Hidden Library", "Pull the red book", secret_id.as_str()];

    // What reading the Secret gives: its area, rooms, doors and routes.
    let atlas = mapper.get_current_atlas();
    assert_eq!(atlas.map_of(&secret_area), Some(w.map));
    assert_eq!(
        atlas
            .get_rooms_by_title("Hidden Library")
            .map(|(area, room)| (area, room.get_room_number()))
            .collect::<Vec<_>>(),
        [(secret_area, RoomNumber(1))]
    );
    assert!(
        atlas
            .get_path_between_rooms(&key(w.map, 3), &key(w.map, 4))
            .is_some(),
        "the passage the Secret keeps routes the Cellar to the Garden"
    );
    assert!(
        atlas
            .get_path_between_rooms(&key(w.map, 2), &key(secret_area, 1))
            .is_some()
    );
    assert_eq!(
        atlas
            .get_area(&w.map)
            .expect("the manor")
            .source_layers()
            .len(),
        1
    );
    assert!(!secret_marks_on_disk(cache.path(), w.map, &marks).is_empty());

    // The grant goes while the map moves on too, and the refetch the new
    // row calls for fails: the Secret goes anyway, everywhere, at once.
    w.server.revoke_secret(w.map, w.secret, &w.traveler);
    w.server.bump_rev(w.map);
    w.server.fail_next_area_reads(1);
    tick(&mapper).await;
    let gone = |mapper: &Mapper| {
        let atlas = mapper.get_current_atlas();
        assert!(atlas.get_area(&secret_area).is_none());
        assert_eq!(atlas.map_of(&secret_area), None);
        assert_eq!(atlas.get_rooms_by_title("Hidden Library").len(), 0);
        assert!(
            atlas
                .get_path_between_rooms(&key(w.map, 3), &key(w.map, 4))
                .is_none()
        );
        assert!(
            atlas
                .get_path_between_rooms(&key(w.map, 2), &key(secret_area, 1))
                .is_none()
        );
        if let Some(map) = atlas.get_area(&w.map) {
            assert!(map.source_layers().is_empty());
            assert!(map.meta().sources.is_empty());
        }
        assert_eq!(
            secret_marks_on_disk(cache.path(), w.map, &marks),
            Vec::<String>::new()
        );
    };
    gone(&mapper);

    // The next tick brings the map back, without the Secret.
    tick(&mapper).await;
    assert!(mapper.get_current_atlas().get_area(&w.map).is_some());
    gone(&mapper);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn losing_the_map_takes_its_secret_too() {
    let w = World::new("unshared").await;
    let cache = TempCacheDir::new("secret-unshared");
    let mapper = new_synced_mapper(&w.server.base_url, &w.traveler.api_key, cache.path()).await;
    let secret_area = AreaId(w.secret);
    assert!(mapper.get_current_atlas().get_area(&secret_area).is_some());
    w.server
        .state
        .lock()
        .grants
        .retain(|grant| grant.grantee_id != w.traveler.id);
    tick(&mapper).await;
    let atlas = mapper.get_current_atlas();
    assert!(atlas.get_area(&w.map).is_none());
    assert!(atlas.get_area(&secret_area).is_none());
    assert_eq!(atlas.get_rooms_by_title("Hidden Library").len(), 0);
    assert!(cache_files_for_area(cache.path(), w.map).is_empty());
}

/// A door explicitly owned by a Secret stays there and reaches its readers
/// alone. Its destination does not choose its owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_door_into_a_secret_reaches_its_readers_alone() {
    let w = World::new("door").await;
    let owner_cache = TempCacheDir::new("secret-door-owner");
    let traveler_cache = TempCacheDir::new("secret-door-traveler");
    let editor_cache = TempCacheDir::new("secret-door-editor");
    let owner = new_synced_mapper(&w.server.base_url, &w.owner.api_key, owner_cache.path()).await;
    let traveler = new_synced_mapper(
        &w.server.base_url,
        &w.traveler.api_key,
        traveler_cache.path(),
    )
    .await;
    let editor =
        new_synced_mapper(&w.server.base_url, &w.editor.api_key, editor_cache.path()).await;
    let secret_area = AreaId(w.secret);

    // From the Hall into the Hidden Library, stored as Secret-owned data
    // on the Hall. An ordinary create_exit call would instead own it on Map.
    let created = ExitId(Uuid::new_v4());
    owner
        .mutate_batches(vec![
            AreaMutationBatch::strict(
                w.map,
                vec![AreaMutation::CreateExit {
                    room_number: RoomNumber(1),
                    room_source: None,
                    body: ExitArgs {
                        id: Some(created),
                        from_direction: ExitDirection::Down,
                        to_area_id: Some(w.map),
                        to_source: Some(SourceId::Secret(w.secret)),
                        to_room_number: Some(RoomNumber(1)),
                        is_hidden: true,
                        ..ExitArgs::default()
                    },
                }],
                "Secret doorway",
            )
            .in_source(SourceId::Secret(w.secret)),
        ])
        .expect("a Secret-owned door into the Secret");
    let predicted = owner
        .get_current_atlas()
        .get_area(&w.map)
        .expect("the manor")
        .source_layers()[0]
        .anchored_exits(RoomNumber(1))
        .to_vec();
    assert_eq!(predicted.len(), 1, "{created:?}");
    assert!(owner.wait_for_sync_completion(10).await.expect("saved"));
    assert_eq!(w.server.secret_rev(w.map, w.secret), 2);

    tick(&traveler).await;
    tick(&editor).await;
    let seen = traveler
        .get_current_atlas()
        .get_area(&w.map)
        .expect("the manor")
        .source_layers()[0]
        .anchored_exits(RoomNumber(1))
        .to_vec();
    assert_eq!(
        seen.iter()
            .map(|exit| (exit.id, exit.to_area_id, exit.to_room_number))
            .collect::<Vec<_>>(),
        predicted
            .iter()
            .map(|exit| (exit.id, exit.to_area_id, exit.to_room_number))
            .collect::<Vec<_>>()
    );
    assert!(
        traveler
            .get_current_atlas()
            .get_path_between_rooms(
                &RoomKey::new(w.map, RoomNumber(1)),
                &RoomKey::new(secret_area, RoomNumber(1))
            )
            .is_some()
    );
    let editor_atlas = editor.get_current_atlas();
    let manor = editor_atlas.get_area(&w.map).expect("the manor");
    assert!(manor.source_layers().is_empty());
    assert_eq!(
        manor
            .get_room(&RoomNumber(1))
            .expect("the Hall")
            .get_exits()
            .len(),
        1,
        "the Hall shows the editor its map exit alone"
    );
}

/// A write to a Secret the client no longer holds is refused before it is
/// queued, so nothing optimistic brings the Secret back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_to_a_lost_secret_is_refused_and_brings_nothing_back() {
    let w = World::new("lostwrite").await;
    w.server.revoke_secret(w.map, w.secret, &w.traveler);
    w.server
        .grant_secret(w.map, w.secret, &w.owner, &w.traveler, &["add", "edit"]);
    let cache = TempCacheDir::new("secret-lost-write");
    let mapper = new_synced_mapper(&w.server.base_url, &w.traveler.api_key, cache.path()).await;
    assert!(
        mapper
            .get_current_atlas()
            .get_area(&AreaId(w.secret))
            .is_some()
    );
    w.server.revoke_secret(w.map, w.secret, &w.traveler);
    tick(&mapper).await;

    let refused = mapper.mutate_batches(vec![
        AreaMutationBatch::strict(
            w.map,
            vec![AreaMutation::UpsertRoomProperty {
                room_number: RoomNumber(3),
                room_source: None,
                name: "notes".to_string(),
                value: "Still here?".to_string(),
            }],
            "Note",
        )
        .in_source(SourceId::Secret(w.secret)),
    ]);
    assert!(
        matches!(&refused, Err(CloudError::InvalidInput(code)) if code == "secret_unavailable"),
        "{refused:?}"
    );
    let atlas = mapper.get_current_atlas();
    assert!(atlas.get_area(&AreaId(w.secret)).is_none());
    assert!(
        atlas
            .get_area(&w.map)
            .expect("the manor")
            .meta()
            .sources
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// The mock: what the server answers about hidden sources (format-3 §8)
// ---------------------------------------------------------------------------

/// The exit echo in an answer's `data`.
fn echoed_exit(text: &str) -> Value {
    data(text)["data"][0]["exit"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_echoes_withhold_destinations_the_writer_cannot_read() {
    let w = World::new("echoes").await;
    let road = w.server.add_exit(w.map, 3, "East", Some((w.elsewhere, 1)));
    let rev = map_rev(&w.server, &w.editor, w.map).await;
    let path = |exit: Uuid| json!([{ "op": "update_exit", "exit_id": exit, "body": { "path": "squeeze" } }]);

    // The editor edits a map exit into the catacombs, which they cannot read.
    let (status, text) = mutate(
        &w.server,
        &w.editor,
        w.map,
        None,
        rev,
        Uuid::new_v4(),
        path(road),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert!(!text.contains(&w.elsewhere.to_string()), "{text}");
    let echo = echoed_exit(&text);
    assert_eq!(
        [
            &echo["to_area_id"],
            &echo["to_room_number"],
            &echo["to_direction"]
        ],
        [&Value::Null, &Value::Null, &Value::Null]
    );
    let parsed: smudgy_cloud::mutation::MutationResult =
        serde_json::from_value(data(&text)).expect("the client reads a withheld echo");
    assert_eq!(parsed.data.len(), 1);

    // The owner reads the catacombs: the same edit echoes them.
    let (_, text) = mutate(
        &w.server,
        &w.owner,
        w.map,
        None,
        rev + 1,
        Uuid::new_v4(),
        path(road),
    )
    .await;
    assert_eq!(echoed_exit(&text)["to_area_id"], json!(w.elsewhere));

    // A Secret's writer who cannot read where its tunnel leads hears the
    // same of a Secret's exit.
    w.server
        .grant_secret(w.map, w.secret, &w.owner, &w.traveler, &["edit"]);
    let secret = w.secret.to_string();
    let (status, text) = mutate(
        &w.server,
        &w.traveler,
        w.map,
        Some(&secret),
        1,
        Uuid::new_v4(),
        path(w.tunnel),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert!(!text.contains(&w.elsewhere.to_string()), "{text}");
    assert_eq!(echoed_exit(&text)["to_area_id"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hidden_exits_never_refuse_a_merge() {
    let w = World::new("merges").await;
    // Another map's Secret keeps a way into the Garden, and the manor's
    // Secret a tunnel out of it: neither is the editor's to see.
    let crypt = w.server.add_secret(w.elsewhere, "Crypt");
    w.server.add_secret_exit(
        w.elsewhere,
        crypt,
        SecretPlace::Map(1),
        "West",
        Some(SecretPlace::Elsewhere(w.map, 4)),
    );
    let rev = map_rev(&w.server, &w.editor, w.map).await;
    let merge = |remove: i32| json!([{ "op": "assert_merge_safe", "keep_room_number": 1, "remove_room_number": remove }]);
    let (op_map, op_twin) = (Uuid::new_v4(), Uuid::new_v4());
    let on_map = mutate(&w.server, &w.editor, w.map, None, rev, op_map, merge(4)).await;
    let on_twin = mutate(&w.server, &w.editor, w.twin, None, rev, op_twin, merge(4)).await;
    let ids = [
        (w.map.to_string(), "MAP"),
        (w.twin.to_string(), "MAP"),
        (op_map.to_string(), "OP"),
        (op_twin.to_string(), "OP"),
    ];
    assert_eq!(on_map.0, 200, "{}", on_map.1);
    assert_eq!(normalized(&on_map.1, &ids), normalized(&on_twin.1, &ids));

    // The map's own link out of a room still refuses, read or not.
    w.server.add_exit(w.map, 3, "East", Some((w.elsewhere, 1)));
    let (status, text) = mutate(
        &w.server,
        &w.editor,
        w.map,
        None,
        rev,
        Uuid::new_v4(),
        merge(3),
    )
    .await;
    assert_eq!(status, 409, "{text}");
    assert!(text.contains("merge_cross_area_links"), "{text}");
}

/// One creation reusing `id`, of an exit, a new exit's link, or a label.
fn creating(kind: &str, id: Uuid, area: AreaId) -> Value {
    match kind {
        "exit" => {
            let mut body = exit_body(Some(area.0), Some(4));
            body["id"] = json!(id);
            json!([{ "op": "create_exit", "room_number": 3, "body": body }])
        }
        "link" => {
            let mut body = exit_body(Some(area.0), Some(4));
            body["new_connection_id"] = json!(id);
            json!([{ "op": "create_exit", "room_number": 3, "body": body }])
        }
        _ => json!([{ "op": "create_label", "body": {
            "id": id, "x": 1.0, "y": 1.0, "width": 2.0, "height": 1.0,
            "horizontal_alignment": "Center", "vertical_alignment": "Center", "text": "Note"
        } }]),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_ids_preserve_hidden_objects_and_revisions() {
    let w = World::new("ids").await;
    let secret = w.secret.to_string();
    let label = Uuid::new_v4();
    let (status, text) = mutate(
        &w.server,
        &w.owner,
        w.map,
        Some(&secret),
        1,
        Uuid::new_v4(),
        creating("label", label, w.map),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    let secret_link = {
        let st = w.server.state.lock();
        st.areas[&w.map.0].secrets[0]
            .exits
            .iter()
            .find(|exit| exit.id == w.passage)
            .unwrap()
            .connection_id
    };
    let catacomb_exit = w.server.add_exit(w.elsewhere, 1, "Up", None);
    let secret_rev = w.server.secret_rev(w.map, w.secret);
    let catacombs_rev = w.server.area_rev(w.elsewhere);
    for (kind, held) in [
        ("exit", w.passage),
        ("link", secret_link),
        ("label", label),
        ("exit", catacomb_exit),
    ] {
        let (status, text) = mutate(
            &w.server,
            &w.editor,
            w.map,
            None,
            1,
            Uuid::new_v4(),
            creating(kind, held, w.map),
        )
        .await;
        assert_eq!(
            status,
            if kind == "link" { 422 } else { 400 },
            "{kind}: {text}"
        );
        assert!(
            !text.contains(&secret),
            "must not identify the hidden source"
        );
    }
    let st = w.server.state.lock();
    let kept = &st.areas[&w.map.0].secrets[0];
    let passage = kept
        .exits
        .iter()
        .find(|exit| exit.id == w.passage)
        .expect("identity unchanged");
    assert_eq!(passage.connection_id, secret_link);
    assert!(
        kept.connections
            .iter()
            .any(|connection| connection.id == secret_link)
    );
    assert!(kept.labels.iter().any(|item| item.id == label));
    assert!(
        st.areas[&w.elsewhere.0]
            .exits
            .iter()
            .any(|exit| exit.id == catacomb_exit)
    );
    assert_eq!(kept.rev, secret_rev);
    assert_eq!(st.areas[&w.elsewhere.0].rev, catacombs_rev);
    assert_eq!(st.areas[&w.map.0].rev, 1);
}

/// A Secret's exit can't name its own rooms in another map, and the
/// refusal is decided from the request alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_to_source_with_another_maps_id_is_refused_from_the_request_alone() {
    let w = World::new("crossmap").await;
    let secret = w.secret.to_string();
    let mut answers = Vec::new();
    for (user, target) in [
        (&w.owner, w.elsewhere.0),
        (&w.owner, Uuid::new_v4()),
        (&w.traveler, w.elsewhere.0),
        (&w.reader, Uuid::new_v4()),
    ] {
        let mut body = exit_body(Some(target), Some(1));
        body["to_source"] = json!(secret);
        answers.push(
            mutate(
                &w.server,
                user,
                w.map,
                Some(&secret),
                1,
                Uuid::new_v4(),
                json!([{ "op": "create_exit", "room_number": 3, "body": body }]),
            )
            .await,
        );
    }
    let first = answers[0].clone();
    assert_eq!(first.0, 400, "{}", first.1);
    assert!(first.1.contains("to_source"), "{}", first.1);
    assert!(answers.iter().all(|answer| *answer == first), "{answers:?}");
}

/// A move may hand a link back with its ends in the server's order (map
/// rooms first, then by number) and its route reversed with them: the
/// client shows and moves it as served, assuming no order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moved_link_may_come_back_with_its_ends_swapped() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("swapper@example.com", "swapper", true);
    let area = server.create_area(&owner, "Stair");
    server.add_room(area, 2, "Landing");
    server.add_room(area, 3, "Attic");
    server.add_exit(area, 3, "Down", Some((area, 2)));
    server.add_exit(area, 2, "Up", Some((area, 3)));
    // Stored from the attic down, as older maps may hold it: endpoint A is
    // room 3, and the route runs from there.
    let link = {
        let mut st = server.state.lock();
        let connection = &mut st.areas.get_mut(&area.0).expect("the stair").connections[0];
        if connection.endpoint_a.room_number == 2 {
            let b = connection.endpoint_b.as_mut().expect("a two-way link");
            std::mem::swap(&mut connection.endpoint_a, b);
        }
        connection.route_points = vec![(3.0, 1.0), (2.0, 1.0)];
        connection.id
    };
    let cache = TempCacheDir::new("secret-swap");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let secret = mapper
        .create_secret(area, "Loft", None)
        .await
        .expect("create")
        .source;
    let both = || smudgy_cloud::MovedContent {
        rooms: vec![RoomNumber(2), RoomNumber(3)],
        ..smudgy_cloud::MovedContent::default()
    };
    mapper
        .move_content(area, SourceId::map(), secret, both())
        .await
        .expect("into the Secret");

    let atlas = mapper.get_current_atlas();
    let map = atlas.get_area(&area).expect("the stair");
    let layer = &map.source_layers()[0];
    let served = layer
        .content()
        .get_connections()
        .iter()
        .find(|connection| connection.id.0 == link)
        .expect("the link moved with its rooms");
    assert_eq!(
        (
            served.endpoint_a.room_number,
            served.endpoint_b.map(|end| end.room_number)
        ),
        (RoomNumber(2), Some(RoomNumber(3))),
        "the server's order"
    );
    assert_eq!(
        served
            .route_points
            .iter()
            .map(|point| (point.x, point.y))
            .collect::<Vec<_>>(),
        [(2.0, 1.0), (3.0, 1.0)],
        "the route reversed with the ends"
    );
    assert!(
        layer
            .content()
            .get_room_connections()
            .iter()
            .any(|drawn| drawn.connection_id.0 == link),
        "the link is drawn"
    );

    mapper
        .move_content(area, secret, SourceId::map(), both())
        .await
        .expect("and back");
    let atlas = mapper.get_current_atlas();
    let map = atlas.get_area(&area).expect("the stair");
    assert!(
        map.get_connections()
            .iter()
            .any(|connection| connection.id.0 == link)
    );
    assert!(
        map.get_room_connections()
            .iter()
            .any(|drawn| drawn.connection_id.0 == link)
    );
}

// ---------------------------------------------------------------------------
// Rooms that go
// ---------------------------------------------------------------------------

/// A map room as a client shows it: number, title, and where each of its
/// exits leads, by direction.
type SeenRoom = (i32, String, Vec<(String, Option<i32>)>);

fn rooms_seen(mapper: &Mapper, area: AreaId) -> Vec<SeenRoom> {
    let atlas = mapper.get_current_atlas();
    let map = atlas.get_area(&area).expect("the map is loaded");
    let mut rooms: Vec<SeenRoom> = map
        .get_rooms()
        .iter()
        .map(|room| {
            let mut exits: Vec<(String, Option<i32>)> = room
                .get_exits()
                .iter()
                .map(|exit| {
                    (
                        format!("{:?}", exit.from_direction),
                        exit.to_room_number.map(|number| number.0),
                    )
                })
                .collect();
            exits.sort();
            (
                room.get_room_number().0,
                room.get_title().to_string(),
                exits,
            )
        })
        .collect();
    rooms.sort();
    rooms
}

/// Where exit `id` of one of map `map`'s Secrets leads, as the mock stores
/// it, a map room by its own number.
fn stored_secret_exit(
    server: &support::MockHandle,
    map: AreaId,
    id: Uuid,
) -> (Option<Uuid>, Option<i32>) {
    let st = server.state.lock();
    let exit = st.areas[&map.0]
        .secrets
        .iter()
        .flat_map(|secret| secret.exits.iter())
        .find(|exit| exit.id == id)
        .expect("the exit is kept");
    let to_room = exit.to_room_number.map(|key| {
        if exit.to_area_id == Some(map.0) {
            support::state::map_room_of(key).unwrap_or(key)
        } else {
            key
        }
    });
    (exit.to_area_id, to_room)
}

/// The keep: Gate 1 to Vault 6, with a stair from the Tower 5 up into the
/// Vault, and its twin without a Secret. "Bookcase", a Secret of the keep,
/// keeps a hidden door from the Gate down into the Vault. An editor edits
/// both maps and cannot read the Secret.
///
/// The editor deletes the Vault on both. The door loses its destination at
/// the server, though the editor's client never saw it; the map's next room
/// is 6 again, and the new room 6 is no door's destination. The editor sees
/// the keep exactly as the twin, and the owner sees the door leading
/// nowhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_room_takes_a_secrets_door_with_it_and_frees_its_number() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("gone-owner@example.com", "goneo", true);
    let editor = server.create_user("gone-editor@example.com", "gonee", true);
    let map = server.create_area(&owner, "Keep");
    let twin = server.create_area(&owner, "Keep twin");
    for area in [map, twin] {
        for (number, title) in [
            (1, "Gate"),
            (2, "Yard"),
            (3, "Hall"),
            (4, "Stair"),
            (5, "Tower"),
            (6, "Vault"),
        ] {
            server.add_room(area, number, title);
        }
        server.add_exit(area, 5, "Up", Some((area, 6)));
        server.grant(
            &owner,
            &editor,
            GrantScope::Area(area),
            GrantFlags {
                can_edit: true,
                ..GrantFlags::default()
            },
        );
    }
    let secret = server.add_secret(map, "Bookcase");
    let door = server.add_secret_exit(
        map,
        secret,
        SecretPlace::Map(1),
        "Down",
        Some(SecretPlace::Map(6)),
    );
    let cache = TempCacheDir::new("gone-editor");
    let mapper = new_synced_mapper(&server.base_url, &editor.api_key, cache.path()).await;
    wait_until(|| {
        let atlas = mapper.get_current_atlas();
        atlas.get_area(&map).is_some() && atlas.get_area(&twin).is_some()
    })
    .await;
    assert_eq!(
        stored_secret_exit(&server, map, door),
        (Some(map.0), Some(6))
    );

    for area in [map, twin] {
        let deleted = mapper
            .delete_room(RoomKey::new(area, RoomNumber(6)))
            .expect("queued");
        mapper
            .wait_for_mutation(deleted.operation_id().expect("a cloud write"))
            .await
            .expect("the Vault goes");
    }
    assert_eq!(
        stored_secret_exit(&server, map, door),
        (None, None),
        "the door into the Vault leads nowhere"
    );
    assert_eq!(server.secret_rev(map, secret), 2);

    for area in [map, twin] {
        let next = mapper.try_next_room_number(&area).expect("a number");
        assert_eq!(next, RoomNumber(6), "the Vault's number is free");
        let created = mapper
            .create_room(
                RoomKey::new(area, next),
                RoomUpdates {
                    title: Some("Armory".into()),
                    ..RoomUpdates::default()
                },
            )
            .expect("queued");
        mapper
            .wait_for_mutation(created.operation_id().expect("a cloud write"))
            .await
            .expect("the Armory is made");
    }
    assert_eq!(
        stored_secret_exit(&server, map, door),
        (None, None),
        "nothing leads into the new room 6"
    );

    // The editor sees the keep as the twin: the same rooms and exits, no
    // place besides the map, and the same revision.
    tick(&mapper).await;
    assert_eq!(rooms_seen(&mapper, map), rooms_seen(&mapper, twin));
    {
        let atlas = mapper.get_current_atlas();
        let keep = atlas.get_area(&map).expect("the keep");
        let keep_twin = atlas.get_area(&twin).expect("the twin");
        assert!(
            keep.meta()
                .sources
                .iter()
                .all(|bundle| bundle.source.is_map())
        );
        assert!(keep.source_layers().is_empty());
        assert_eq!(keep.get_rev(), keep_twin.get_rev());
    }
    let (_, projection) = get(&server, &editor, &format!("/areas/{map}")).await;
    for mark in [secret.to_string(), "Bookcase".to_string(), door.to_string()] {
        assert!(!projection.contains(&mark), "{mark}: {projection}");
    }

    // The owner reads the door, leading nowhere, and the new room 6.
    let owner_cache = TempCacheDir::new("gone-owner");
    let owner_mapper =
        new_synced_mapper(&server.base_url, &owner.api_key, owner_cache.path()).await;
    wait_until(|| owner_mapper.get_current_atlas().get_area(&map).is_some()).await;
    let atlas = owner_mapper.get_current_atlas();
    let keep = atlas.get_area(&map).expect("the keep");
    let bundle = keep
        .meta()
        .sources
        .iter()
        .find(|bundle| bundle.source == SourceId::Secret(secret))
        .expect("the owner reads the Secret");
    let kept = bundle
        .room_data
        .iter()
        .flat_map(|data| data.exits.iter())
        .find(|exit| exit.id.0 == door)
        .expect("the door is still kept on the Gate");
    assert_eq!((kept.to_area_id, kept.to_room_number), (None, None));
    assert_eq!(
        atlas
            .get_room(&RoomKey::new(map, RoomNumber(6)))
            .expect("room 6")
            .get_title(),
        "Armory"
    );
}

/// The owner moves the keep's Vault into a Secret. Another map's road into
/// the Vault, and a hidden way into it that a Secret of that map keeps, lose
/// their destination, and the road's map moves its revision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_moved_out_of_the_map_keeps_incoming_exits_on_the_same_room() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("moved-out@example.com", "movedout", true);
    let keep = server.create_area(&owner, "Keep");
    let road = server.create_area(&owner, "Road");
    server.add_room(keep, 1, "Gate");
    server.add_room(keep, 6, "Vault");
    server.add_room(road, 1, "Milestone");
    let way = server.add_exit(road, 1, "East", Some((keep, 6)));
    let ditch = server.add_secret(road, "Ditch");
    let hidden = server.add_secret_exit(
        road,
        ditch,
        SecretPlace::Map(1),
        "Down",
        Some(SecretPlace::Elsewhere(keep, 6)),
    );
    let cache = TempCacheDir::new("moved-out");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    wait_until(|| {
        let atlas = mapper.get_current_atlas();
        atlas.get_area(&keep).is_some() && atlas.get_area(&road).is_some()
    })
    .await;
    let road_rev = server.state.lock().areas[&road.0].rev;
    let bookcase = mapper
        .create_secret(keep, "Bookcase", None)
        .await
        .expect("create")
        .source;

    let moved = mapper
        .move_content(
            keep,
            SourceId::map(),
            bookcase,
            smudgy_cloud::MovedContent {
                rooms: vec![RoomNumber(6)],
                ..smudgy_cloud::MovedContent::default()
            },
        )
        .await
        .expect("the Vault moves");
    assert!(!moved.versions.iter().any(|version| version.id == road.0));
    tick(&mapper).await;
    let atlas = mapper.get_current_atlas();
    let room = atlas
        .get_room(&RoomKey::new(road, RoomNumber(1)))
        .expect("Milestone");
    let visible = room
        .get_exits()
        .iter()
        .find(|exit| exit.id.0 == way)
        .expect("the road exit remains");
    let SourceId::Secret(secret_id) = bookcase else {
        panic!("a Secret")
    };
    assert_eq!(visible.to_area_id, Some(AreaId(secret_id)));
    assert_eq!(visible.to_room_number, Some(RoomNumber(6)));
    let source = atlas.get_area(&road).unwrap();
    let layer = source
        .source_layers()
        .iter()
        .find(|layer| layer.source() == SourceId::Secret(ditch))
        .unwrap();
    let attached = layer
        .anchored_exits(RoomNumber(1))
        .iter()
        .find(|exit| exit.id.0 == hidden)
        .unwrap();
    assert_eq!(attached.to_area_id, Some(AreaId(secret_id)));
    assert_eq!(attached.to_room_number, Some(RoomNumber(6)));
    assert_eq!(server.state.lock().areas[&road.0].rev, road_rev);
}

/// Owned rooms of a map's Secret layer, ringed on the canvas. Attachment
/// anchors are stored separately and never appear in this list.
fn layer_own_rooms(mapper: &Mapper, map: AreaId) -> Vec<(i32, String)> {
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&map).expect("the map is loaded");
    let layer = &area.source_layers()[0];
    let mut own: Vec<(i32, String)> = layer
        .content()
        .get_rooms()
        .iter()
        .map(|room| (room.get_room_number().0, room.get_title().to_string()))
        .collect();
    own.sort();
    own
}

/// Linking a Secret's room to a map room, as the Link tool writes it, keeps
/// the map room the map's: only the link and its doors go into the Secret,
/// so the layer's own rooms stay the Secret's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linking_a_secret_room_to_a_map_room_keeps_the_map_room_the_maps() {
    let w = World::new("linkmaproom").await;
    let cache = TempCacheDir::new("secret-link-map-room");
    let owner = new_synced_mapper(&w.server.base_url, &w.owner.api_key, cache.path()).await;
    let secret = SourceId::Secret(w.secret);
    assert_eq!(
        layer_own_rooms(&owner, w.map),
        vec![(1, "Hidden Library".to_string())],
        "before"
    );

    // Hidden Library (Secret 1) ⇄ Cellar (map 3), as commands::create_link
    // writes a link between a Secret's room and a map room.
    let connection_id = smudgy_cloud::ConnectionId(Uuid::new_v4());
    let endpoint = |room: i32, source: Option<SourceId>, direction: ExitDirection| {
        let (side, port_offset) = smudgy_cloud::default_anchor_for_direction(direction, None);
        smudgy_cloud::ConnectionEndpoint {
            source,
            room_number: RoomNumber(room),
            side,
            port_offset,
            port_mode: PortMode::AutoPinned,
        }
    };
    let ops = vec![
        AreaMutation::CreateConnection {
            body: smudgy_cloud::ConnectionArgs {
                id: connection_id,
                endpoint_a: endpoint(1, Some(secret), ExitDirection::East),
                endpoint_b: Some(endpoint(3, None, ExitDirection::West)),
                routing: ConnectionRouting::default(),
                segment_shape: smudgy_cloud::SegmentShape::Direct,
                corner: smudgy_cloud::CornerStyle::Sharp,
                route_points: Vec::new(),
                dash: ConnectionDash::default(),
                color: String::new(),
                thickness: smudgy_cloud::DEFAULT_CONNECTION_THICKNESS,
            },
        },
        AreaMutation::CreateExit {
            room_source: Some(secret),
            room_number: RoomNumber(1),
            body: ExitArgs {
                id: Some(smudgy_cloud::ExitId(Uuid::new_v4())),
                connection_id: Some(connection_id),
                from_direction: ExitDirection::East,
                to_area_id: Some(w.map),
                to_room_number: Some(RoomNumber(3)),
                to_source: None,
                to_direction: Some(ExitDirection::West),
                weight: 1.0,
                ..ExitArgs::default()
            },
        },
        AreaMutation::CreateExit {
            room_source: None,
            room_number: RoomNumber(3),
            body: ExitArgs {
                id: Some(smudgy_cloud::ExitId(Uuid::new_v4())),
                connection_id: Some(connection_id),
                from_direction: ExitDirection::West,
                to_area_id: Some(w.map),
                to_room_number: Some(RoomNumber(1)),
                to_source: Some(secret),
                to_direction: Some(ExitDirection::East),
                weight: 1.0,
                ..ExitArgs::default()
            },
        },
    ];
    let submissions = owner
        .mutate_batches(vec![
            AreaMutationBatch::strict(w.map, ops, "Link").in_source(secret),
        ])
        .expect("queued");
    assert_eq!(
        layer_own_rooms(&owner, w.map),
        vec![(1, "Hidden Library".to_string())],
        "while the link is pending"
    );
    for submission in submissions {
        owner
            .wait_for_mutation(submission.operation_id().expect("an envelope"))
            .await
            .expect("the server takes the link");
    }
    tick(&owner).await;
    assert_eq!(
        layer_own_rooms(&owner, w.map),
        vec![(1, "Hidden Library".to_string())],
        "once the server holds the link"
    );
    let st = w.server.state.lock();
    let held = st.areas[&w.map.0]
        .secrets
        .iter()
        .find(|candidate| candidate.id == w.secret)
        .expect("the Secret");
    assert!(
        held.connections
            .iter()
            .any(|link| link.id == connection_id.0),
        "the Secret keeps the confirmed link"
    );
    assert!(
        st.areas[&w.map.0]
            .connections
            .iter()
            .all(|link| link.id != connection_id.0),
        "the map keeps none of it"
    );
}
