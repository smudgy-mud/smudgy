//! The mock server against the server's behaviour, one probe per rule. Each
//! test names the server source it follows (smudgy-cloudflare `src/`).
#![allow(clippy::too_many_lines)]
mod support;

use serde_json::{Value, json};
use smudgy_cloud::clan_secrets::{
    NewSecret, NewSecretOwner, OfferRequest, SecretRecipient, secret_preset,
};
use smudgy_cloud::cloud_api::{CreateShareRequest, ShareScope, TransferDirection};
use smudgy_cloud::{
    AreaId, AtlasId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource,
    MapperBackend,
};
use support::clans::{ClanGrantScope, ClanRecipient};
use support::{GrantFlags, GrantScope, MockHandle, MockServer, TestUser};
use uuid::Uuid;

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

async fn get(server: &MockHandle, user: &TestUser, path: &str) -> (u16, Value) {
    call(
        reqwest::Method::GET,
        &format!("{}{path}", server.base_url),
        &user.api_key,
        None,
    )
    .await
}

async fn mutate(
    server: &MockHandle,
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

fn exit_body(id: Uuid, direction: &str, to: Option<(AreaId, i32)>) -> Value {
    json!({
        "id": id, "from_direction": direction,
        "to_area_id": to.map(|(area, _)| area), "to_room_number": to.map(|(_, room)| room),
        "to_direction": null, "is_hidden": false, "weight": 1.0,
    })
}

fn create_exit(room: i32, body: Value) -> Value {
    let mut op = json!({ "op": "create_exit", "room_number": room });
    op["body"] = body;
    op
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_content_ids_are_library_local_and_refuse_transfer_before_ownership_changes() {
    let server = MockServer::spawn().await;
    let from = server.create_user("uuid-from@example.com", "from", true);
    let to = server.create_user("uuid-to@example.com", "to", true);
    server.befriend(&from, &to);
    let incoming = server.create_area(&from, "Incoming");
    let existing = server.create_area(&to, "Existing");
    let secret = server.add_secret(existing, "Hidden destination");
    let id = Uuid::new_v4();
    let payload = json!([{ "op": "create_shape", "body": {
        "id": id, "x": 0.0, "y": 0.0, "width": 2.0, "height": 1.0,
        "shape_type": "Rectangle"
    }}]);
    for (user, area, source) in [(&from, incoming, None), (&to, existing, Some(secret))] {
        let (status, result) = mutate(&server, user, area, source, payload.clone()).await;
        assert_eq!(status, 200, "{result}");
    }
    let before_incoming = get(&server, &from, &format!("/areas/{incoming}")).await;
    let before_existing = get(&server, &to, &format!("/areas/{existing}")).await;
    let (status, offer) = call(
        reqwest::Method::POST,
        &format!("{}/areas/{incoming}/transfer", server.base_url),
        &from.api_key,
        Some(json!({ "to_user_id": to.id })),
    )
    .await;
    assert_eq!(status, 201, "{offer}");
    let offer_id = offer["data"]["id"].as_str().unwrap();
    let (status, result) = call(
        reqwest::Method::POST,
        &format!("{}/transfers/{offer_id}/accept", server.base_url),
        &to.api_key,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 400, "{result}");
    assert!(!result.to_string().contains(&secret.to_string()));
    assert_eq!(
        get(&server, &from, &format!("/areas/{incoming}")).await,
        before_incoming
    );
    assert_eq!(
        get(&server, &to, &format!("/areas/{existing}")).await,
        before_existing
    );
    let st = server.state.lock();
    assert_eq!(st.areas[&incoming.0].user_id, from.id);
    assert_eq!(
        st.pending_transfers
            .iter()
            .find(|offer| offer.id.to_string() == offer_id)
            .unwrap()
            .status,
        "Offered"
    );
}

/// The exits of `room` in the map bundle of a projection.
fn map_exits(projection: &Value, room: i32) -> Vec<Value> {
    projection["data"]["sources"][0]["rooms"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["room_number"] == json!(room))
        .flat_map(|entry| entry["exits"].as_array().cloned().unwrap_or_default())
        .collect()
}

/// Two maps of one owner, Road and Manor, each with rooms 1 and 2, which a
/// traveler edits; Manor holds the Bookcase Secret with its room 1.
struct Places {
    server: MockHandle,
    owner: TestUser,
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
        let (owner, traveler) = (user("o"), user("t"));
        server.befriend(&owner, &traveler);
        let road = server.create_area(&owner, "Road");
        let manor = server.create_area(&owner, "Manor");
        for area in [road, manor] {
            server.add_room(area, 1, "One");
            server.add_room(area, 2, "Two");
            server.grant(
                &owner,
                &traveler,
                GrantScope::Area(area),
                GrantFlags {
                    can_edit: true,
                    ..GrantFlags::default()
                },
            );
        }
        let secret = server.add_secret(manor, "Bookcase");
        server.add_secret_room(manor, secret, 1, "Hidden Library");
        Self {
            server,
            owner,
            traveler,
            road,
            manor,
            secret,
        }
    }
}

/// A readable source exposes its exit's ID even when the destination is
/// unreadable. That visible identity remains taken and must not be rekeyed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exit_with_an_unreadable_destination_keeps_its_visible_identity() {
    let w = Places::new("ids").await;
    let hidden = Uuid::new_v4();
    let mut body = exit_body(hidden, "North", Some((w.manor, 1)));
    body["to_source"] = json!(w.secret);
    let (status, reply) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        json!([create_exit(1, body)]),
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    // The traveler edits Road but does not read the Bookcase.
    let (status, reply) = mutate(
        &w.server,
        &w.traveler,
        w.road,
        None,
        json!([create_exit(2, exit_body(hidden, "East", Some((w.road, 1))))]),
    )
    .await;
    assert_eq!(
        status, 400,
        "the source-owned exit's visible id remains taken: {reply}"
    );

    // Destination access does not change ownership or identity of the exit.
    let (_, projection) = get(&w.server, &w.owner, &format!("/areas/{}", w.road)).await;
    let north = map_exits(&projection, 1);
    assert_eq!(north.len(), 1, "{projection}");
    assert_eq!(north[0]["to_source"], json!(w.secret));
    assert_eq!(north[0]["id"], json!(hidden));
    assert!(map_exits(&projection, 2).is_empty());
    let (_, projection) = get(&w.server, &w.traveler, &format!("/areas/{}", w.road)).await;
    let north = map_exits(&projection, 1);
    assert_eq!(north[0]["id"], json!(hidden));
    assert_eq!(north[0]["to_unknown"], json!(true));
    assert!(north[0]["to_area_id"].is_null());
    assert!(north[0]["to_room_number"].is_null());
    assert!(north[0]["to_source"].is_null());
}

