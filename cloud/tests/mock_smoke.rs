//! Smoke tests against the contract-shaped mock server in `tests/support/`.
//!
//! Auth flows use raw `reqwest` (exercising the wire directly); CRUD goes
//! through `CloudMapper`.
#![allow(clippy::too_many_lines, clippy::similar_names)]

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use smudgy_cloud::mutation::{AreaMutation, MutationEnvelope, OpResult, Precondition};
use smudgy_cloud::{CloudMapper, CreateAreaRequest, MapperBackend, RoomNumber, RoomUpdates, Uuid};
use std::collections::BTreeMap;
use support::{GrantFlags, GrantScope, MockServer};

/// GET `url` with a bearer credential; returns (status, parsed body).
async fn get_json(client: &reqwest::Client, url: &str, token: &str) -> (StatusCode, Value) {
    let response = client
        .get(url)
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .expect("request sends");
    let status = response.status();
    let body: Value = response.json().await.expect("json body");
    (status, body)
}

// ---------------------------------------------------------------------------
// 1. CloudMapper CRUD round-trip
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_mapper_crud_roundtrip() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let mapper = CloudMapper::new(server.base_url.clone(), owner.api_key.clone());

    // Create.
    let area = mapper
        .create_area(CreateAreaRequest {
            name: "Test Area".to_string(),
            atlas_id: None,
            clan_id: None,
            ownership: None,
            ephemeral: false,
            properties: BTreeMap::new(),
        })
        .await
        .expect("create_area");
    assert_eq!(area.name, "Test Area");
    assert_eq!(area.rev, 1);

    // List: the access block is present and owned.
    let listed = mapper.list_areas().await.expect("list_areas");
    let item = listed
        .iter()
        .find(|a| a.id == area.id)
        .expect("created area listed");
    let access = item.access.expect("access block present on list rows");
    assert!(access.is_owner, "creator owns the area");
    assert!(access.can_edit && access.can_copy && access.include_secrets);
    assert!(
        item.owner_nickname.is_none(),
        "owned rows carry no owner_nickname"
    );

    // Detail fetch.
    let details = mapper.get_area(&area.id).await.expect("get_area");
    let rev_before = details.area.rev;
    assert_eq!(rev_before, 1);
    assert!(details.rooms.is_empty());
    assert!(
        details.area.projection_token.is_some(),
        "projection carries a token"
    );

    // A room upsert rides a mutation envelope and bumps the served rev. The
    // area precondition REQUIRES the access fingerprint (bare revisions are
    // ambiguous across projection classes).
    let result = mapper
        .execute_mutation(
            &area.id,
            &MutationEnvelope {
                source: smudgy_cloud::SourceId::map(),
                operation_id: Uuid::new_v4(),
                preconditions: vec![Precondition::source(
                    area.id.0,
                    smudgy_cloud::SourceId::map(),
                    rev_before,
                )],
                payload: vec![AreaMutation::UpsertRoom {
                    room_source: None,
                    room_number: RoomNumber(1),
                    body: RoomUpdates {
                        title: Some("Entry Hall".to_string()),
                        ..RoomUpdates::default()
                    },
                }],
            },
        )
        .await
        .expect("compound mutation");
    assert!(
        matches!(&result.data[0], OpResult::Room { room } if room.title == "Entry Hall"),
        "the envelope echoes the stored room"
    );
    assert_eq!(result.versions.len(), 1);
    assert_eq!(result.versions[0].rev, rev_before + 1);

    let details_after = mapper.get_area(&area.id).await.expect("get_area again");
    let rev_after = details_after.area.rev;
    assert!(
        rev_after > rev_before,
        "rev bumps after a room write ({rev_before} -> {rev_after})"
    );
    assert_eq!(details_after.rooms.len(), 1);
    assert_eq!(details_after.rooms[0].title, "Entry Hall");
    assert_ne!(
        details.area.projection_token, details_after.area.projection_token,
        "the token changes when visible content changes"
    );

    // /sync carries the same token and the map's revision.
    let sync = mapper
        .sync_state()
        .await
        .expect("sync_state")
        .expect("mock supports /sync");
    let row = sync
        .iter()
        .find(|r| r.area_id == area.id)
        .expect("sync row for the area");
    assert_eq!(row.map_rev(), Some(details_after.area.rev));
    assert_eq!(
        Some(&row.projection_token),
        details_after.area.projection_token.as_ref()
    );
}

