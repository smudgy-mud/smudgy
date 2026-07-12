//! Smoke tests against the contract-shaped mock server in `tests/support/`.
//!
//! Auth flows use raw `reqwest` (exercising the wire directly); CRUD goes
//! through `CloudMapper`.
#![allow(clippy::too_many_lines, clippy::similar_names)]

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use smudgy_cloud::mapper::RoomKey;
use smudgy_cloud::{
    AreaId, CloudMapper, CreateAreaRequest, MapperBackend, RoomNumber, RoomUpdates,
};
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
    assert!(item.owner_nickname.is_none(), "owned rows carry no owner_nickname");

    // Detail fetch.
    let details = mapper.get_area(&area.id).await.expect("get_area");
    let rev_before = details.area.rev;
    assert_eq!(rev_before, 1);
    assert!(details.rooms.is_empty());
    assert!(
        details.content_hash.is_some(),
        "projection carries a content hash"
    );

    // Room upsert bumps the served rev.
    let room = mapper
        .update_room(
            &RoomKey::new(area.id, RoomNumber(1)),
            RoomUpdates {
                title: Some("Entry Hall".to_string()),
                ..RoomUpdates::default()
            },
        )
        .await
        .expect("update_room");
    assert_eq!(room.title, "Entry Hall");

    let details_after = mapper.get_area(&area.id).await.expect("get_area again");
    let rev_after = details_after.area.rev;
    assert!(
        rev_after > rev_before,
        "rev bumps after a room write ({rev_before} -> {rev_after})"
    );
    assert_eq!(details_after.rooms.len(), 1);
    assert_eq!(details_after.rooms[0].title, "Entry Hall");
    assert_ne!(
        details.content_hash, details_after.content_hash,
        "content hash changes when visible content changes"
    );

    // /sync sees the area with the owner fingerprint.
    let sync = mapper
        .sync_state()
        .await
        .expect("sync_state")
        .expect("mock supports /sync");
    let row = sync
        .iter()
        .find(|r| r.area_id == area.id)
        .expect("sync row for the area");
    assert_eq!(row.rev, details_after.area.rev);
    assert_eq!(row.access_fingerprint.len(), 16);
}

// ---------------------------------------------------------------------------
// 2. Identity flow via raw reqwest
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_flow_signup_verify_me() {
    let server = MockServer::spawn().await;
    let client = reqwest::Client::new();
    let base = &server.base_url;

    // Signup is enumeration-flat 202.
    let response = client
        .post(format!("{base}/auth/signup"))
        .json(&json!({
            "email": "new@example.com",
            "nickname": "newbie",
        }))
        .send()
        .await
        .expect("signup sends");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["data"]["status"], "accepted");

    // The emailed code is exposed through state for tests.
    let code = server
        .verify_code_for("new@example.com")
        .expect("code minted");

    // Verify-email consumes the code and returns a session + the profile.
    let response = client
        .post(format!("{base}/auth/verify-email"))
        .json(&json!({ "email": "new@example.com", "code": code }))
        .send()
        .await
        .expect("verify sends");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json");
    let session = body["data"]["session_token"]
        .as_str()
        .expect("session token");
    assert!(session.starts_with("smudgy_sess_"));
    assert_eq!(body["data"]["user"]["nickname"], "newbie");
    assert!(
        body["data"].get("needs_nickname").is_none(),
        "needs_nickname omitted when false"
    );

    // The code is single-use, and the failure is the uniform 404.
    let response = client
        .post(format!("{base}/auth/verify-email"))
        .json(&json!({ "email": "new@example.com", "code": code }))
        .send()
        .await
        .expect("re-verify sends");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["error"], "Not found");

    // /me over the fresh session: verified, full profile (email included).
    let (status, body) = get_json(&client, &format!("{base}/me"), session).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["email"], "new@example.com");
    assert!(
        !body["data"]["email_verified_at"].is_null(),
        "email_verified_at set after verification"
    );

    // A returning login is the same enumeration-flat 202 — known email...
    let response = client
        .post(format!("{base}/auth/login"))
        .json(&json!({ "email": "new@example.com" }))
        .send()
        .await
        .expect("login sends");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let fresh = server
        .verify_code_for("new@example.com")
        .expect("sign-in code minted");

    // ...or new — a first-time email is provisioned on first sight behind the
    // same enumeration-flat 202, minting a code for the new (nickname-less)
    // account (the unified email-only entry).
    let response = client
        .post(format!("{base}/auth/login"))
        .json(&json!({ "email": "stranger@example.com" }))
        .send()
        .await
        .expect("first-time login sends");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(
        server.verify_code_for("stranger@example.com").is_some(),
        "create-on-absent mints a code for the new account",
    );

    // The fresh code signs the returning user in; the handle is kept.
    let response = client
        .post(format!("{base}/auth/verify-email"))
        .json(&json!({ "email": "new@example.com", "code": fresh }))
        .send()
        .await
        .expect("returning verify sends");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["data"]["user"]["nickname"], "newbie");
}