/// Server (src/library/graph/ids.ts `claimId`): an id the writer is shown
/// in use, including an exit into a Secret room they read, stays taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shown_foreign_exits_id_stays_taken() {
    let w = Places::new("idsshown").await;
    let shown = Uuid::new_v4();
    let mut body = exit_body(shown, "North", Some((w.manor, 1)));
    body["to_source"] = json!(w.secret);
    let (status, reply) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        json!([create_exit(1, body)]),
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    let (status, _) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        json!([create_exit(2, exit_body(shown, "East", Some((w.road, 1))))]),
    )
    .await;
    assert_eq!(status, 400, "the Bookcase's reader sees the id in use");
}

/// Server (src/library/graph/projection.ts `exitView`, run for every
/// readable bundle): `linked_areas` names maps a Secret's exits lead into.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_areas_cover_a_secrets_exits() {
    let w = Places::new("linked").await;
    // The Bookcase keeps an exit on Manor's map room 1 into Road room 2.
    let (status, reply) = mutate(
        &w.server,
        &w.owner,
        w.manor,
        Some(w.secret),
        json!([create_exit(
            1,
            exit_body(Uuid::new_v4(), "West", Some((w.road, 2)))
        )]),
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    let lists_road = |projection: &Value| {
        projection["data"]["linked_areas"]
            .as_array()
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry["to_area_id"] == json!(w.road))
            })
    };
    let (_, projection) = get(&w.server, &w.owner, &format!("/areas/{}", w.manor)).await;
    assert!(lists_road(&projection), "{projection}");
    // A viewer who does not read the Bookcase hears nothing of its exits.
    let (_, projection) = get(&w.server, &w.traveler, &format!("/areas/{}", w.manor)).await;
    assert!(!lists_road(&projection), "{projection}");
}

/// A user with both clients: the session-token API and the map client.
struct Person {
    user: TestUser,
    api: CloudApiClient,
    maps: CloudMapper,
}

fn person(server: &MockHandle, nickname: &str) -> Person {
    let user = server.create_user(&format!("{nickname}@example.com"), nickname, true);
    let api = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    );
    let maps = CloudMapper::new(server.base_url.clone(), user.api_key.clone());
    Person { user, api, maps }
}

/// Server (src/library/secrets/frozen.ts `forgetInClan`, and
/// src/library/clans/membership.ts `depart`): a deleted account departs
/// every clan; its grants on the clan's Secrets go
/// (`DELETE FROM secret_assignments WHERE recipient_user = ?`), and so do
/// the Secret ownership offers it made or is named in (`dropOffersOf`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_an_account_ends_its_clan_secret_grants_and_offers() {
    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let ann = person(&server, "ann");
    let bo = person(&server, "bo");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    for joining in [&ann, &bo] {
        server.join_clan(clan, &joining.user);
    }
    let atlas = server.create_clan_atlas(clan, "Roads");
    let area = server.create_clan_area(clan, atlas, "Midgaard");
    let everyone = server.clan_builtin_group(clan, "members");
    server.clan_grant(
        clan,
        ClanRecipient::Group(everyone),
        ClanGrantScope::Atlases([atlas].into()),
        &["area.read", "secret.create_member_owned"],
    );
    let secret = ann
        .maps
        .create_secret_as(
            &area,
            &NewSecret {
                name: "Quest".to_string(),
                color: None,
                owner: NewSecretOwner::Members { clan_id: clan },
            },
            ann.maps.auth_generation(),
        )
        .await
        .unwrap()
        .source;
    ann.api
        .grant_clan_secret(
            &secret,
            SecretRecipient::User {
                user_id: bo.user.id,
            },
            &secret_preset::READER[1..],
        )
        .await
        .expect("an owner shares with a member");
    ann.api
        .offer_secret_ownership(&secret, &OfferRequest::to_members(vec![bo.user.id], false))
        .await
        .expect("an owner offers ownership to a reader");

    bo.api
        .delete_account()
        .await
        .expect("bo deletes his account");

    let grants = ann.api.clan_secret_grants(&secret).await.unwrap();
    assert!(
        grants
            .iter()
            .all(|grant| grant.recipient.user() != Some(bo.user.id)),
        "no grant names the deleted account: {grants:?}"
    );
    let offers = ann.api.secret_offers(&secret).await.unwrap();
    assert!(
        offers.is_empty(),
        "no offer names the deleted account: {offers:?}"
    );
}

