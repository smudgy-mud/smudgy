//! Client HTTP contract for reviewed, source-qualified transfers. These are
//! paired with the real server's transfer-access-review and retained-attachment
//! suites; the mock must not approve a move the server would refuse.
#![allow(clippy::too_many_lines)] // Keep each adversarial scenario and its assertions together.
mod support;
use serde_json::{Value, json};
use smudgy_cloud::AreaId;
use support::{GrantFlags, GrantScope, MockHandle, MockServer, SecretPlace, TestUser};
use uuid::Uuid;

async fn post(
    server: &MockHandle,
    user: &TestUser,
    area: AreaId,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(format!("{}/areas/{area}/moves{path}", server.base_url))
        .bearer_auth(&user.api_key)
        .json(body)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
fn request(server: &MockHandle, area: AreaId, from: Option<Uuid>, to: Option<Uuid>) -> Value {
    let wire = |s: Option<Uuid>| s.map_or_else(|| "map".into(), |s| s.to_string());
    let rev =
        |s: Option<Uuid>| s.map_or_else(|| server.area_rev(area), |s| server.secret_rev(area, s));
    json!({"operation_id":Uuid::new_v4(),"from":wire(from),"to":wire(to),"preconditions":[
        {"resource":"source","id":area,"source":wire(from),"expected_rev":rev(from)},
        {"resource":"source","id":area,"source":wire(to),"expected_rev":rev(to)}]})
}
async fn reviewed(server: &MockHandle, user: &TestUser, area: AreaId, body: &mut Value) -> Value {
    let (status, preview) = post(server, user, area, "/preview", body).await;
    assert_eq!(status, 200, "{preview}");
    body["access_review"] = preview["data"]["token"].clone();
    preview["data"].clone()
}
async fn projection(server: &MockHandle, user: &TestUser, area: AreaId) -> Value {
    reqwest::Client::new()
        .get(format!("{}/areas/{area}", server.base_url))
        .bearer_auth(&user.api_key)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["data"]
        .clone()
}
fn bundle(view: &Value, source: Uuid) -> &Value {
    view["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source"] == json!(source))
        .unwrap()
}

async fn mutate(
    server: &MockHandle,
    user: &TestUser,
    area: AreaId,
    source: &str,
    payload: Value,
) -> (u16, Value) {
    let rev = {
        let st = server.state.lock();
        let map = &st.areas[&area.0];
        support::source_refs::source_for(map, source, user.id)
            .map_or(0, |source| support::reviewed_moves::rev(map, source))
    };
    let response = reqwest::Client::new()
        .post(format!("{}/areas/{area}/mutations", server.base_url))
        .bearer_auth(&user.api_key)
        .json(&json!({"operation_id":Uuid::new_v4(),"source":source,
            "preconditions":[{"resource":"source","id":area,"source":source,"expected_rev":rev}],
            "payload":payload}))
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

#[tokio::test]
async fn cross_map_private_destinations_dont_route_to_ordinary_rooms_with_the_same_room_number() {
    let server = MockServer::spawn().await;
    let alice = server.create_user("alice@example.com", "alice", true);
    let bob = server.create_user("bob@example.com", "bob", true);
    server.befriend(&alice, &bob);
    let origin = server.create_area(&alice, "Origin");
    let target = server.create_area(&alice, "Target");
    server.add_room(origin, 1, "Departure");
    server.add_room(target, 7, "Ordinary seven");
    for area in [origin, target] {
        server.grant(&alice, &bob, GrantScope::Area(area), GrantFlags::default());
    }
    for (author, title) in [
        (&alice, "Alice's private seven"),
        (&bob, "Bob's private seven"),
    ] {
        let (status, result) = mutate(
            &server,
            author,
            target,
            "private",
            json!([
                {"op":"upsert_room","room_source":"private","room_number":7,"body":{"title":title}}
            ]),
        )
        .await;
        assert_eq!(status, 200, "{result}");
    }
    let exit = Uuid::new_v4();
    let (status, result) = mutate(
        &server,
        &alice,
        origin,
        "map",
        json!([
        {"op":"create_exit","room_number":1,"body":{"id":exit,"from_direction":"North","is_hidden":false,"weight":1,
                "to_area_id":target,"to_source":"private","to_room_number":7}}
        ]),
    )
    .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["data"]["data"][0]["exit"]["to_source"], "private");
    let alice_view = projection(&server, &alice, origin).await;
    assert_eq!(
        alice_view["sources"][0]["rooms"][0]["exits"][0]["to_source"],
        "private"
    );
    let bob_view = projection(&server, &bob, origin).await;
    assert_eq!(
        bob_view["sources"][0]["rooms"][0]["exits"][0]["to_unknown"],
        true
    );
    assert!(bob_view["sources"][0]["rooms"][0]["exits"][0]["to_area_id"].is_null());

    let private_rev = server.state.lock().areas[&target.0].private_sources[&alice.id].rev;
    let mut movement = json!({"operation_id":Uuid::new_v4(),"from":"private","to":"map","rooms":[7],
        "preconditions":[{"resource":"source","id":target,"source":"private","expected_rev":private_rev},
            {"resource":"source","id":target,"source":"map","expected_rev":server.area_rev(target)}]});
    reviewed(&server, &alice, target, &mut movement).await;
    let (status, result) = post(&server, &alice, target, "", &movement).await;
    assert_eq!(status, 200, "{result}");
    let bob_view = projection(&server, &bob, origin).await;
    let followed = &bob_view["sources"][0]["rooms"][0]["exits"][0];
    assert_eq!(followed["to_room_number"], 8);
    assert!(followed["to_source"].is_null());
    assert_eq!(followed["to_unknown"], false);
    let target_view = projection(&server, &bob, target).await;
    let private = target_view["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source"] == "private")
        .unwrap();
    assert_eq!(private["rooms"][0]["title"], "Bob's private seven");
}

