//! Exits into maps another Library holds, over real HTTP against the mock
//! (format-3 §4): the writer must read the map there, the exit names one of
//! its map rooms or none, and nothing is created or moved in it. An
//! unreadable map, a room it does not hold, and a map read only through a
//! clan's link all answer the uniform 404.

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{GrantFlags, GrantScope, MockHandle, MockServer, TestUser};
use uuid::Uuid;

async fn exit_into(
    server: &MockHandle,
    writer: &TestUser,
    from: smudgy_cloud::AreaId,
    to: smudgy_cloud::AreaId,
    to_room: Option<i32>,
) -> (StatusCode, Value) {
    let envelope = json!({
        "operation_id": Uuid::new_v4(),
        "preconditions": [{"resource": "source", "id": from, "expected_rev": server.area_rev(from)}],
        "payload": [{
            "op": "create_exit",
            "room_number": 1,
            "body": {
                "from_direction": "North", "to_area_id": to, "to_room_number": to_room,
                "is_hidden": false, "door": null, "weight": 1.0,
            },
        }],
    });
    let response = reqwest::Client::new()
        .post(format!("{}/areas/{from}/mutations", server.base_url))
        .header("authorization", format!("Bearer {}", writer.api_key))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exits_lead_into_rooms_of_maps_their_writer_reads_elsewhere() {
    let server = MockServer::spawn().await;
    let ana = server.create_user("ana@example.com", "ana", true);
    let ben = server.create_user("ben@example.com", "ben", true);
    let mine = server.create_area(&ana, "Home");
    server.add_room(mine, 1, "Door");
    let shared = server.create_area(&ben, "Ben's woods");
    server.add_room(shared, 5, "Clearing");
    server.grant(&ben, &ana, GrantScope::Area(shared), GrantFlags::VIEW_ONLY);
    let hidden = server.create_area(&ben, "Ben's cellar");
    server.add_room(hidden, 5, "Vault");
    let before = server.area_rev(shared);

    let (status, _body) = exit_into(&server, &ana, mine, shared, Some(5)).await;
    assert_eq!(status, StatusCode::OK, "a room of a map Ana reads: {_body}");
    let (status, _) = exit_into(&server, &ana, mine, shared, None).await;
    assert_eq!(status, StatusCode::OK, "no room at all");
    assert_eq!(server.area_rev(shared), before, "nothing moves there");

    // No placeholder: a room the map does not hold is the same 404 as a map
    // Ana cannot read, whatever it holds.
    let (missing_room, missing_body) = exit_into(&server, &ana, mine, shared, Some(9)).await;
    let (unread, unread_body) = exit_into(&server, &ana, mine, hidden, Some(5)).await;
    let (nothing, nothing_body) = exit_into(
        &server,
        &ana,
        mine,
        smudgy_cloud::AreaId(Uuid::new_v4()),
        Some(5),
    )
    .await;
    assert_eq!(missing_room, StatusCode::NOT_FOUND);
    assert_eq!((unread, &unread_body), (missing_room, &missing_body));
    assert_eq!((nothing, &nothing_body), (missing_room, &missing_body));
    assert_eq!(server.area_rev(shared), before);
}