/// Server (src/maps/routes.ts `finishRemoval`, src/control/transfers.ts
/// `cancelOffersOf`): deleting a map or folder cancels its live offers, so
/// the recipient no longer lists them and declining one is the uniform 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_an_offered_map_or_folder_cancels_its_offer() {
    let server = MockServer::spawn().await;
    let ana = person(&server, "ana");
    let ben = person(&server, "ben");
    server.befriend(&ana.user, &ben.user);
    let area = server.create_area(&ana.user, "Heirloom");
    let atlas = AtlasId(server.create_atlas(&ana.user, "Keepsakes"));
    let map_offer = ana
        .api
        .offer_area_transfer(area, ben.user.id)
        .await
        .expect("ana offers her map");
    let folder_offer = ana
        .api
        .offer_atlas_transfer(atlas, ben.user.id)
        .await
        .expect("ana offers her folder");

    ana.maps
        .delete_area(&area)
        .await
        .expect("ana deletes her map");
    ana.maps
        .delete_atlas(&atlas)
        .await
        .expect("ana deletes her folder");

    let received = ben
        .api
        .transfers(TransferDirection::Received)
        .await
        .unwrap();
    assert!(received.is_empty(), "both offers end: {received:?}");
    for offer in [map_offer.id, folder_offer.id] {
        let declined = ben.api.decline_transfer(offer).await;
        assert!(
            matches!(declined, Err(CloudError::NotFoundOrNoAccess)),
            "a cancelled offer is the uniform 404: {declined:?}"
        );
    }
}

/// Server (src/library/maps/moves.ts `exportTransfer`: `subtrees(...
/// grantee_id = to OR can_admin = 1)`, recursive over `parent_id`): a user
/// transfer drops the recipient's own grants with everything re-shared
/// under them. Cy, who reads the map through Ben's re-share, loses it once
/// Ben accepts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transfer_drops_reshares_under_the_recipients_grant() {
    let server = MockServer::spawn().await;
    let ana = person(&server, "ana");
    let ben = person(&server, "ben");
    let cy = person(&server, "cy");
    server.befriend(&ana.user, &ben.user);
    server.befriend(&ben.user, &cy.user);
    let area = server.create_area(&ana.user, "Heirloom");
    server.grant(
        &ana.user,
        &ben.user,
        GrantScope::Area(area),
        GrantFlags {
            can_reshare: true,
            ..GrantFlags::default()
        },
    );
    ben.api
        .create_share(CreateShareRequest {
            grantee_id: cy.user.id,
            scope: ShareScope::Area { area_id: area },
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            can_admin: false,
            host_hints: None,
        })
        .await
        .expect("Ben re-shares with Cy");
    let reads = |rows: Vec<smudgy_cloud::Area>| rows.iter().any(|row| row.id == area);
    assert!(
        reads(cy.maps.list_areas().await.unwrap()),
        "Cy reads it first"
    );

    let offer = ana
        .api
        .offer_area_transfer(area, ben.user.id)
        .await
        .unwrap();
    ben.api
        .accept_transfer(offer.id, None, None)
        .await
        .expect("Ben accepts");

    assert!(
        !reads(cy.maps.list_areas().await.unwrap()),
        "the re-share under Ben's grant ends with it"
    );
    assert!(reads(ben.maps.list_areas().await.unwrap()), "Ben owns it");
}

/// Server (src/library/maps/copies.ts `exportAtlasCopy`): copying an atlas
/// the caller reads none of the maps of, and does not administer, is the
/// uniform 404; nothing is made and nothing of the atlas is revealed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copying_an_unreadable_atlas_is_not_found() {
    let server = MockServer::spawn().await;
    let ana = person(&server, "ana");
    let zed = person(&server, "zed");
    let atlas = server.create_atlas(&ana.user, "Ana's private folder");
    let area = server.create_area_in_atlas(&ana.user, "Hideout", atlas);
    let copied = zed.api.copy_atlas(AtlasId(atlas), None).await;
    assert!(
        matches!(copied, Err(CloudError::NotFoundOrNoAccess)),
        "the uniform 404: {copied:?}"
    );
    assert!(
        zed.maps.list_atlases().await.unwrap().is_empty(),
        "no folder is made for the copier"
    );

    // A reader of one of its maps reaches it.
    server.grant(
        &ana.user,
        &zed.user,
        GrantScope::Area(area),
        GrantFlags::default(),
    );
    let copied = zed.api.copy_atlas(AtlasId(atlas), None).await;
    assert!(copied.is_ok(), "{copied:?}");
}