// ---------------------------------------------------------------------------
// 3. Redaction: secret rooms/exits filtered; hidden targets tokenized
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redaction_hides_secrets_and_tokenizes_hidden_targets() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Shared Area");
    let hidden = server.create_area(&owner, "Hidden Area");
    server.add_room(hidden, 1, "Far Side", false);

    server.add_room(area, 1, "Plaza", false);
    server.add_room(area, 2, "Vault", true); // secret room
    server.add_room(area, 3, "Market", false);
    let e_public = server.add_exit(area, 1, "North", Some((area, 3)), false);
    let e_to_secret = server.add_exit(area, 1, "East", Some((area, 2)), false);
    let e_secret = server.add_exit(area, 3, "South", Some((area, 1)), true);
    let e_cross = server.add_exit(area, 1, "West", Some((hidden, 1)), false);

    server.grant(&owner, &grantee, GrantScope::Area(area), GrantFlags::VIEW_ONLY);

    let client = reqwest::Client::new();
    let base = server.base_url.clone();
    let url = format!("{base}/areas/{area}");

    // --- Grantee view: redacted ---
    let (status, body) = get_json(&client, &url, &grantee.api_key).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["access"]["is_owner"], false);
    let rooms = data["rooms"].as_array().expect("rooms array");
    let room_numbers: Vec<i64> = rooms
        .iter()
        .map(|r| r["room_number"].as_i64().expect("number"))
        .collect();
    assert_eq!(room_numbers, vec![1, 3], "secret room 2 is filtered out");

    let room1 = rooms.iter().find(|r| r["room_number"] == 1).expect("room 1");
    let exit_ids: Vec<&str> = room1["exits"]
        .as_array()
        .expect("exits")
        .iter()
        .map(|e| e["id"].as_str().expect("exit id"))
        .collect();
    assert!(exit_ids.contains(&e_public.to_string().as_str()));
    assert!(
        !exit_ids.contains(&e_to_secret.to_string().as_str()),
        "exit into the secret room is dropped for the grantee"
    );

    let cross = room1["exits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == e_cross.to_string())
        .expect("cross-area exit survives");
    assert_eq!(cross["to_unknown"], true);
    assert!(cross["to_area_id"].is_null(), "hidden target id is nulled");
    assert!(cross["to_room_number"].is_null());
    let token = cross["to_area_token"].as_str().expect("token present");
    assert!(token.starts_with("u_") && token.len() == 18);
    assert_eq!(cross["is_secret"], false);

    let room3 = rooms.iter().find(|r| r["room_number"] == 3).expect("room 3");
    assert!(
        room3["exits"].as_array().expect("exits").is_empty(),
        "secret exit is dropped for the grantee"
    );

    // linked_areas carries only the hidden token entry (same-area links are
    // not foreign).
    let linked = data["linked_areas"].as_array().expect("linked");
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0]["visible"], false);
    assert_eq!(linked[0]["to_area_token"], token);

    // Token is stable across fetches.
    let (_, body_again) = get_json(&client, &url, &grantee.api_key).await;
    let cross_again = body_again["data"]["rooms"][0]["exits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == e_cross.to_string())
        .expect("cross exit again");
    assert_eq!(
        cross_again["to_area_token"], token,
        "to_area_token is stable per (viewer, target)"
    );

    // --- Owner view: everything, real ids ---
    let (status, body) = get_json(&client, &url, &owner.api_key).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["access"]["is_owner"], true);
    let rooms = data["rooms"].as_array().expect("rooms");
    let room_numbers: Vec<i64> = rooms
        .iter()
        .map(|r| r["room_number"].as_i64().expect("number"))
        .collect();
    assert_eq!(room_numbers, vec![1, 2, 3], "owner sees the secret room");

    let room1 = rooms.iter().find(|r| r["room_number"] == 1).expect("room 1");
    let exits1 = room1["exits"].as_array().expect("exits");
    assert!(
        exits1.iter().any(|e| e["id"] == e_to_secret.to_string()),
        "owner sees the exit into the secret room"
    );
    let cross = exits1
        .iter()
        .find(|e| e["id"] == e_cross.to_string())
        .expect("cross exit");
    assert_eq!(cross["to_unknown"], false);
    assert_eq!(
        cross["to_area_id"].as_str().expect("real target id"),
        hidden.to_string(),
        "owner sees the real cross-area target"
    );

    let room3 = rooms.iter().find(|r| r["room_number"] == 3).expect("room 3");
    let secret_exit = room3["exits"]
        .as_array()
        .expect("exits")
        .iter()
        .find(|e| e["id"] == e_secret.to_string())
        .expect("owner sees the secret exit")
        .clone();
    assert_eq!(secret_exit["is_secret"], true);

    // Unshared area is a uniform 404 for the grantee.
    let hidden_url = format!("{base}/areas/{hidden}");
    let (status, body) = get_json(&client, &hidden_url, &grantee.api_key).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Not found");
}