#[tokio::test]
async fn retained_attachment_visibility_uses_its_existing_grant_and_the_current_anchor() {
    let server = MockServer::spawn().await;
    let alice = server.create_user("alice@example.com", "alice", true);
    let bob = server.create_user("bob@example.com", "bob", true);
    let carol = server.create_user("carol@example.com", "carol", true);
    let area = server.create_area(&alice, "Map");
    server.add_room(area, 7, "Ordinary seven");
    let a = server.add_secret(area, "A");
    let b = server.add_secret(area, "B");
    server.add_secret_room(area, a, 7, "Hidden seven");
    for viewer in [&bob, &carol] {
        server.befriend(&alice, viewer);
        server.grant(
            &alice,
            viewer,
            GrantScope::Area(area),
            GrantFlags {
                can_edit: viewer.id == bob.id,
                ..GrantFlags::default()
            },
        );
    }
    server.grant_secret(area, a, &alice, &bob, &["remove", "copy"]);
    server.grant_secret(area, b, &alice, &carol, &[]);
    {
        let mut st = server.state.lock();
        let map = st.areas.get_mut(&area.0).unwrap();
        let anchor = map.secrets[0].rooms[&7].identity;
        let key =
            support::source_refs::key_for(map, support::source_refs::Source::Secret(1), anchor)
                .unwrap();
        map.secrets[1]
            .rooms
            .get_mut(&key)
            .unwrap()
            .properties
            .insert(
                "note".into(),
                support::state::RoomPropRecord {
                    value: "Already shared with Carol".into(),
                },
            );
    }
    assert!(
        bundle(&projection(&server, &carol, area).await, b)["room_data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let b_rev = server.secret_rev(area, b);
    let mut body = request(&server, area, Some(a), None);
    body["rooms"] = json!([7]);
    let review = reviewed(&server, &bob, area, &mut body).await;
    assert_eq!(review["destination_notice"], true);
    assert!(
        !review.to_string().contains(&b.to_string()),
        "hidden attachment source is not in the review"
    );
    let (status, result) = post(&server, &bob, area, "", &body).await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["data"]["renumbered"], json!([{"from":7,"to":8}]));
    assert_eq!(
        server.secret_rev(area, b),
        b_rev,
        "retained content never changed owner or revision"
    );
    let carol_view = projection(&server, &carol, area).await;
    assert_eq!(bundle(&carol_view, b)["room_data"][0]["room_number"], 8);
    assert_eq!(
        bundle(&carol_view, b)["room_data"][0]["properties"][0]["value"],
        "Already shared with Carol"
    );
    assert!(
        !projection(&server, &bob, area).await["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["source"] == json!(b))
    );
    let mut back = request(&server, area, None, Some(a));
    back["rooms"] = json!([8]);
    reviewed(&server, &alice, area, &mut back).await;
    assert_eq!(post(&server, &alice, area, "", &back).await.0, 200);
    assert!(
        bundle(&projection(&server, &carol, area).await, b)["room_data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let alice_view = projection(&server, &alice, area).await;
    assert_eq!(
        bundle(&alice_view, b)["room_data"][0]["room_source"],
        json!(a)
    );
    assert_eq!(
        alice_view["sources"][0]["rooms"][0]["title"],
        "Ordinary seven"
    );
}

#[tokio::test]
async fn property_choices_are_bound_to_the_review_and_add_does_not_allow_replacement() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let editor = server.create_user("editor@example.com", "editor", true);
    server.befriend(&owner, &editor);
    let area = server.create_area(&owner, "Map");
    server.add_room(area, 4, "Anchor");
    server.grant(
        &owner,
        &editor,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );
    let a = server.add_secret(area, "A");
    let b = server.add_secret(area, "B");
    server.grant_secret(area, a, &owner, &editor, &["remove", "copy"]);
    server.grant_secret(area, b, &owner, &editor, &["add"]);
    for (source, value) in [(a, "incoming"), (b, "keep me")] {
        server.set_secret_room_property(area, source, SecretPlace::Map(4), "note", value);
    }
    let mut body = request(&server, area, Some(a), Some(b));
    body["properties"] = json!([{"room_number":4,"name":"note"}]);
    let review = reviewed(&server, &editor, area, &mut body).await;
    assert_eq!(review["property_conflicts"][0]["can_replace"], false);
    assert_eq!(
        post(&server, &editor, area, "", &body).await.1["details"]["reason"],
        "move_property_conflict"
    );
    body["property_resolutions"] =
        json!([{"property":{"room_number":4,"name":"note"},"keep":"source"}]);
    assert_eq!(post(&server, &editor, area, "/preview", &body).await.0, 404);
    body["property_resolutions"][0]["keep"] = json!("destination");
    assert_eq!(
        post(&server, &editor, area, "", &body).await.1["details"]["reason"],
        "stale_access_review"
    );
    reviewed(&server, &editor, area, &mut body).await;
    let (status, result) = post(&server, &editor, area, "", &body).await;
    assert_eq!(status, 200, "{result}");
    let view = projection(&server, &owner, area).await;
    assert_eq!(
        bundle(&view, b)["room_data"][0]["properties"][0]["value"],
        "keep me"
    );
    assert!(bundle(&view, a)["room_data"].as_array().unwrap().is_empty());
    assert_eq!(view["sources"][0]["rooms"][0]["room_number"], 4);
}

#[tokio::test]
async fn a_review_is_not_authority_and_visible_audience_changes_invalidate_it() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let reader = server.create_user("reader@example.com", "reader", true);
    server.befriend(&owner, &reader);
    let area = server.create_area(&owner, "Map");
    server.add_room(area, 1, "Room");
    let secret = server.add_secret(area, "Secret");
    let mut body = request(&server, area, None, Some(secret));
    body["rooms"] = json!([1]);
    reviewed(&server, &owner, area, &mut body).await;
    server.grant(
        &owner,
        &reader,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );
    assert_eq!(
        post(&server, &owner, area, "", &body).await.1["details"]["reason"],
        "stale_access_review"
    );
    assert_eq!(post(&server, &reader, area, "", &body).await.0, 404);
    reviewed(&server, &owner, area, &mut body).await;
    let (status, first) = post(&server, &owner, area, "", &body).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(
        post(&server, &owner, area, "", &body).await.1,
        first,
        "receipt replay is exact"
    );
}

#[tokio::test]
async fn empty_groups_still_have_different_future_audiences() {
    use smudgy_cloud::{CloudApiClient, Credential, CredentialSource};
    use support::clans::{ClanGrantScope, ClanRecipient};
    let server = MockServer::spawn().await;
    let owner = server.create_user("clan-owner@example.com", "owner", true);
    let bob = server.create_user("clan-bob@example.com", "bob", true);
    let api = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(owner.session_token.clone()))),
    );
    let clan = api.create_clan("Exploration").await.unwrap().id;
    server.join_clan(clan, &bob);
    let explorers = api
        .create_clan_group(clan, "Explorers", None)
        .await
        .unwrap()
        .id;
    let archivists = api
        .create_clan_group(clan, "Archivists", None)
        .await
        .unwrap()
        .id;
    let atlas = server.create_clan_atlas(clan, "Maps");
    let area = server.create_clan_area(clan, atlas, "Map");
    server.clan_grant(
        clan,
        ClanRecipient::Group(server.clan_builtin_group(clan, "members")),
        ClanGrantScope::Areas([area.0].into()),
        &["area.read"],
    );
    let mut secrets = Vec::new();
    for (name, group, bob_right) in [("A", explorers, "remove"), ("B", archivists, "add")] {
        let response: Value = reqwest::Client::new()
            .post(format!("{}/areas/{area}/secrets", server.base_url))
            .bearer_auth(&owner.api_key)
            .json(&json!({"name":name,"ownership":"clan","clan_id":clan}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["success"], true, "{response}");
        let id = Uuid::parse_str(response["data"]["source"].as_str().unwrap()).unwrap();
        {
            let mut st = server.state.lock();
            let record = st
                .areas
                .get_mut(&area.0)
                .unwrap()
                .secrets
                .iter_mut()
                .find(|s| s.id == id)
                .unwrap()
                .clan
                .as_mut()
                .unwrap();
            for (recipient, actions) in [
                (ClanRecipient::Group(group), Vec::new()),
                (ClanRecipient::User(bob.id), vec![bob_right]),
            ] {
                record
                    .grants
                    .push(support::clan_secrets::ClanSecretGrantRecord {
                        id: Uuid::new_v4(),
                        recipient,
                        actions: actions.into_iter().collect(),
                        grantor_id: owner.id,
                        created_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                    });
            }
        }
        secrets.push(id);
    }
    server.add_secret_room(area, secrets[0], 1, "Room");
    let mut body = request(&server, area, Some(secrets[0]), Some(secrets[1]));
    body["rooms"] = json!([1]);
    let (status, denied) = post(&server, &bob, area, "/preview", &body).await;
    assert_eq!(
        status, 404,
        "Remove/Add do not authorize disclosure to future group members: {denied}"
    );
    let preview = reviewed(&server, &owner, area, &mut body).await;
    assert!(
        preview["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["action"] == "read"
                && c["may_gain"].to_string().contains(&archivists.to_string())
                && c["may_lose"].to_string().contains(&explorers.to_string())),
        "{preview}"
    );
    assert!(
        !preview["changes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["action"] == "read")
            .any(|c| c.to_string().contains(&bob.id.to_string())),
        "unchanged user access does not become a roster"
    );
}

#[tokio::test]
async fn map_owned_data_can_be_written_on_a_read_only_secret_room_without_aliasing() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let editor = server.create_user("editor@example.com", "editor", true);
    let area = server.create_area(&owner, "Map");
    server.add_room(area, 1, "Ordinary");
    let secret = server.add_secret(area, "Secret");
    server.add_secret_room(area, secret, 1, "Secret one");
    server.befriend(&owner, &editor);
    server.grant(
        &owner,
        &editor,
        GrantScope::Area(area),
        GrantFlags {
            can_edit: true,
            ..GrantFlags::default()
        },
    );
    server.grant_secret(area, secret, &owner, &editor, &[]);
    let body = json!({"operation_id":Uuid::new_v4(), "preconditions":[{"resource":"source","id":area,"source":"map","expected_rev":server.area_rev(area)}],
        "payload":[{"op":"upsert_room_property","room_source":secret,"room_number":1,"name":"note","value":"Map-owned note"}]});
    let reply: Value = reqwest::Client::new()
        .post(format!("{}/areas/{area}/mutations", server.base_url))
        .bearer_auth(&editor.api_key)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["success"], true, "{reply}");
    assert_eq!(server.secret_rev(area, secret), 1);
    let view = projection(&server, &editor, area).await;
    assert_eq!(
        view["sources"][0]["room_data"][0]["room_source"],
        json!(secret)
    );
    assert!(
        view["sources"][0]["rooms"][0]["properties"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.revoke_secret(area, secret, &editor);
    assert!(
        projection(&server, &editor, area).await["sources"][0]["room_data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut denied = body;
    denied["operation_id"] = json!(Uuid::new_v4());
    denied["preconditions"][0]["expected_rev"] = json!(server.area_rev(area));
    let response = reqwest::Client::new()
        .post(format!("{}/areas/{area}/mutations", server.base_url))
        .bearer_auth(&editor.api_key)
        .json(&denied)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn filing_needs_sharing_authority_even_with_copy_and_rechecks_the_named_group_access() {
    use smudgy_cloud::{
        AtlasId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource,
        MapperBackend,
    };
    use support::clans::{ClanGrantScope, ClanRecipient};
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let bob = server.create_user("bob@example.com", "bob", true);
    let api = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(owner.session_token.clone()))),
    );
    let clan = api.create_clan("Exploration").await.unwrap().id;
    server.join_clan(clan, &bob);
    let explorers = api
        .create_clan_group(clan, "Explorers", None)
        .await
        .unwrap()
        .id;
    let a = server.create_clan_atlas(clan, "A");
    let b = server.create_clan_atlas(clan, "B");
    let area = server.create_clan_area(clan, a, "Map");
    server.clan_grant(
        clan,
        ClanRecipient::User(bob.id),
        ClanGrantScope::Areas([area.0].into()),
        &[
            "area.read",
            "area.refile",
            "secret.read",
            "secret.copy",
            "grant.inspect",
        ],
    );
    server.clan_grant(
        clan,
        ClanRecipient::Group(server.clan_builtin_group(clan, "members")),
        ClanGrantScope::Atlases([b].into()),
        &["atlas.accept_filing"],
    );
    server.clan_grant(
        clan,
        ClanRecipient::Group(explorers),
        ClanGrantScope::Areas([area.0].into()),
        &["area.read"],
    );
    server.clan_grant(
        clan,
        ClanRecipient::Group(explorers),
        ClanGrantScope::Atlases([b].into()),
        &["secret.read"],
    );
    let response: Value = reqwest::Client::new()
        .post(format!("{}/areas/{area}/secrets", server.base_url))
        .bearer_auth(&owner.api_key)
        .json(&json!({"name":"Routes","ownership":"clan","clan_id":clan}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["success"], true, "{response}");
    let maps = CloudMapper::new(server.base_url.clone(), bob.api_key.clone());
    let target = Some(AtlasId(b));
    assert!(
        matches!(
            maps.review_filing(&area, target, 0).await,
            Err(CloudError::NotFoundOrNoAccess)
        ),
        "Copy cannot authorize live Secret access increases"
    );
    assert_eq!(server.state.lock().areas[&area.0].atlas_id, Some(a));
    server.clan_grant(
        clan,
        ClanRecipient::User(bob.id),
        ClanGrantScope::Areas([area.0].into()),
        &["secret.manage_access"],
    );
    let preview = maps.review_filing(&area, target, 0).await.unwrap();
    assert!(preview.destination_notice);
    assert!(
        serde_json::to_value(&preview).unwrap()["changes"]
            .to_string()
            .contains("Explorers")
    );
    server.clan_grant(
        clan,
        ClanRecipient::Group(explorers),
        ClanGrantScope::Atlases([b].into()),
        &["secret.copy"],
    );
    assert!(
        matches!(maps.commit_reviewed_filing(&area, target, &preview.token, 0).await, Err(CloudError::StructuralConflict(reason)) if reason == "stale_access_review")
    );
    assert_eq!(server.state.lock().areas[&area.0].atlas_id, Some(a));
    let refreshed = maps.review_filing(&area, target, 0).await.unwrap();
    maps.commit_reviewed_filing(&area, target, &refreshed.token, 0)
        .await
        .unwrap();
    assert_eq!(server.state.lock().areas[&area.0].atlas_id, Some(b));
}