fn session_api(server: &MockHandle, user: &TestUser) -> CloudApiClient {
    CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    )
}

/// Transfer retains source-owned exits and their room references. Each reader
/// receives destination metadata only for rooms they can currently read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transfer_retains_exits_and_redacts_destinations_the_recipient_does_not_read() {
    let w = Places::new("moveexits").await;
    let closet = w.server.add_secret(w.manor, "Closet");
    w.server.add_secret_room(w.manor, closet, 1, "Coats");
    w.server
        .grant_secret(w.manor, closet, &w.owner, &w.traveler, &[]);
    let into = |secret: Uuid, direction: &str| {
        let mut body = exit_body(Uuid::new_v4(), direction, Some((w.manor, 1)));
        body["to_source"] = json!(secret);
        create_exit(1, body)
    };
    let (status, reply) = mutate(
        &w.server,
        &w.owner,
        w.road,
        None,
        json!([
            into(w.secret, "North"),
            into(closet, "South"),
            create_exit(2, exit_body(Uuid::new_v4(), "East", Some((w.road, 1)))),
        ]),
    )
    .await;
    assert_eq!(status, 200, "{reply}");

    w.server.befriend(&w.owner, &w.traveler);
    let offer = session_api(&w.server, &w.owner)
        .offer_area_transfer(w.road, w.traveler.id)
        .await
        .expect("the owner offers Road");
    session_api(&w.server, &w.traveler)
        .accept_transfer(offer.id, None, None)
        .await
        .expect("the traveler accepts");

    // The former owner reads both Secrets through the retained references.
    let (_, projection) = get(&w.server, &w.owner, &format!("/areas/{}", w.road)).await;
    let exits = map_exits(&projection, 1);
    assert_eq!(exits.len(), 2, "{projection}");
    assert!(exits.iter().any(|exit| exit["to_source"] == json!(closet)));
    assert!(
        exits
            .iter()
            .any(|exit| exit["to_source"] == json!(w.secret))
    );
    assert_eq!(
        projection["data"]["sources"][0]["connections"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let (_, projection) = get(&w.server, &w.traveler, &format!("/areas/{}", w.road)).await;
    let exits = map_exits(&projection, 1);
    assert_eq!(exits.len(), 2, "both exits belong to the received map");
    let hidden = exits
        .iter()
        .find(|exit| exit["from_direction"] == "North")
        .unwrap();
    assert_eq!(hidden["to_unknown"], json!(true));
    assert!(hidden["to_area_id"].is_null());
    assert!(hidden["to_source"].is_null());
    let shown = exits
        .iter()
        .find(|exit| exit["from_direction"] == "South")
        .unwrap();
    assert_eq!(shown["to_source"], json!(closet));
    assert_eq!(
        projection["data"]["sources"][0]["connections"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

/// A move of map rooms `rooms` of `area` into Secret `secret`, at both
/// sources' current revisions.
async fn move_rooms(
    server: &MockHandle,
    user: &TestUser,
    area: AreaId,
    secret: Uuid,
    rooms: &[i32],
) -> (u16, Value) {
    let wire = secret.to_string();
    let mut request = json!({
        "operation_id": Uuid::new_v4(),
        "from": "map",
        "to": wire,
        "preconditions": [
            { "resource": "source", "id": area, "source": "map", "expected_rev": server.area_rev(area) },
            { "resource": "source", "id": area, "source": wire, "expected_rev": server.secret_rev(area, secret) },
        ],
        "rooms": rooms,
    });
    let (status, preview) = call(
        reqwest::Method::POST,
        &format!("{}/areas/{area}/moves/preview", server.base_url),
        &user.api_key,
        Some(request.clone()),
    )
    .await;
    if status != 200 {
        return (status, preview);
    }
    request["access_review"] = preview["data"]["token"].clone();
    call(
        reqwest::Method::POST,
        &format!("{}/areas/{area}/moves", server.base_url),
        &user.api_key,
        Some(request),
    )
    .await
}

/// Server (src/library/graph/moves.ts `move`): a move needs `remove` on the
/// source it takes from and `add` on the one it puts into, whoever owns the
/// map; anything less is the uniform 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_writer_holding_remove_and_add_moves_content() {
    let w = Places::new("moves").await;
    // The traveler edits Manor's map but does not read the Bookcase.
    let (status, reply) = move_rooms(&w.server, &w.traveler, w.manor, w.secret, &[2]).await;
    assert_eq!(status, 404, "{reply}");
    // Reading it is not adding to it.
    w.server
        .grant_secret(w.manor, w.secret, &w.owner, &w.traveler, &[]);
    let (status, reply) = move_rooms(&w.server, &w.traveler, w.manor, w.secret, &[2]).await;
    assert_eq!(status, 404, "{reply}");

    w.server
        .grant_secret(w.manor, w.secret, &w.owner, &w.traveler, &["add"]);
    let (status, reply) = move_rooms(&w.server, &w.traveler, w.manor, w.secret, &[2]).await;
    assert_eq!(status, 200, "{reply}");
    let (_, projection) = get(&w.server, &w.owner, &format!("/areas/{}", w.manor)).await;
    let map_rooms: Vec<Value> = projection["data"]["sources"][0]["rooms"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|room| room["room_number"].clone())
        .collect();
    assert_eq!(
        map_rooms,
        vec![json!(1)],
        "room 2 left the map: {projection}"
    );
}

/// Server (src/identity/routes.ts `/auth/logout`): logging out deletes the
/// session the token names without authenticating the caller, so an
/// account partway through its deletion logs out with a 204 too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logging_out_during_a_deletion_ends_the_session() {
    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    server.interrupt_next_account_deletions(1);
    let interrupted = mira.api.delete_account().await;
    assert!(interrupted.is_err(), "{interrupted:?}");

    mira.api.logout().await.expect("logout is a 204");
    assert!(
        !server
            .state
            .lock()
            .sessions
            .contains_key(&mira.user.session_token),
        "the session is gone"
    );
}

/// Server (format-3.md §4 "Exits into maps held elsewhere"): an
/// `update_exit` judges a destination its body names even when it is the
/// exit's current one, and `to_room_number` alone keeps the exit's map.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_exit_judges_a_named_destination_even_when_unchanged() {
    let server = MockServer::spawn().await;
    let ann = server.create_user("ann@example.com", "ann", true);
    let bo = server.create_user("bo@example.com", "bo", true);
    let road = server.create_area(&ann, "Road");
    server.add_room(road, 1, "Gate");
    let manor = server.create_area(&bo, "Manor");
    server.add_room(manor, 1, "Hall");
    server.add_room(manor, 2, "Cellar");
    let exit = server.add_exit(road, 1, "north", Some((manor, 1)));
    let update = |body: Value| json!([{ "op": "update_exit", "exit_id": exit, "body": body }]);

    // Ann does not read Manor: every room number there is the 404, the
    // exit's current one included.
    for room in [1, 2, 9] {
        let (status, _) = mutate(
            &server,
            &ann,
            road,
            None,
            update(json!({ "to_area_id": manor, "to_room_number": room })),
        )
        .await;
        assert_eq!(status, 404, "room {room}");
    }
    // A change that names no destination is no question about Manor.
    let (status, body) = mutate(&server, &ann, road, None, update(json!({ "path": "n" }))).await;
    assert_eq!(status, 200, "{body}");

    // Once Ann reads Manor, the room number alone keeps the exit's map.
    server.grant(&bo, &ann, GrantScope::Area(manor), GrantFlags::VIEW_ONLY);
    let (status, body) = mutate(
        &server,
        &ann,
        road,
        None,
        update(json!({ "to_room_number": 2 })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let st = server.state.lock();
    let stored = st.areas[&road.0]
        .exits
        .iter()
        .find(|row| row.id == exit)
        .unwrap();
    assert_eq!(
        (stored.to_area_id, stored.to_room_number),
        (Some(manor.0), Some(2))
    );
}

/// Server (architecture.md §8.2, `request-shape-first.test.ts`): a request's
/// body or query is judged before anything it names is looked up, so a
/// malformed one is a 400 whatever it names, and a well-formed one naming
/// something hidden or missing is the uniform 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_requests_shape_is_judged_before_its_subject() {
    let server = MockServer::spawn().await;
    let ann = server.create_user("ann@example.com", "ann", true);
    let bo = server.create_user("bo@example.com", "bo", true);
    let mine = server.create_area(&ann, "Road");
    server.add_room(mine, 1, "Gate");
    let hidden = server.create_area(&bo, "Manor");
    let missing = AreaId(Uuid::new_v4());
    let long_name = "x".repeat(256);
    let secret = Uuid::new_v4();
    // A body's `"{a}"` names the map the path names.
    let naming = |body: &Option<Value>, area: AreaId| {
        body.as_ref().map(|body| {
            serde_json::from_str::<Value>(&body.to_string().replace("{a}", &area.to_string()))
                .expect("a body stays JSON")
        })
    };
    let routes: Vec<(reqwest::Method, &str, Option<Value>, Option<Value>)> = vec![
        (
            reqwest::Method::PUT,
            "/areas/{a}",
            Some(json!({ "name": long_name })),
            Some(json!({ "name": "Fine" })),
        ),
        (
            reqwest::Method::DELETE,
            "/areas/{a}?expected_rev=soon",
            None,
            None,
        ),
        (
            reqwest::Method::POST,
            "/areas/{a}/secrets",
            Some(json!({ "name": "Cache", "ownership": "owner", "clan_id": Uuid::new_v4() })),
            Some(json!({ "name": "Cache" })),
        ),
        (
            reqwest::Method::POST,
            "/areas/{a}/moves",
            Some(
                json!({ "operation_id": Uuid::new_v4(), "from": "attic", "to": "map", "labels": [Uuid::new_v4()] }),
            ),
            Some(
                json!({ "operation_id": Uuid::new_v4(), "from": "map", "to": secret, "labels": [Uuid::new_v4()],
                    "preconditions": [
                        { "resource": "source", "id": "{a}", "source": "map", "expected_rev": 1 },
                        { "resource": "source", "id": "{a}", "source": secret, "expected_rev": 0 },
                    ] }),
            ),
        ),
        (
            reqwest::Method::POST,
            "/areas/{a}/copy",
            Some(json!({ "name": "nul\u{0}" })),
            Some(json!({ "name": "Copy" })),
        ),
    ];
    for (method, template, malformed, well_formed) in routes {
        for area in [mine, hidden, missing] {
            let url = format!(
                "{}{}",
                server.base_url,
                template.replace("{a}", &area.to_string())
            );
            let (status, body) =
                call(method.clone(), &url, &ann.api_key, naming(&malformed, area)).await;
            assert_eq!(status, 400, "{method} {template} on {area}: {body}");
        }
        if well_formed.is_some() {
            for area in [hidden, missing] {
                let url = format!(
                    "{}{}",
                    server.base_url,
                    template.replace("{a}", &area.to_string())
                );
                let (status, body) = call(
                    method.clone(),
                    &url,
                    &ann.api_key,
                    naming(&well_formed, area),
                )
                .await;
                assert_eq!(status, 404, "{method} {template} on {area}: {body}");
            }
        }
    }
    // A Secret grant's body is judged before the Secret is looked up.
    let url = format!("{}/secrets/{}/grants", server.base_url, Uuid::new_v4());
    let (status, _) = call(reqwest::Method::POST, &url, &ann.api_key, Some(json!({}))).await;
    assert_eq!(status, 400);
    let url = format!("{}/areas/{hidden}?expected_rev=3", server.base_url);
    let (status, _) = call(reqwest::Method::DELETE, &url, &ann.api_key, None).await;
    assert_eq!(status, 404, "a well-formed query on a hidden map");
}

/// Server (architecture.md §7.3 `projection_token`): the token covers the
/// maps a projection's exits lead into, readable ones by name and the rest by
/// `to_area_token`, and the atlas's name; nothing done to a map the caller
/// does not read moves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_projection_token_covers_linked_maps_and_the_atlas_name() {
    let server = MockServer::spawn().await;
    let ann = server.create_user("ann@example.com", "ann", true);
    let bo = server.create_user("bo@example.com", "bo", true);
    let shelf = server.create_atlas(&ann, "Shelf");
    let road = server.create_area_in_atlas(&ann, "Road", shelf);
    server.add_room(road, 1, "Gate");
    let manor = server.create_area(&bo, "Manor");
    server.add_room(manor, 1, "Hall");
    server.add_exit(road, 1, "North", Some((manor, 1)));
    let token = || async {
        let (status, body) = get(&server, &ann, &format!("/areas/{road}")).await;
        assert_eq!(status, 200, "{body}");
        body["data"]["projection_token"]
            .as_str()
            .expect("a token")
            .to_string()
    };
    let rename = |area: AreaId, name: &str| {
        server.state.lock().areas.get_mut(&area.0).unwrap().name = name.to_string();
    };

    let unread = token().await;
    rename(manor, "Manor House");
    assert_eq!(
        token().await,
        unread,
        "a map Ann does not read moves nothing"
    );

    let grant = server.grant(&bo, &ann, GrantScope::Area(manor), GrantFlags::VIEW_ONLY);
    let readable = token().await;
    assert_ne!(readable, unread, "the linked map became readable");
    rename(manor, "Old Manor");
    let renamed = token().await;
    assert_ne!(renamed, readable, "a readable linked map was renamed");
    server.state.lock().grants.retain(|row| row.id != grant);
    assert_ne!(token().await, renamed, "the linked map became unreadable");

    let before = token().await;
    server.state.lock().atlases.get_mut(&shelf).unwrap().name = "Bookshelf".to_string();
    assert_ne!(token().await, before, "the atlas was renamed");
}

/// Server (architecture.md §8.2, `maps/envelope.ts`, `clans/routes.ts`,
/// `shape-before-lookup.test.ts`): an envelope's preconditions are part of
/// its shape, one naming the written source of the map in the path (a
/// move's, one naming each of its two sources), so a malformed one is a 400
/// whatever map it names, a receipt's replay included; clan routes judge
/// their body after the credential and before the clan.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preconditions_and_clan_bodies_are_judged_before_their_subject() {
    let server = MockServer::spawn().await;
    let ann = server.create_user("ann@example.com", "ann", true);
    let bo = server.create_user("bo@example.com", "bo", true);
    let mine = server.create_area(&ann, "Road");
    server.add_room(mine, 1, "Gate");
    let hidden = server.create_area(&bo, "Manor");
    let missing = AreaId(Uuid::new_v4());
    let payload = json!([{ "op": "upsert_area_property", "name": "terrain", "value": "road" }]);
    let envelope = |area: AreaId, preconditions: Value| {
        json!({
            "operation_id": Uuid::new_v4(),
            "source": "map",
            "preconditions": preconditions,
            "payload": payload,
        })
        .to_string()
        .replace("{a}", &area.to_string())
    };
    let malformed = [
        json!([]),
        json!([{ "resource": "source", "id": Uuid::new_v4(), "source": "map", "expected_rev": 1 }]),
        json!([{ "resource": "source", "id": "{a}", "source": Uuid::new_v4(), "expected_rev": 1 }]),
        json!([
            { "resource": "source", "id": "{a}", "source": "map", "expected_rev": 1 },
            { "resource": "source", "id": "{a}", "source": "map", "expected_rev": 1 },
        ]),
    ];
    for preconditions in malformed {
        for area in [mine, hidden, missing] {
            let body: Value = serde_json::from_str(&envelope(area, preconditions.clone())).unwrap();
            let url = format!("{}/areas/{area}/mutations", server.base_url);
            let (status, answer) =
                call(reqwest::Method::POST, &url, &ann.api_key, Some(body)).await;
            assert_eq!(status, 400, "{preconditions} on {area}: {answer}");
        }
    }
    for area in [mine, hidden, missing] {
        let url = format!("{}/areas/{area}/moves", server.base_url);
        let body = json!({
            "operation_id": Uuid::new_v4(), "from": "map", "to": Uuid::new_v4(), "rooms": [1],
            "preconditions": [{ "id": area, "source": "map", "expected_rev": 1 }],
        });
        let (status, answer) = call(reqwest::Method::POST, &url, &ann.api_key, Some(body)).await;
        assert_eq!(status, 400, "a move naming one source on {area}: {answer}");
    }

    // A replay sends its preconditions well formed, at any revision.
    let url = format!("{}/areas/{mine}/mutations", server.base_url);
    let operation = Uuid::new_v4();
    let replay = |preconditions: Value| {
        json!({
            "operation_id": operation, "source": "map",
            "preconditions": preconditions, "payload": payload,
        })
    };
    let current = json!([{ "resource": "source", "id": mine, "source": "map",
        "expected_rev": server.area_rev(mine) }]);
    let (status, first) = call(
        reqwest::Method::POST,
        &url,
        &ann.api_key,
        Some(replay(current)),
    )
    .await;
    assert_eq!(status, 200, "{first}");
    let stale = json!([{ "resource": "source", "id": mine, "source": "map", "expected_rev": 0 }]);
    let (status, again) = call(
        reqwest::Method::POST,
        &url,
        &ann.api_key,
        Some(replay(stale)),
    )
    .await;
    assert_eq!((status, &again), (200, &first));
    let (status, _) = call(
        reqwest::Method::POST,
        &url,
        &ann.api_key,
        Some(replay(json!([]))),
    )
    .await;
    assert_eq!(status, 400, "a replay without its precondition");

    // Clan routes: the credential, then the body, then the clan.
    let clan = Uuid::new_v4();
    let clan_routes = [
        (
            reqwest::Method::PATCH,
            format!("/clans/{clan}"),
            json!({ "name": 5 }),
        ),
        (
            reqwest::Method::PATCH,
            format!("/clans/{clan}/members/{}", Uuid::new_v4()),
            json!({ "is_owner": "yes" }),
        ),
        (
            reqwest::Method::POST,
            format!("/clans/{clan}/groups"),
            json!({}),
        ),
    ];
    for (method, path, body) in clan_routes {
        let url = format!("{}{path}", server.base_url);
        let (status, answer) = call(method.clone(), &url, &ann.api_key, Some(body.clone())).await;
        assert_eq!(status, 400, "{method} {path}: {answer}");
        let (status, _) = call(method.clone(), &url, "smudgy_sess_unknown", Some(body)).await;
        assert_eq!(status, 401, "{method} {path} without a credential");
    }
    let url = format!("{}/clans/{clan}/grants?area_id=nowhere", server.base_url);
    let (status, _) = call(reqwest::Method::GET, &url, &ann.api_key, None).await;
    assert_eq!(status, 400, "a malformed filter on an unknown clan");
}