// ---------------------------------------------------------------------------
// 2. Format 3: the map bundle, and targets the viewer cannot read
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn projection_bundles_the_map_and_tokenizes_hidden_targets() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Shared Area");
    let hidden = server.create_area(&owner, "Hidden Area");
    server.add_room(hidden, 1, "Far Side");
    server.add_room(area, 1, "Plaza");
    server.add_room(area, 3, "Market");
    let e_public = server.add_exit(area, 1, "North", Some((area, 3)));
    let e_cross = server.add_exit(area, 1, "West", Some((hidden, 1)));
    server.grant(
        &owner,
        &grantee,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );

    let client = reqwest::Client::new();
    let base = server.base_url.clone();
    let url = format!("{base}/areas/{area}");
    let (status, body) = get_json(&client, &url, &grantee.api_key).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["format_version"], 3);
    assert!(
        data["projection_token"]
            .as_str()
            .is_some_and(|t| t.starts_with("p_"))
    );
    assert!(data.get("rev").is_none() && data.get("rooms").is_none());
    let sources = data["sources"].as_array().expect("sources");
    assert_eq!(sources.len(), 1, "the mock serves the map source only");
    let map = &sources[0];
    assert_eq!(map["source"], "map");
    assert_eq!(map["actions"], json!(["read"]), "a view-only grantee reads");

    let room1 = map["rooms"]
        .as_array()
        .expect("rooms")
        .iter()
        .find(|r| r["room_number"] == 1)
        .expect("room 1");
    let exits = room1["exits"].as_array().expect("exits");
    assert!(exits.iter().any(|e| e["id"] == e_public.to_string()));
    let cross = exits
        .iter()
        .find(|e| e["id"] == e_cross.to_string())
        .expect("cross-area exit survives");
    assert_eq!(cross["to_unknown"], true);
    assert!(cross["to_area_id"].is_null(), "hidden target id is nulled");
    assert!(cross["to_room_number"].is_null());
    let token = cross["to_area_token"].as_str().expect("token present");
    assert!(token.starts_with("u_") && token.len() == 18);

    // Unshared area is a uniform 404 for the grantee.
    let hidden_url = format!("{base}/areas/{hidden}");
    let (status, body) = get_json(&client, &hidden_url, &grantee.api_key).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Not found");
}

// ---------------------------------------------------------------------------
// 3. Format 3: write bodies carrying `is_secret` are refused
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_bodies_carrying_is_secret_are_refused() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let area = server.create_area(&owner, "Flagless");
    server.add_room(area, 1, "Hall");
    let label = server.add_label(area, "Sign");
    let shape = server.add_shape(area);
    let rev = server.area_rev(area);

    let base = server.base_url.clone();
    let envelope = json!({
        "operation_id": Uuid::new_v4(),
        "preconditions": [{"resource": "source", "id": area, "expected_rev": rev}],
        "payload": [{
            "op": "upsert_room",
            "room_number": 1,
            "body": {"title": "Vault", "is_secret": true},
        }],
    });
    let writes = [
        (
            reqwest::Method::POST,
            format!("{base}/areas/{area}/mutations"),
            envelope,
        ),
        (
            reqwest::Method::PUT,
            format!("{base}/areas/{area}/1"),
            json!({"title": "Vault", "is_secret": true}),
        ),
        (
            reqwest::Method::PUT,
            format!("{base}/areas/{area}/properties/theme"),
            json!({"value": "dark", "is_secret": false}),
        ),
        (
            reqwest::Method::PUT,
            format!("{base}/areas/{area}/rooms/1/properties/note"),
            json!({"value": "hidden", "is_secret": true}),
        ),
        (
            reqwest::Method::POST,
            format!("{base}/areas/{area}/labels"),
            json!({
                "x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0,
                "horizontal_alignment": "Center", "vertical_alignment": "Center",
                "text": "Psst", "is_secret": true,
            }),
        ),
        (
            reqwest::Method::PUT,
            format!("{base}/areas/{area}/labels/{label}"),
            json!({"text": "Psst", "is_secret": true}),
        ),
        (
            reqwest::Method::POST,
            format!("{base}/areas/{area}/shapes"),
            json!({
                "x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0,
                "shape_type": "Rectangle", "is_secret": true,
            }),
        ),
        (
            reqwest::Method::PUT,
            format!("{base}/areas/{area}/shapes/{shape}"),
            json!({"radius": 2.0, "is_secret": true}),
        ),
    ];

    let client = reqwest::Client::new();
    for (method, url, body) in writes {
        let response = client
            .request(method, &url)
            .header("authorization", format!("Bearer {}", owner.api_key))
            .json(&body)
            .send()
            .await
            .expect("request sends");
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{url} refuses `is_secret`"
        );
        let body: Value = response.json().await.expect("json body");
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|error| error.contains("`is_secret` is not part of format 3")),
            "{url} names the refused key: {body}"
        );
    }
    assert_eq!(server.area_rev(area), rev, "no refused write applied");
}
