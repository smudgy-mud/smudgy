//! Transfers and copies end to end over real HTTP against the
//! contract-shaped mock in `tests/support/{transfers,clone}.rs`: a map to a
//! friend, a map or atlas into a clan (Clan-owned, or Member-owned when it
//! "stays mine"), the refusals, acceptances stopped before the flip,
//! receipts that move with the map, and copies carrying exactly the Secrets
//! their copier holds `copy` on (format-3 §5.2–§5.4).
#![allow(clippy::too_many_lines)]

mod support;

use std::collections::BTreeSet;

use smudgy_cloud::clan_maps::MapOwnership;
use smudgy_cloud::clans::action;
use smudgy_cloud::cloud_api::{
    CopyAreaRequest, SecretGrantChange, TransferDirection, TransferRecipient, TransferView,
};
use smudgy_cloud::{
    Area, AreaId, AtlasId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource,
    MapperBackend, SourceId,
};
use support::clans::{ClanGrantScope, ClanRecipient};
use support::{GrantFlags, GrantScope, MockHandle, MockServer, SecretPlace, TestUser};
use uuid::Uuid;

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

fn is_not_found<T: std::fmt::Debug>(result: &Result<T, CloudError>) -> bool {
    matches!(result, Err(CloudError::NotFoundOrNoAccess))
}

async fn row(person: &Person, area: AreaId) -> Option<Area> {
    person
        .maps
        .list_areas()
        .await
        .expect("list maps")
        .into_iter()
        .find(|row| row.id == area)
}

/// The names of the Secrets `person` reads on `area`, with their ownership.
async fn secrets(person: &Person, area: AreaId) -> Vec<(String, String, Option<Uuid>)> {
    person
        .api
        .area_secrets(area)
        .await
        .expect("list Secrets")
        .into_iter()
        .map(|secret| (secret.name, secret.ownership, secret.clan_id))
        .collect()
}

fn actions(list: &std::collections::BTreeSet<String>) -> Vec<&str> {
    list.iter().map(String::as_str).collect()
}

/// A map with one room and a Secret holding a room of its own.
fn map_with_secret(
    server: &MockHandle,
    owner: &TestUser,
    name: &str,
    secret: &str,
) -> (AreaId, Uuid) {
    let area = server.create_area(owner, name);
    server.add_room(area, 1, "Gate");
    let secret = server.add_secret(area, secret);
    server.add_secret_room(area, secret, 7, "Hidden cellar");
    (area, secret)
}

/// A clan of `owner`'s with one folder, `members` joined, and Read for
/// every member on that folder.
async fn clan_with_folder(
    server: &MockHandle,
    owner: &Person,
    members: &[&Person],
) -> (Uuid, AtlasId) {
    let clan = owner.api.create_clan("Lantern Company").await.unwrap().id;
    for member in members {
        server.join_clan(clan, &member.user);
    }
    let folder = owner.api.create_clan_atlas(clan, "Roads").await.unwrap().id;
    let everyone = server.clan_builtin_group(clan, "members");
    server.clan_grant(
        clan,
        ClanRecipient::Group(everyone),
        ClanGrantScope::Atlases([folder.0].into()),
        &["area.read"],
    );
    (clan, folder)
}

fn offer_names_clan(offer: &TransferView, clan: Uuid) -> bool {
    offer.to_clan_id == Some(clan)
        && offer.to_clan_name.as_deref() == Some("Lantern Company")
        && offer.to_user_id.is_none()
        && offer.to_nickname.is_none()
}