/// Server (architecture.md §8.2, `maps/envelope.ts`): what a source keeps is
/// bounded, and a new exit's destination fields agree, as part of the
/// envelope's shape: a 400 whatever map the envelope names.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_envelopes_field_bounds_are_judged_before_its_map() {
    let server = MockServer::spawn().await;
    let ann = server.create_user("ann@example.com", "ann", true);
    let bo = server.create_user("bo@example.com", "bo", true);
    let mine = server.create_area(&ann, "Road");
    server.add_room(mine, 1, "Gate");
    let hidden = server.create_area(&bo, "Manor");
    let missing = AreaId(Uuid::new_v4());
    let long = "x".repeat(65);
    let payloads = [
        json!([{ "op": "upsert_area_property", "name": long, "value": "v" }]),
        json!([{ "op": "upsert_room_property", "room_number": 1, "name": long, "value": "v" }]),
        json!([{ "op": "add_room_tag", "room_number": 1, "tag": "  " }]),
        json!([{ "op": "add_room_tag", "room_number": 1, "tag": long }]),
        json!([{ "op": "create_exit", "room_number": 1, "body": {
            "from_direction": "North", "to_room_number": 2, "is_hidden": false, "weight": 1.0,
        }}]),
        json!([{ "op": "create_label", "body": {
            "id": Uuid::new_v4(), "level": 0, "x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0,
            "horizontal_alignment": "center", "vertical_alignment": "center", "text": "T",
            "color": long, "background_color": "#000000", "font_size": 12, "font_weight": 400,
        }}]),
    ];
    for payload in payloads {
        for area in [mine, hidden, missing] {
            let body = json!({
                "operation_id": Uuid::new_v4(), "source": "map",
                "preconditions": [{ "resource": "source", "id": area, "source": "map",
                    "expected_rev": 1 }],
                "payload": payload,
            });
            let url = format!("{}/areas/{area}/mutations", server.base_url);
            let (status, answer) =
                call(reqwest::Method::POST, &url, &ann.api_key, Some(body)).await;
            assert_eq!(status, 400, "{payload} on {area}: {answer}");
        }
    }
}

