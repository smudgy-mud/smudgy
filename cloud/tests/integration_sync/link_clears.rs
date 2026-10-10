//! A gesture's link clearing never takes an exit into another
//! map's Secret room for an exit into a deleted map room of the same number.
use super::*;
use serde_json::{Value, json};
use smudgy_cloud::mapper::AreaMutationBatch;

async fn http(
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_map_room_keeps_an_exit_into_a_secret_room_of_that_number() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("clears-o@example.com", "clearso", true);
    let road = server.create_area(&owner, "Road");
    let manor = server.create_area(&owner, "Manor");
    for area in [road, manor] {
        server.add_room(area, 1, "One");
        server.add_room(area, 2, "Two");
    }
    let secret = server.add_secret(manor, "Bookcase");
    server.add_secret_room(manor, secret, 1, "Hidden Library");
    server.add_secret_room(manor, secret, 2, "Reading Nook");
    // Road room 1 leads North into the Bookcase's own room 2.
    let foreign = Uuid::new_v4();
    let rev = server.area_rev(road);
    let (status, body) = http(
        reqwest::Method::POST,
        &format!("{}/areas/{road}/mutations", server.base_url),
        &owner.api_key,
        Some(json!({
            "operation_id": Uuid::new_v4(), "source": "map",
            "preconditions": [{ "resource": "source", "id": road, "source": "map", "expected_rev": rev }],
            "payload": [{ "op": "create_exit", "room_number": 1, "body": {
                "id": foreign, "from_direction": "North", "to_area_id": manor, "to_room_number": 2,
                "to_source": secret, "to_direction": null, "is_hidden": false, "weight": 1.0 } }],
        })),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let cache = TempCacheDir::new("link-clears");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    // One gesture: a note on Road room 1, then Manor's MAP room 2 goes.
    let submissions = mapper
        .mutate_batches(vec![
            AreaMutationBatch::strict(
                road,
                vec![AreaMutation::UpsertRoomProperty {
                    room_number: RoomNumber(1),
                    room_source: None,
                    name: "note".to_string(),
                    value: "x".to_string(),
                }],
                "note",
            ),
            AreaMutationBatch::strict(
                manor,
                vec![AreaMutation::DeleteRoom {
                    room_number: RoomNumber(2),
                    room_source: None,
                }],
                "delete map room 2",
            ),
        ])
        .expect("queued");
    for submission in submissions {
        if let Some(id) = submission.operation_id() {
            mapper.wait_for_mutation(id).await.expect("accepted");
        }
    }
    let (_, projection) = http(
        reqwest::Method::GET,
        &format!("{}/areas/{road}", server.base_url),
        &owner.api_key,
        None,
    )
    .await;
    let exits = projection["data"]["sources"][0]["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|room| room["room_number"] == 1)
        .map(|room| room["exits"].clone())
        .unwrap();
    let exit = exits
        .as_array()
        .unwrap()
        .iter()
        .find(|exit| exit["id"] == json!(foreign))
        .cloned();
    eprintln!("Road room 1 exits after the gesture: {exits}");
    let exit = exit.expect("the exit into the Bookcase's room 2 is still there");
    assert_eq!(
        exit["to_source"],
        json!(secret),
        "still leads into the Bookcase: {exit}"
    );
    assert_eq!(exit["to_room_number"], 2, "{exit}");
}

/// Deleting a Secret's own room clears, on screen at once, the exits other
/// maps keep into it: the server dangles them in the deletion's own
/// transaction, and their maps' refetches bring its copies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_secrets_room_clears_other_maps_exits_into_it_on_screen() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("clears-s@example.com", "clearss", true);
    let road = server.create_area(&owner, "Road");
    let manor = server.create_area(&owner, "Manor");
    for area in [road, manor] {
        server.add_room(area, 1, "One");
    }
    let secret = server.add_secret(manor, "Bookcase");
    server.add_secret_room(manor, secret, 1, "Hidden Library");
    server.add_secret_room(manor, secret, 2, "Reading Nook");
    let foreign = Uuid::new_v4();
    let rev = server.area_rev(road);
    let (status, body) = http(
        reqwest::Method::POST,
        &format!("{}/areas/{road}/mutations", server.base_url),
        &owner.api_key,
        Some(json!({
            "operation_id": Uuid::new_v4(), "source": "map",
            "preconditions": [{ "resource": "source", "id": road, "source": "map", "expected_rev": rev }],
            "payload": [{ "op": "create_exit", "room_number": 1, "body": {
                "id": foreign, "from_direction": "North", "to_area_id": manor, "to_room_number": 2,
                "to_source": secret, "to_direction": null, "is_hidden": false, "weight": 1.0 } }],
        })),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let cache = TempCacheDir::new("secret-room-clears");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let exit_into_nook = |mapper: &Mapper| {
        mapper
            .get_current_atlas()
            .get_area(&road)
            .and_then(|area| area.get_room(&RoomNumber(1)).cloned())
            .and_then(|room| {
                room.get_exits()
                    .iter()
                    .find(|exit| exit.id == smudgy_cloud::ExitId(foreign))
                    .map(|exit| (exit.to_area_id, exit.to_room_number))
            })
    };
    assert_eq!(
        exit_into_nook(&mapper),
        Some((Some(AreaId(secret)), Some(RoomNumber(2)))),
        "the Road leads into the Reading Nook"
    );

    let own = smudgy_cloud::SourceId::Secret(secret);
    let submissions = mapper
        .mutate_batches(vec![
            AreaMutationBatch::strict(
                manor,
                vec![AreaMutation::DeleteRoom {
                    room_number: RoomNumber(2),
                    room_source: Some(own),
                }],
                "delete the Reading Nook",
            )
            .in_source(own),
        ])
        .expect("queued");
    assert_eq!(
        exit_into_nook(&mapper),
        Some((None, None)),
        "the exit into the deleted room is cleared on screen"
    );
    for submission in submissions {
        if let Some(id) = submission.operation_id() {
            mapper.wait_for_mutation(id).await.expect("accepted");
        }
    }
}