// ---------------------------------------------------------------------------
// To a friend
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_map_given_to_a_friend_brings_its_secrets_and_shares_back() {
    let server = MockServer::spawn().await;
    let ana = person(&server, "ana");
    let ben = person(&server, "ben");
    let cy = person(&server, "cy");
    server.befriend(&ana.user, &ben.user);
    server.befriend(&ana.user, &cy.user);
    let (area, secret) = map_with_secret(&server, &ana.user, "Heirloom", "Bookcase");
    // Ben's own grant on the Secret goes when he owns it; Cy's stays.
    server.grant_secret(area, secret, &ana.user, &ben.user, &[]);
    server.grant_secret(area, secret, &ana.user, &cy.user, &["edit"]);
    server.grant(
        &ana.user,
        &cy.user,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );
    let shelf = ben.maps.create_atlas("Shelf").await.unwrap().id;

    let offer = ana
        .api
        .offer_area_transfer(area, ben.user.id)
        .await
        .unwrap();
    assert_eq!(offer.to_user_id, Some(ben.user.id));
    assert_eq!(offer.to_nickname.as_deref(), Some("ben"));
    assert!(offer.to_clan_id.is_none() && offer.to_clan_name.is_none());
    assert_eq!(offer.subject_name.as_deref(), Some("Heirloom"));
    // One live offer at a time, and a second says so.
    assert!(
        matches!(
            ana.api.offer_area_transfer(area, cy.user.id).await,
            Err(CloudError::TransferAlreadyPending)
        ),
        "a second offer for the same map"
    );
    let received = ben
        .api
        .transfers(TransferDirection::Received)
        .await
        .unwrap();
    assert_eq!(
        received.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![offer.id]
    );
    let offered = ana.api.transfers(TransferDirection::Offered).await.unwrap();
    assert_eq!(
        offered.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![offer.id]
    );

    // An atlas that is not the recipient's own is refused, and the offer stands.
    let elsewhere = ana.maps.create_atlas("Not Ben's").await.unwrap().id;
    assert!(is_not_found(
        &ben.api
            .accept_transfer(offer.id, None, Some(elsewhere))
            .await
    ));
    let accepted = ben
        .api
        .accept_transfer(offer.id, Some("Ben's Heirloom".to_string()), Some(shelf))
        .await
        .unwrap();
    assert_eq!(accepted.status, "Accepted");
    assert_eq!(accepted.subject_name.as_deref(), Some("Ben's Heirloom"));

    let mine = row(&ben, area).await.expect("Ben lists the map");
    assert_eq!(mine.user_id, Some(ben.user.id));
    assert_eq!(mine.atlas_id, Some(shelf));
    assert!(mine.access.unwrap().is_owner);
    let theirs = row(&ana, area)
        .await
        .expect("the share-back keeps it listed");
    let access = theirs.access.unwrap();
    assert!(!access.is_owner && access.can_admin);

    // The Secret is Ben's own now; Ana keeps every action on it, copying
    // included.
    assert_eq!(
        secrets(&ben, area).await,
        vec![("Bookcase".to_string(), "owner".to_string(), None)]
    );
    let ana_view = ana.api.area_secrets(area).await.unwrap();
    assert_eq!(
        actions(&ana_view[0].actions),
        vec!["add", "copy", "edit", "manage_access", "read", "remove"]
    );
    let grants = ben
        .api
        .secret_grants(&SourceId::Secret(secret))
        .await
        .unwrap();
    let mut grantees: Vec<Uuid> = grants.iter().map(|grant| grant.grantee_id).collect();
    grantees.sort();
    let mut expected = vec![ana.user.id, cy.user.id];
    expected.sort();
    assert_eq!(grantees, expected, "Ben's own grant went; Cy's moved");
    assert!(
        ben.api
            .transfers(TransferDirection::Received)
            .await
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Into a clan
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_clan_transfers_require_the_destination_and_replay_without_an_offer() {
    let server = MockServer::spawn().await;
    let owner = person(&server, "owner");
    let member = person(&server, "member");
    let (clan, folder) = clan_with_folder(&server, &owner, &[&member]).await;
    let area = server.create_area(&member.user, "My map");
    let operation = Uuid::new_v4();
    assert!(is_not_found(
        &member
            .api
            .transfer_area_to_clan(area, clan, MapOwnership::Members, folder, operation)
            .await
    ));
    assert!(
        member
            .api
            .transfers(TransferDirection::Offered)
            .await
            .unwrap()
            .is_empty()
    );
    server.clan_grant(
        clan,
        ClanRecipient::Group(server.clan_builtin_group(clan, "members")),
        ClanGrantScope::Atlases([folder.0].into()),
        &["atlas.accept_transfer"],
    );
    let elsewhere = owner
        .api
        .create_clan_atlas(clan, "Elsewhere")
        .await
        .unwrap()
        .id;
    assert!(is_not_found(
        &member
            .api
            .transfer_area_to_clan(area, clan, MapOwnership::Members, elsewhere, operation)
            .await
    ));
    server.interrupt_next_acceptance(|_, _| true);
    assert!(
        member
            .api
            .transfer_area_to_clan(area, clan, MapOwnership::Members, folder, operation)
            .await
            .is_err()
    );
    assert_eq!(
        row(&member, area).await.unwrap().user_id,
        Some(member.user.id)
    );
    assert!(
        member
            .api
            .transfers(TransferDirection::Offered)
            .await
            .unwrap()
            .is_empty()
    );
    let moved = member
        .api
        .transfer_area_to_clan(area, clan, MapOwnership::Members, folder, operation)
        .await
        .unwrap();
    assert_eq!(moved.id, operation);
    assert_eq!(moved.status, "Accepted");
    assert_eq!(row(&member, area).await.unwrap().clan_id, Some(clan));
    let repeated = member
        .api
        .transfer_area_to_clan(area, clan, MapOwnership::Members, folder, operation)
        .await
        .unwrap();
    assert_eq!(repeated.id, operation);
    assert!(owner.api.clan_transfers(clan).await.unwrap().is_empty());
    assert!(matches!(
        member
            .api
            .transfer_area_to_clan(area, clan, MapOwnership::Clan, folder, operation)
            .await,
        Err(CloudError::InvalidInput(_))
    ));
    assert!(
        !server
            .state
            .lock()
            .http_requests
            .iter()
            .any(|(_, path)| path.ends_with("/accept"))
    );
    let atlas = server.create_atlas(&owner.user, "My atlas");
    let child = server.create_area_in_atlas(&owner.user, "A child", atlas);
    assert!(is_not_found(
        &member
            .api
            .transfer_atlas_to_clan(AtlasId(atlas), clan, MapOwnership::Clan, Uuid::new_v4())
            .await
    ));
    owner
        .api
        .transfer_atlas_to_clan(AtlasId(atlas), clan, MapOwnership::Clan, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(row(&owner, child).await.unwrap().clan_id, Some(clan));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_map_offered_to_a_clan_is_accepted_into_a_folder() {
    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let tomas = person(&server, "tomas");
    let kai = person(&server, "kai");
    let outsider = person(&server, "outsider");
    let (clan, roads) = clan_with_folder(&server, &mira, &[&tomas, &kai]).await;
    server.befriend(&tomas.user, &kai.user);
    server.befriend(&tomas.user, &outsider.user);

    let (area, secret) = map_with_secret(&server, &tomas.user, "Solace", "Cache");
    // Kai holds two grants that merge; the outsider's grant and share end.
    server.grant_secret(area, secret, &tomas.user, &kai.user, &["edit"]);
    server.grant_secret(area, secret, &tomas.user, &kai.user, &["add"]);
    server.grant_secret(area, secret, &tomas.user, &outsider.user, &[]);
    server.grant(
        &tomas.user,
        &outsider.user,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );

    // Owners hold atlas.accept_transfer; a plain member does not.
    let summary = mira.api.clan(clan).await.unwrap();
    assert!(summary.can(action::ACCEPT_TRANSFER));
    assert!(
        !kai.api
            .clan(clan)
            .await
            .unwrap()
            .can(action::ACCEPT_TRANSFER)
    );

    let offer = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Clan)
        .await
        .unwrap();
    assert!(offer_names_clan(&offer, clan), "{offer:?}");
    assert_eq!(offer.status, "Offered");

    // It is the offerer's offer, and the clan's: nobody "received" it.
    let offered = tomas
        .api
        .transfers(TransferDirection::Offered)
        .await
        .unwrap();
    assert!(
        offered
            .iter()
            .any(|t| t.id == offer.id && offer_names_clan(t, clan))
    );
    assert!(
        mira.api
            .transfers(TransferDirection::Received)
            .await
            .unwrap()
            .is_empty()
    );
    let incoming = mira.api.clan_transfers(clan).await.unwrap();
    assert_eq!(incoming.len(), 1);
    assert!(offer_names_clan(&incoming[0], clan));
    assert_eq!(incoming[0].from_nickname.as_deref(), Some("tomas"));
    assert_eq!(incoming[0].subject_name.as_deref(), Some("Solace"));
    assert!(is_not_found(&kai.api.clan_transfers(clan).await));
    assert!(is_not_found(&outsider.api.clan_transfers(clan).await));

    // A map needs a folder; a plain member may not accept.
    let missing = mira.api.accept_transfer(offer.id, None, None).await;
    assert!(
        matches!(missing, Err(CloudError::InvalidInput(_))),
        "{missing:?}"
    );
    assert!(is_not_found(
        &kai.api.accept_transfer(offer.id, None, Some(roads)).await
    ));
    let accepted = mira
        .api
        .accept_transfer(offer.id, None, Some(roads))
        .await
        .unwrap();
    assert_eq!(accepted.status, "Accepted");
    assert!(mira.api.clan_transfers(clan).await.unwrap().is_empty());

    // The clan owns it, filed in the folder.
    let listed = row(&mira, area).await.expect("the clan's map");
    assert_eq!(listed.user_id, None);
    assert_eq!(listed.clan_id, Some(clan));
    assert_eq!(listed.atlas_id, Some(roads));
    let former = row(&tomas, area).await.expect("Tomas reads it as a member");
    let access = former.access.unwrap();
    assert!(!access.is_owner && !access.can_admin);
    assert!(row(&outsider, area).await.is_none(), "shares end");

    // The Secret is Member-owned, Tomas its owner; the clan's owner sees
    // nothing of it, Kai keeps one merged grant.
    assert_eq!(
        secrets(&tomas, area).await,
        vec![("Cache".to_string(), "members".to_string(), Some(clan))]
    );
    let tomas_view = tomas.api.area_secrets(area).await.unwrap();
    assert!(tomas_view[0].actions.contains("manage_ownership"));
    // The Clan Secret pages agree: Tomas its one recorded owner, Kai a
    // reader through his grant, the clan's owner nowhere.
    let owners = tomas
        .api
        .secret_owners(&SourceId::Secret(secret))
        .await
        .unwrap();
    assert_eq!(
        owners
            .iter()
            .map(|owner| (owner.user_id, owner.active))
            .collect::<Vec<_>>(),
        vec![(tomas.user.id, true)]
    );
    let access = tomas
        .api
        .secret_access(&SourceId::Secret(secret))
        .await
        .unwrap();
    assert_eq!(access.ownership, "members");
    assert_eq!(access.clan_id, clan);
    let readers: Vec<Uuid> = access.members.iter().map(|reader| reader.user_id).collect();
    assert!(readers.contains(&tomas.user.id) && readers.contains(&kai.user.id));
    assert!(!readers.contains(&mira.user.id));
    assert!(secrets(&mira, area).await.is_empty());
    let kai_view = kai.api.area_secrets(area).await.unwrap();
    assert_eq!(kai_view.len(), 1);
    assert_eq!(actions(&kai_view[0].actions), vec!["add", "edit", "read"]);
    assert!(is_not_found(&outsider.api.area_secrets(area).await));

    // A clan's map is never transferred, by anyone.
    assert!(is_not_found(
        &tomas.api.offer_area_transfer(area, kai.user.id).await
    ));
    assert!(is_not_found(
        &server
            .legacy_clan_transfer(&mira.user, Some(area), None, clan, MapOwnership::Clan)
            .await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_atlas_offered_to_a_clan_becomes_one_of_its_folders() {
    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let tomas = person(&server, "tomas");
    let (clan, _roads) = clan_with_folder(&server, &mira, &[&tomas]).await;
    let atlas = server.create_atlas(&tomas.user, "Tomas's Roads");
    let area = server.create_area_in_atlas(&tomas.user, "Old Road", atlas);
    let offer = server
        .legacy_clan_transfer(
            &tomas.user,
            None,
            Some(AtlasId(atlas)),
            clan,
            MapOwnership::Clan,
        )
        .await
        .unwrap();
    assert_eq!(offer.atlas_id, Some(AtlasId(atlas)));
    assert!(offer_names_clan(&offer, clan));

    // An atlas needs atlas.accept_transfer on the clan; a folder is ignored.
    mira.api
        .accept_transfer(offer.id, None, None)
        .await
        .unwrap();
    let folders = mira.maps.list_atlases().await.unwrap();
    let folder = folders
        .iter()
        .find(|folder| folder.id == AtlasId(atlas))
        .expect("the atlas is one of the clan's folders");
    assert_eq!(folder.clan_id, Some(clan));
    let listed = row(&mira, area).await.expect("its map came along");
    assert_eq!((listed.user_id, listed.clan_id), (None, Some(clan)));
    assert_eq!(listed.atlas_id, Some(AtlasId(atlas)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfers_into_a_clan_refuse_what_they_must() {
    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let tomas = person(&server, "tomas");
    let kai = person(&server, "kai");
    let outsider = person(&server, "outsider");
    let (clan, roads) = clan_with_folder(&server, &mira, &[&tomas, &kai]).await;

    // The clan's own maps and folders are never offered, even by its owner.
    let clan_map = mira
        .api
        .create_clan_area(clan, roads, "Clan Hall", MapOwnership::Clan)
        .await
        .unwrap()
        .id;
    assert!(is_not_found(
        &mira.api.offer_area_transfer(clan_map, tomas.user.id).await
    ));
    assert!(is_not_found(
        &mira.api.offer_atlas_transfer(roads, tomas.user.id).await
    ));
    assert!(is_not_found(
        &mira
            .api
            .transfer_area_to_clan(clan_map, clan, MapOwnership::Clan, roads, Uuid::new_v4())
            .await
    ));
    assert!(is_not_found(
        &mira
            .api
            .transfer_atlas_to_clan(roads, clan, MapOwnership::Clan, Uuid::new_v4())
            .await
    ));

    // Only a member offers to the clan, and only their own map.
    let area = server.create_area(&tomas.user, "Workshop");
    let outsiders = server.create_area(&outsider.user, "Shed");
    assert!(is_not_found(
        &server
            .legacy_clan_transfer(
                &outsider.user,
                Some(outsiders),
                None,
                clan,
                MapOwnership::Clan
            )
            .await
    ));
    assert!(is_not_found(
        &server
            .legacy_clan_transfer(&kai.user, Some(area), None, clan, MapOwnership::Clan)
            .await
    ));

    // The body names exactly one recipient.
    let http = reqwest::Client::new();
    for body in [
        serde_json::json!({}),
        serde_json::json!({ "to_user_id": kai.user.id, "to_clan_id": clan }),
    ] {
        let status = http
            .post(format!("{}/areas/{}/transfer", server.base_url, area))
            .bearer_auth(&tomas.user.api_key)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status.as_u16(), 400, "{body}");
    }

    // One live offer per subject; decline by an acceptor, cancel by the
    // offerer, nothing for anyone else.
    let offer = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Clan)
        .await
        .unwrap();
    let again = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Clan)
        .await;
    assert!(again.is_err(), "{again:?}");
    assert!(is_not_found(&kai.api.decline_transfer(offer.id).await));
    assert!(is_not_found(&mira.api.cancel_transfer(offer.id).await));
    mira.api.decline_transfer(offer.id).await.unwrap();
    assert!(mira.api.clan_transfers(clan).await.unwrap().is_empty());
    let offer = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Clan)
        .await
        .unwrap();
    tomas.api.cancel_transfer(offer.id).await.unwrap();
    assert!(is_not_found(
        &mira.api.accept_transfer(offer.id, None, Some(roads)).await
    ));

    // A folder of another clan, or one the acceptor may not fill, is refused
    // and the offer stands.
    let other = kai.api.create_clan("Other Company").await.unwrap().id;
    let theirs = kai.api.create_clan_atlas(other, "Theirs").await.unwrap().id;
    let offer = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Clan)
        .await
        .unwrap();
    assert!(is_not_found(
        &mira.api.accept_transfer(offer.id, None, Some(theirs)).await
    ));
    assert_eq!(mira.api.clan_transfers(clan).await.unwrap().len(), 1);

    // Leaving the clan cancels the member's offers to it.
    tomas
        .api
        .remove_clan_member(clan, tomas.user.id)
        .await
        .unwrap();
    assert!(mira.api.clan_transfers(clan).await.unwrap().is_empty());
    assert!(is_not_found(
        &mira.api.accept_transfer(offer.id, None, Some(roads)).await
    ));
    assert_eq!(
        row(&tomas, area).await.unwrap().user_id,
        Some(tomas.user.id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_map_that_stays_its_givers_arrives_member_owned() {
    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let tomas = person(&server, "tomas");
    let kai = person(&server, "kai");
    let (clan, roads) = clan_with_folder(&server, &mira, &[&tomas, &kai]).await;
    let (area, _) = map_with_secret(&server, &tomas.user, "Crossroads", "Cellar");
    server.befriend(&tomas.user, &kai.user);
    server.grant(
        &tomas.user,
        &kai.user,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );

    let offer = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Members)
        .await
        .unwrap();
    assert_eq!(offer.ownership, Some(MapOwnership::Members));
    let listed = mira.api.clan_transfers(clan).await.unwrap();
    assert_eq!(listed[0].ownership, Some(MapOwnership::Members));
    mira.api
        .accept_transfer(offer.id, None, Some(roads))
        .await
        .expect("the clan admits it");

    // Tomas owns it in the clan; the clan's own grants and its owners do
    // not reach it, and the friend share ended.
    let moved = row(&tomas, area).await.expect("its owner reads it");
    assert_eq!(moved.clan_id, Some(clan));
    assert_eq!(moved.atlas_id, Some(roads));
    assert!(moved.clan_ownership.member_owned());
    assert!(moved.clan_ownership.owned_by_me);
    assert!(moved.can(action::DELETE_AREA));
    assert!(
        row(&mira, area).await.is_none(),
        "clan owners do not see it"
    );
    assert!(row(&kai, area).await.is_none(), "shares end");
    assert_eq!(server.map_owners(area), Some(vec![tomas.user.id]));
    // Its owner Secret is Tomas's Member-owned Secret now.
    assert!(
        secrets(&tomas, area)
            .await
            .iter()
            .any(|(name, ownership, _)| name == "Cellar" && ownership == "members")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_block_or_departure_during_an_acceptance_ends_it() {
    let server = MockServer::spawn().await;
    let tomas = person(&server, "tomas");
    let kai = person(&server, "kai");
    server.befriend(&tomas.user, &kai.user);
    let area = server.create_area(&tomas.user, "Crossroads");

    // A block raised while the acceptance runs: the flip moves nothing, the
    // answer is the uniform 404, and the offer is cancelled.
    let offer = tomas
        .api
        .offer_area_transfer(area, TransferRecipient::User(kai.user.id))
        .await
        .unwrap();
    server.interrupt_next_acceptance(|st, offer| {
        st.blocks.push(support::state::BlockRecord {
            blocker_id: offer.to_user_id.expect("a user"),
            blocked_id: offer.from_user_id,
            created_at: chrono::Utc::now(),
        });
        false
    });
    assert!(is_not_found(
        &kai.api.accept_transfer(offer.id, None, None).await
    ));
    assert!(
        kai.api
            .transfers(TransferDirection::Received)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        row(&tomas, area).await.unwrap().user_id,
        Some(tomas.user.id),
        "the map stays its offerer's"
    );
    assert!(row(&kai, area).await.is_none(), "nothing reached Kai");

    // The offerer leaving the recipient clan meanwhile ends it the same way.
    let (clan, roads) = clan_with_folder(&server, &kai, &[&tomas]).await;
    let given = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Clan)
        .await
        .unwrap();
    server.interrupt_next_acceptance(|st, offer| {
        let clan = offer.to_clan_id.expect("a clan");
        if let Some(member) = st
            .clans
            .clans
            .get_mut(&clan)
            .and_then(|record| record.members.get_mut(&offer.from_user_id))
        {
            member.status = support::clans::MemberStatus::Left;
        }
        false
    });
    assert!(is_not_found(
        &kai.api.accept_transfer(given.id, None, Some(roads)).await
    ));
    assert!(kai.api.clan_transfers(clan).await.unwrap().is_empty());
    assert_eq!(
        row(&tomas, area).await.unwrap().user_id,
        Some(tomas.user.id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_acceptance_stopped_before_its_flip_cancels_an_offer_that_cannot_stand() {
    let server = MockServer::spawn().await;
    let tomas = person(&server, "tomas");
    let kai = person(&server, "kai");
    server.befriend(&tomas.user, &kai.user);
    let area = server.create_area(&tomas.user, "Crossroads");
    let offer = tomas
        .api
        .offer_area_transfer(area, TransferRecipient::User(kai.user.id))
        .await
        .unwrap();

    // A failure alone puts the offer back.
    server.interrupt_next_acceptance(|_, _| true);
    assert!(is_not_found(
        &kai.api.accept_transfer(offer.id, None, None).await
    ));
    let received = kai
        .api
        .transfers(TransferDirection::Received)
        .await
        .unwrap();
    assert_eq!(received.len(), 1, "the offer stands");

    // An unfriending that ran meanwhile cancels it: the lists drop it.
    server.interrupt_next_acceptance(|st, offer| {
        let (a, b) = (offer.from_user_id, offer.to_user_id.expect("a user"));
        st.friendships.retain(|f| {
            !((f.requester_id == a && f.addressee_id == b)
                || (f.requester_id == b && f.addressee_id == a))
        });
        true
    });
    assert!(is_not_found(
        &kai.api.accept_transfer(offer.id, None, None).await
    ));
    assert!(
        kai.api
            .transfers(TransferDirection::Received)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        tomas
            .api
            .transfers(TransferDirection::Offered)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(is_not_found(
        &kai.api.accept_transfer(offer.id, None, None).await
    ));
    assert_eq!(
        row(&tomas, area).await.unwrap().user_id,
        Some(tomas.user.id)
    );

    // An offerer whose deletion began meanwhile transfers nothing either.
    let (clan, roads) = clan_with_folder(&server, &kai, &[&tomas]).await;
    let given = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Clan)
        .await
        .unwrap();
    server.interrupt_next_acceptance(|st, offer| {
        st.deleting_accounts.insert(offer.from_user_id);
        false
    });
    assert!(is_not_found(
        &kai.api.accept_transfer(given.id, None, Some(roads)).await
    ));
    assert!(kai.api.clan_transfers(clan).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_retried_after_a_transfer_replays_its_result() {
    let server = MockServer::spawn().await;
    let tomas = person(&server, "tomas");
    let kai = person(&server, "kai");
    server.befriend(&tomas.user, &kai.user);
    let area = server.create_area(&tomas.user, "Crossroads");
    server.add_room(area, 1, "Gate");
    let envelope = serde_json::json!({
        "operation_id": Uuid::new_v4(),
        "preconditions": [{
            "resource": "source",
            "id": area,
            "expected_rev": server.area_rev(area),
        }],
        "payload": [{"op": "upsert_room", "room_number": 1, "body": {"title": "Old gate"}}],
    });
    let http = reqwest::Client::new();
    let post = || {
        http.post(format!("{}/areas/{area}/mutations", server.base_url))
            .header("authorization", format!("Bearer {}", tomas.user.api_key))
            .json(&envelope)
            .send()
    };
    let first = post().await.unwrap();
    assert!(first.status().is_success());
    let first: serde_json::Value = first.json().await.unwrap();

    let offer = tomas
        .api
        .offer_area_transfer(area, TransferRecipient::User(kai.user.id))
        .await
        .unwrap();
    kai.api.accept_transfer(offer.id, None, None).await.unwrap();

    // The former owner reads it through the share-back, and the retry
    // replays the stored answer although the map moved and its revision
    // with it.
    let again = post().await.unwrap();
    assert!(again.status().is_success());
    let again: serde_json::Value = again.json().await.unwrap();
    assert_eq!(again, first);
}

// ---------------------------------------------------------------------------
// Copies
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_carries_the_secrets_its_copier_may_copy_and_nothing_else() {
    let server = MockServer::spawn().await;
    let ana = person(&server, "ana");
    let ben = person(&server, "ben");
    server.befriend(&ana.user, &ben.user);
    let (area, known) = map_with_secret(&server, &ana.user, "Archive", "Known");
    server.set_secret_room_property(area, known, SecretPlace::Map(1), "key", "under the mat");
    let glimpsed = server.add_secret(area, "Glimpsed");
    server.add_secret_room(area, glimpsed, 8, "Reading room");
    let unknown = server.add_secret(area, "Unknown");
    server.add_secret_room(area, unknown, 9, "Vault");
    server.grant_secret(area, known, &ana.user, &ben.user, &["copy"]);
    // Every other action, but not `copy`: Ben reads and edits it, and it
    // stays behind.
    server.grant_secret(
        area,
        glimpsed,
        &ana.user,
        &ben.user,
        &["add", "edit", "remove", "manage_access"],
    );
    server.grant(
        &ana.user,
        &ben.user,
        GrantScope::Area(area),
        GrantFlags {
            can_copy: true,
            ..GrantFlags::VIEW_ONLY
        },
    );

    let copy = ben
        .api
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .unwrap();
    assert_eq!(copy.user_id, Some(ben.user.id));

    // The Secret Ben may copy is his own owner Secret on the copy, with a
    // new id, every action and no grants; the one he only reads and the
    // one he does not read left no trace.
    let carried = ben.api.area_secrets(copy.id).await.unwrap();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].name, "Known");
    assert_eq!(carried[0].ownership, "owner");
    assert_ne!(carried[0].source, SourceId::Secret(known));
    assert_eq!(
        actions(&carried[0].actions),
        vec![
            "add",
            "copy",
            "delete",
            "edit",
            "manage_access",
            "read",
            "remove",
            "rename"
        ]
    );
    assert!(
        ben.api
            .secret_grants(&carried[0].source)
            .await
            .unwrap()
            .is_empty()
    );
    {
        let st = server.state.lock();
        let copied = &st.areas[&copy.id.0];
        assert_eq!(copied.secrets.len(), 1);
        let secret = &copied.secrets[0];
        assert_eq!(secret.rev, 1);
        assert!(secret.rooms.contains_key(&7), "its own rooms come along");
        let held = secret.rooms.values().any(|room| {
            room.properties
                .get("key")
                .is_some_and(|p| p.value == "under the mat")
        });
        assert!(held, "its data on map rooms comes along");
        assert!(
            !copied.rooms.contains_key(&8) && !copied.rooms.contains_key(&9),
            "nothing of the Secrets left behind reaches the copy"
        );
    }
    // Ana neither reads the copy nor its Secret.
    assert!(is_not_found(&ana.api.area_secrets(copy.id).await));
    // The original keeps every Secret and its grants.
    assert_eq!(secrets(&ana, area).await.len(), 3);
    assert_eq!(secrets(&ben, area).await.len(), 2);

    // Ana's own copy carries every owner Secret: the owner holds `copy`.
    let own = ana
        .api
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .unwrap();
    let names: Vec<String> = secrets(&ana, own.id)
        .await
        .into_iter()
        .map(|(name, _, _)| name)
        .collect();
    assert_eq!(names, ["Known", "Glimpsed", "Unknown"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clan_map_is_copied_out_by_a_member_holding_area_copy() {
    let server = MockServer::spawn().await;
    let mira = person(&server, "mira");
    let tomas = person(&server, "tomas");
    let kai = person(&server, "kai");
    let (clan, roads) = clan_with_folder(&server, &mira, &[&tomas, &kai]).await;
    server.befriend(&tomas.user, &kai.user);
    let (area, secret) = map_with_secret(&server, &tomas.user, "Solace", "Cache");
    server.grant_secret(area, secret, &tomas.user, &kai.user, &[]);
    let offer = server
        .legacy_clan_transfer(&tomas.user, Some(area), None, clan, MapOwnership::Clan)
        .await
        .unwrap();
    mira.api
        .accept_transfer(offer.id, None, Some(roads))
        .await
        .unwrap();

    // Read alone does not copy.
    assert!(is_not_found(
        &kai.api.copy_area(area, &CopyAreaRequest::default()).await
    ));
    let everyone = server.clan_builtin_group(clan, "members");
    server.clan_grant(
        clan,
        ClanRecipient::Group(everyone),
        ClanGrantScope::Atlases([roads.0].into_iter().collect()),
        &["area.read", "area.copy"],
    );
    let copy = kai
        .api
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .unwrap();
    assert_eq!(copy.user_id, Some(kai.user.id));
    let listed = row(&kai, copy.id).await.expect("Kai's own copy");
    assert_eq!(listed.clan_id, None);
    assert!(listed.access.unwrap().is_owner);
    // Kai reads the Member-owned Secret, but his grant does not name
    // `copy`: his copy is made as if it did not exist.
    assert_eq!(secrets(&kai, area).await.len(), 1);
    assert!(secrets(&kai, copy.id).await.is_empty());

    // Its recorded owner gives him `copy`, and his next copy carries it as
    // his own owner Secret.
    let cache = SourceId::Secret(secret);
    let grant = tomas
        .api
        .clan_secret_grants(&cache)
        .await
        .unwrap()
        .into_iter()
        .find(|grant| grant.recipient.user() == Some(kai.user.id))
        .expect("Kai's grant came along into the clan");
    tomas
        .api
        .update_clan_secret_grant(
            &cache,
            grant.id,
            &SecretGrantChange {
                add: ["copy".to_string()].into(),
                remove: BTreeSet::new(),
            },
        )
        .await
        .unwrap();
    let copy = kai
        .api
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .unwrap();
    assert_eq!(
        secrets(&kai, copy.id).await,
        vec![("Cache".to_string(), "owner".to_string(), None)]
    );

    // Its recorded owner holds `copy` through ownership.
    let owned = tomas
        .api
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .unwrap();
    assert_eq!(secrets(&tomas, owned.id).await.len(), 1);

    // The clan's owner copies it too, without a Secret they never read.
    let owners_copy = mira
        .api
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .unwrap();
    assert!(secrets(&mira, owners_copy.id).await.is_empty());
}