/// Server (src/clans/routes.ts `credentialsFirst`, docs/clans.md): a clan
/// route answers a request without valid credentials 401, whatever its
/// shape. A caller's malformed clan ID or body is the 400, and any other
/// malformed path ID the uniform 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clan_routes_judge_credentials_before_shape() {
    use reqwest::Method;

    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    let requests = [
        (
            Method::GET,
            "/clans/not-a-uuid".to_string(),
            None,
            Some(400),
        ),
        (
            Method::PATCH,
            "/clans/not-a-uuid".to_string(),
            Some(json!({ "name": "x" })),
            Some(400),
        ),
        (
            Method::DELETE,
            format!("/clans/{clan}/members/not-a-uuid"),
            None,
            Some(404),
        ),
        (
            Method::PATCH,
            format!("/clans/{clan}"),
            Some(json!({ "name": 5 })),
            Some(400),
        ),
        (
            Method::DELETE,
            "/clan-invitations/not-a-uuid".to_string(),
            None,
            Some(404),
        ),
        (
            Method::GET,
            "/clans/not-a-uuid/resources".to_string(),
            None,
            None,
        ),
        (Method::GET, format!("/clans/{clan}"), None, Some(200)),
    ];
    let client = reqwest::Client::new();
    for (method, path, body, for_caller) in requests {
        let url = format!("{}{path}", server.base_url);
        let mut bare = client.request(method.clone(), &url);
        if let Some(body) = &body {
            bare = bare.json(body);
        }
        let status = bare.send().await.expect("request sends").status().as_u16();
        assert_eq!(status, 401, "{method} {path} without credentials");
        let (status, _) = call(method.clone(), &url, "smudgy_sess_forged", body.clone()).await;
        assert_eq!(status, 401, "{method} {path} with a forged session");
        if let Some(expected) = for_caller {
            let (status, answer) = call(method.clone(), &url, &mira.user.api_key, body).await;
            assert_eq!(status, expected, "{method} {path} from a caller: {answer}");
        }
    }
}