// ---------------------------------------------------------------------------
// 4. Opaque revs + fingerprints: secret-only edits and include_secrets flips
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opaque_rev_and_fingerprint_semantics() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Revved Area");
    let grant_id = server.grant(
        &owner,
        &grantee,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );

    let client = reqwest::Client::new();
    let base = server.base_url.clone();
    let sync_url = format!("{base}/sync");

    let sync_row = |body: &Value, id: AreaId| -> (i64, String) {
        let row = body["data"]
            .as_array()
            .expect("sync rows")
            .iter()
            .find(|r| r["area_id"] == id.to_string())
            .expect("row for area")
            .clone();
        (
            row["rev"].as_i64().expect("rev"),
            row["access_fingerprint"].as_str().expect("fp").to_string(),
        )
    };

    let (_, body) = get_json(&client, &sync_url, &grantee.api_key).await;
    let (grantee_rev0, grantee_fp0) = sync_row(&body, area);
    let (_, body) = get_json(&client, &sync_url, &owner.api_key).await;
    let (owner_rev0, owner_fp0) = sync_row(&body, area);
    assert_eq!(grantee_rev0, owner_rev0, "no secrets yet: revs agree");
    assert_ne!(grantee_fp0, owner_fp0, "caps differ, fingerprints differ");

    // Owner makes a SECRET-only edit through the API (insert a secret room).
    let response = client
        .put(format!("{base}/areas/{area}/10"))
        .header("authorization", format!("Bearer {}", owner.api_key))
        .json(&json!({"title": "Hidden Cellar", "is_secret": true}))
        .send()
        .await
        .expect("secret room upsert");
    assert_eq!(response.status(), StatusCode::OK);

    let (_, body) = get_json(&client, &sync_url, &owner.api_key).await;
    let (owner_rev1, _) = sync_row(&body, area);
    assert!(owner_rev1 > owner_rev0, "owner rev bumps on a secret write");

    let (_, body) = get_json(&client, &sync_url, &grantee.api_key).await;
    let (grantee_rev1, grantee_fp1) = sync_row(&body, area);
    assert_eq!(
        grantee_rev1, grantee_rev0,
        "served public rev does NOT move on a secret-only edit"
    );
    assert_eq!(grantee_fp1, grantee_fp0, "share writes change no fingerprint");

    // Owner raises include_secrets on the grant (root Area-scope: allowed).
    let response = client
        .patch(format!("{base}/shares/{grant_id}"))
        .header("authorization", format!("Bearer {}", owner.api_key))
        .json(&json!({"include_secrets": true}))
        .send()
        .await
        .expect("patch share");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["data"]["include_secrets"], true);

    let (_, body) = get_json(&client, &sync_url, &grantee.api_key).await;
    let (grantee_rev2, grantee_fp2) = sync_row(&body, area);
    assert_ne!(
        grantee_fp2, grantee_fp1,
        "include_secrets flip changes the fingerprint"
    );
    assert_eq!(
        grantee_rev2, owner_rev1,
        "with include_secrets the served rev jumps to the full rev"
    );

    // The cleared grantee now sees the secret room too.
    let (status, body) = get_json(&client, &format!("{base}/areas/{area}"), &grantee.api_key).await;
    assert_eq!(status, StatusCode::OK);
    let rooms = body["data"]["rooms"].as_array().expect("rooms");
    assert!(
        rooms.iter().any(|r| r["room_number"] == 10),
        "include_secrets grantee sees the secret room"
    );
}