/// Server (src/library/maps/ownership.ts `removeOwner`, docs/clans.md §2):
/// removing an owner of a Member-owned map answers 409 `clan_dissolving`
/// while its clan dissolves, after the request's own checks, so every owner
/// the copies go to keeps one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dissolving_clans_map_keeps_its_owners() {
    use reqwest::Method;

    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let ann = person(&server, "ann");
    let bo = person(&server, "bo");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    for joining in [&ann, &bo] {
        server.join_clan(clan, &joining.user);
    }
    let atlas = server.create_clan_atlas(clan, "Roads");
    let grove = server.create_member_owned_area(clan, atlas, "Grove", &[ann.user.id, bo.user.id]);
    let owner = |user: Uuid| format!("{}/areas/{grove}/owners/{user}", server.base_url);

    // A dissolution that stops partway leaves the clan dissolving.
    server.interrupt_next_dissolutions(1);
    assert!(mira.api.delete_clan(clan).await.is_err());

    let (status, answer) = call(Method::DELETE, &owner(bo.user.id), &ann.user.api_key, None).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (409, Some("clan_dissolving")),
        "{answer}"
    );
    let (status, answer) = call(Method::DELETE, &owner(ann.user.id), &ann.user.api_key, None).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (409, Some("clan_dissolving")),
        "an owner giving up their own ownership: {answer}"
    );
    // The request's own checks come first.
    let (status, _) = call(
        Method::DELETE,
        &owner(mira.user.id),
        &ann.user.api_key,
        None,
    )
    .await;
    assert_eq!(status, 404, "a user who owns nothing");
    let (status, _) = call(Method::DELETE, &owner(bo.user.id), &mira.user.api_key, None).await;
    assert_eq!(status, 404, "a caller who owns nothing");
    assert_eq!(
        server.map_owners(grove),
        Some(vec![ann.user.id, bo.user.id])
    );
}

/// Server (src/maps/ownership.ts `offerRecipients`, docs/clans.md): an
/// ownership offer names at most 16 users, judged with the body's shape: a
/// 400 whatever map the offer names.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ownership_offer_names_at_most_sixteen_users() {
    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let ann = person(&server, "ann");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    server.join_clan(clan, &ann.user);
    let atlas = server.create_clan_atlas(clan, "Roads");
    let grove = server.create_member_owned_area(clan, atlas, "Grove", &[ann.user.id]);
    let missing = AreaId(Uuid::new_v4());
    let users: Vec<Uuid> = (0..17).map(|_| Uuid::new_v4()).collect();
    for area in [grove, missing] {
        let url = format!("{}/areas/{area}/ownership-offers", server.base_url);
        let body = json!({ "user_ids": users, "ownership": "members" });
        let (status, answer) =
            call(reqwest::Method::POST, &url, &ann.user.api_key, Some(body)).await;
        assert_eq!(status, 400, "an offer to 17 users on {area}: {answer}");
    }
}
