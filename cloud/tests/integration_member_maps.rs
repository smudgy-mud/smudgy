//! Member-owned clan maps end to end over real HTTP against the
//! contract-shaped mock in `tests/support/clan_maps.rs` (docs/clans.md §7):
//! creating one, who reaches it, its owners and ownership offers, Make
//! Clan-owned and back, outside shares of Clan-owned maps, folder deletion
//! that leaves hidden maps unfiled, and the copies a departing owner takes.
#![allow(clippy::too_many_lines)]

mod support;

use smudgy_cloud::clan_maps::MapOwnership;
use smudgy_cloud::clans::{ClanGrantFilter, GrantRecipient, GrantScope, action};
use smudgy_cloud::{
    Area, AreaId, AtlasId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource,
    MapperBackend,
};
use support::clans::{ClanGrantScope, ClanRecipient};
use support::{MockHandle, MockServer, TestUser};
use uuid::Uuid;

struct Member {
    user: TestUser,
    api: CloudApiClient,
    maps: CloudMapper,
}

fn member(server: &MockHandle, nickname: &str) -> Member {
    let user = server.create_user(&format!("{nickname}@example.com"), nickname, true);
    let api = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    );
    let maps = CloudMapper::new(server.base_url.clone(), user.api_key.clone());
    Member { user, api, maps }
}

fn is_not_found<T: std::fmt::Debug>(result: &Result<T, CloudError>) -> bool {
    matches!(result, Err(CloudError::NotFoundOrNoAccess))
}

async fn row(member: &Member, area: AreaId) -> Option<Area> {
    member
        .maps
        .list_areas()
        .await
        .expect("list maps")
        .into_iter()
        .find(|row| row.id == area)
}

/// A clan of Mira's, "Lantern Company", with a folder "Roads" where every
/// member reads the clan's maps and may create Member-owned ones, and
/// `others` joined.
struct Clan {
    server: MockHandle,
    id: Uuid,
    roads: AtlasId,
    everyone: Uuid,
    mira: Member,
}

async fn clan(others: &[&str]) -> (Clan, Vec<Member>) {
    let server = MockServer::spawn().await;
    let mira = member(&server, "mira");
    let id = mira.api.create_clan("Lantern Company").await.unwrap().id;
    let roads = mira.api.create_clan_atlas(id, "Roads").await.unwrap().id;
    let everyone = server.clan_builtin_group(id, "members");
    server.clan_grant(
        id,
        ClanRecipient::Group(everyone),
        ClanGrantScope::Atlases([roads.0].into()),
        &["area.read"],
    );
    server.clan_grant(
        id,
        ClanRecipient::Group(everyone),
        ClanGrantScope::Atlases([roads.0].into()),
        &["area.create_member_owned"],
    );
    let members: Vec<Member> = others
        .iter()
        .map(|nickname| {
            let joined = member(&server, nickname);
            server.join_clan(id, &joined.user);
            joined
        })
        .collect();
    (
        Clan {
            server,
            id,
            roads,
            everyone,
            mira,
        },
        members,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_owned_map_reaches_only_its_owners_and_its_own_grants() {
    let (c, people) = clan(&["nessa", "arun"]).await;
    let (nessa, arun) = (&people[0], &people[1]);

    // Creating one needs area.create_member_owned on the folder; the
    // creator is its owner.
    let map = nessa
        .api
        .create_clan_area(c.id, c.roads, "Hidden Grove", MapOwnership::Members)
        .await
        .expect("Nessa may create Member-owned maps in Roads")
        .id;
    let mine = row(nessa, map).await.expect("its owner reads it");
    assert!(mine.clan_ownership.member_owned());
    assert!(mine.clan_ownership.owned_by_me);
    assert_eq!(mine.clan_id, Some(c.id));
    assert!(mine.can(action::DELETE_AREA) && mine.can(action::COPY_AREA));

    // Clan-wide reading and the clan's owners do not reach it.
    assert!(row(&c.mira, map).await.is_none());
    assert!(row(arun, map).await.is_none());
    assert!(is_not_found(&c.mira.api.area_owners(map).await));
    let folder = c
        .mira
        .maps
        .list_atlases()
        .await
        .unwrap()
        .into_iter()
        .find(|atlas| atlas.id == c.roads)
        .unwrap();
    assert_eq!(folder.area_count, 0, "counts leave it out");

    // Its owner grants Arun alone; Arun reads it, Mira still does not, and
    // only its owner and the grantee see the grant.
    let grant = nessa
        .api
        .create_clan_grant(
            c.id,
            GrantRecipient::User {
                user_id: arun.user.id,
            },
            &GrantScope::Areas { ids: vec![map] },
            &[action::EDIT_AREA],
        )
        .await
        .expect("an owner grants one member");
    assert!(grant.actions.contains(action::READ_AREA));
    let arun_row = row(arun, map).await.expect("Arun reads it now");
    assert!(arun_row.can(action::EDIT_AREA));
    assert!(!arun_row.clan_ownership.owned_by_me);
    assert!(row(&c.mira, map).await.is_none());
    let filter = ClanGrantFilter {
        area_id: Some(map),
        ..ClanGrantFilter::default()
    };
    assert_eq!(nessa.api.clan_grants(c.id, filter).await.unwrap().len(), 1);
    assert_eq!(arun.api.clan_grants(c.id, filter).await.unwrap().len(), 1);
    assert!(
        c.mira
            .api
            .clan_grants(c.id, filter)
            .await
            .unwrap()
            .is_empty()
    );

    // Nobody else writes its grants, clan owners included; a scope naming it
    // beside another map is refused to its owner.
    assert!(is_not_found(
        &c.mira
            .api
            .create_clan_grant(
                c.id,
                GrantRecipient::Group {
                    group_id: c.everyone
                },
                &GrantScope::Areas { ids: vec![map] },
                &[action::READ_AREA],
            )
            .await
    ));
    let other = c
        .mira
        .api
        .create_clan_area(c.id, c.roads, "Roads", MapOwnership::Clan)
        .await
        .unwrap()
        .id;
    assert!(matches!(
        nessa
            .api
            .create_clan_grant(
                c.id,
                GrantRecipient::Group {
                    group_id: c.everyone
                },
                &GrantScope::Areas {
                    ids: vec![map, other]
                },
                &[action::READ_AREA],
            )
            .await,
        Err(CloudError::InvalidInput(_))
    ));
    // Actions outside a Member-owned map's own are refused.
    assert!(matches!(
        nessa
            .api
            .update_clan_grant(c.id, grant.id, &["area.share_external"])
            .await,
        Err(CloudError::InvalidInput(_))
    ));

    // Without the folder action, creating one is the uniform 404.
    let outsider_folder = c
        .mira
        .api
        .create_clan_atlas(c.id, "Towns")
        .await
        .unwrap()
        .id;
    assert!(is_not_found(
        &nessa
            .api
            .create_clan_area(c.id, outsider_folder, "Nope", MapOwnership::Members)
            .await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joint_ownership_offers_apply_on_the_last_acceptance() {
    let (c, people) = clan(&["nessa", "arun", "bo"]).await;
    let (nessa, arun, bo) = (&people[0], &people[1], &people[2]);
    let map = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    for reader in [arun, bo] {
        nessa
            .api
            .create_clan_grant(
                c.id,
                GrantRecipient::User {
                    user_id: reader.user.id,
                },
                &GrantScope::Areas { ids: vec![map] },
                &[action::READ_AREA],
            )
            .await
            .unwrap();
    }

    let offer = nessa
        .api
        .offer_area_ownership(
            map,
            &[arun.user.id, bo.user.id],
            MapOwnership::Members,
            false,
        )
        .await
        .expect("an owner offers to readers");
    assert_eq!(offer.recipients.len(), 2);
    let mine = arun.api.my_area_offers().await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].area_name, "Grove");
    // A clan owner who does not read it learns nothing of it.
    assert!(c.mira.api.my_area_offers().await.unwrap().is_empty());
    assert!(is_not_found(&c.mira.api.area_ownership_offers(map).await));

    arun.api
        .accept_area_offer(map, offer.id, None)
        .await
        .unwrap();
    assert_eq!(c.server.map_owners(map), Some(vec![nessa.user.id]));
    bo.api.accept_area_offer(map, offer.id, None).await.unwrap();
    assert_eq!(
        c.server.map_owners(map),
        Some(vec![nessa.user.id, arun.user.id, bo.user.id])
    );
    assert!(row(bo, map).await.unwrap().clan_ownership.owned_by_me);

    // Owners leave on their own; the last one cannot.
    nessa
        .api
        .remove_area_owner(map, nessa.user.id)
        .await
        .unwrap();
    arun.api.remove_area_owner(map, bo.user.id).await.unwrap();
    assert!(matches!(
        arun.api.remove_area_owner(map, arun.user.id).await,
        Err(CloudError::LastOwner)
    ));
    let owners = arun.api.area_owners(map).await.unwrap();
    assert_eq!(owners.len(), 1);
    assert!(owners[0].active);

    // A declined offer ends for everyone; a withdrawn one goes.
    let offer = arun
        .api
        .offer_area_ownership(map, &[bo.user.id], MapOwnership::Members, true)
        .await
        .unwrap();
    bo.api.decline_area_offer(map, offer.id).await.unwrap();
    assert!(bo.api.my_area_offers().await.unwrap().is_empty());
    let offer = arun
        .api
        .offer_area_ownership(map, &[bo.user.id], MapOwnership::Members, true)
        .await
        .unwrap();
    arun.api.withdraw_area_offer(map, offer.id).await.unwrap();
    assert!(
        arun.api
            .area_ownership_offers(map)
            .await
            .unwrap()
            .is_empty()
    );

    // A frozen map takes no offers or grant writes.
    c.server.freeze_map(map);
    assert!(is_not_found(
        &arun
            .api
            .offer_area_ownership(map, &[bo.user.id], MapOwnership::Members, false)
            .await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declining_a_map_offer_needs_the_read_its_listing_needs() {
    let (c, people) = clan(&["nessa", "arun"]).await;
    let (nessa, arun) = (&people[0], &people[1]);
    let map = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    let reading = nessa
        .api
        .create_clan_grant(
            c.id,
            GrantRecipient::User {
                user_id: arun.user.id,
            },
            &GrantScope::Areas { ids: vec![map] },
            &[action::READ_AREA],
        )
        .await
        .unwrap();
    let offer = nessa
        .api
        .offer_area_ownership(map, &[arun.user.id], MapOwnership::Members, false)
        .await
        .unwrap();
    assert_eq!(arun.api.my_area_offers().await.unwrap().len(), 1);

    // Arun stops reading the map: declining its offer is the uniform 404,
    // as for an offer that never was.
    nessa.api.delete_clan_grant(c.id, reading.id).await.unwrap();
    assert!(arun.api.my_area_offers().await.unwrap().is_empty());
    assert!(is_not_found(
        &arun.api.decline_area_offer(map, offer.id).await
    ));
    assert!(is_not_found(
        &arun.api.decline_area_offer(map, Uuid::new_v4()).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ownership_offer_lapses_when_a_grant_naming_its_map_changes() {
    use smudgy_cloud::clan_access::GrantBody;

    let (c, people) = clan(&["nessa", "arun", "bo"]).await;
    let (nessa, arun, bo) = (&people[0], &people[1], &people[2]);
    let map = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    let mut grants = Vec::new();
    for reader in [arun, bo] {
        grants.push(
            nessa
                .api
                .create_clan_grant(
                    c.id,
                    GrantRecipient::User {
                        user_id: reader.user.id,
                    },
                    &GrantScope::Areas { ids: vec![map] },
                    &[action::READ_AREA],
                )
                .await
                .unwrap(),
        );
    }

    // A grant naming the map changes after the offer: the offer lapses.
    let offer = nessa
        .api
        .offer_area_ownership(map, &[arun.user.id], MapOwnership::Members, false)
        .await
        .unwrap();
    nessa
        .api
        .change_clan_grant(
            c.id,
            grants[1].id,
            &GrantBody::of(["area.read", "area.edit"]),
        )
        .await
        .unwrap();
    assert!(is_not_found(
        &arun.api.accept_area_offer(map, offer.id, None).await
    ));
    assert_eq!(c.server.map_owners(map), Some(vec![nessa.user.id]));

    // So does one made after a new grant names it.
    let offer = nessa
        .api
        .offer_area_ownership(map, &[arun.user.id], MapOwnership::Members, false)
        .await
        .unwrap();
    nessa
        .api
        .create_clan_grant(
            c.id,
            GrantRecipient::User {
                user_id: c.mira.user.id,
            },
            &GrantScope::Areas { ids: vec![map] },
            &[action::READ_AREA],
        )
        .await
        .unwrap();
    assert!(is_not_found(
        &arun.api.accept_area_offer(map, offer.id, None).await
    ));

    // An offer made after the change stands.
    let offer = nessa
        .api
        .offer_area_ownership(map, &[arun.user.id], MapOwnership::Members, false)
        .await
        .unwrap();
    arun.api
        .accept_area_offer(map, offer.id, None)
        .await
        .expect("nothing changed since the offer");
    assert_eq!(
        c.server.map_owners(map),
        Some(vec![nessa.user.id, arun.user.id])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_owned_map_becomes_clan_owned_and_back() {
    let (c, people) = clan(&["nessa", "arun"]).await;
    let (nessa, arun) = (&people[0], &people[1]);
    let map = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    nessa
        .api
        .create_clan_grant(
            c.id,
            GrantRecipient::User {
                user_id: arun.user.id,
            },
            &GrantScope::Areas { ids: vec![map] },
            &[action::EDIT_AREA],
        )
        .await
        .unwrap();

    // Offered to the clan: its owners see the offer, and accept it into a
    // folder.
    let offer = nessa
        .api
        .offer_area_ownership(map, &[], MapOwnership::Clan, false)
        .await
        .unwrap();
    assert!(offer.to_clan());
    let waiting = c.mira.api.clan_area_offers(c.id).await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert!(is_not_found(&arun.api.clan_area_offers(c.id).await));
    assert!(matches!(
        c.mira.api.accept_area_offer(map, offer.id, None).await,
        Err(CloudError::InvalidInput(_))
    ));
    let accepted = c
        .mira
        .api
        .accept_area_offer(map, offer.id, Some(c.roads))
        .await
        .unwrap();
    assert_eq!(accepted.ownership, MapOwnership::Clan);
    // Clan-wide grants reach it now; its own grants stay.
    let mira_row = row(&c.mira, map).await.expect("the clan holds it");
    assert_eq!(mira_row.clan_ownership.ownership, Some(MapOwnership::Clan));
    assert!(row(arun, map).await.unwrap().can(action::EDIT_AREA));
    assert_eq!(c.server.map_owners(map), None);

    // Back to members: a clan owner offers it; the recipients own it, and
    // clan-wide reading stops.
    let back = c
        .mira
        .api
        .offer_area_ownership(map, &[nessa.user.id], MapOwnership::Members, false)
        .await
        .unwrap();
    nessa
        .api
        .accept_area_offer(map, back.id, None)
        .await
        .unwrap();
    assert_eq!(c.server.map_owners(map), Some(vec![nessa.user.id]));
    assert!(row(&c.mira, map).await.is_none());
    // Arun's grant names the map alone, so it stays.
    assert!(row(arun, map).await.unwrap().can(action::EDIT_AREA));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_folder_unfiles_the_maps_its_deleter_cannot_see() {
    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    let map = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    c.mira
        .maps
        .delete_atlas(&c.roads)
        .await
        .expect("nothing Mira reads holds it");
    let unfiled = row(nessa, map).await.expect("its owner still has it");
    assert_eq!(unfiled.atlas_id, None);
    assert_eq!(unfiled.clan_id, Some(c.id));

    // A folder holding a Member-owned map its deleter reads stays.
    let towns = c
        .mira
        .api
        .create_clan_atlas(c.id, "Towns")
        .await
        .unwrap()
        .id;
    let shared = c
        .server
        .create_member_owned_area(c.id, towns.0, "Square", &[nessa.user.id]);
    nessa
        .api
        .create_clan_grant(
            c.id,
            GrantRecipient::User {
                user_id: c.mira.user.id,
            },
            &GrantScope::Areas { ids: vec![shared] },
            &[action::READ_AREA],
        )
        .await
        .unwrap();
    assert!(matches!(
        c.mira.maps.delete_atlas(&towns).await,
        Err(CloudError::AtlasNotEmpty)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn departing_owners_take_copies() {
    let (c, people) = clan(&["nessa", "arun"]).await;
    let (nessa, arun) = (&people[0], &people[1]);
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    let square = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Square", &[arun.user.id]);

    // Leaving offers the copy, made before the member leaves.
    let copies = nessa.api.copy_my_clan_maps(c.id).await.unwrap();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].copied_from, grove);
    assert_eq!(copies[0].name, "Grove (copy)");
    nessa
        .api
        .remove_clan_member(c.id, nessa.user.id)
        .await
        .unwrap();
    let copy = row(nessa, copies[0].area_id).await.expect("in My maps");
    assert_eq!(copy.user_id, Some(nessa.user.id));
    assert_eq!(copy.clan_id, None);
    assert_eq!(copy.atlas_id, None);
    // Her ownership stays, dormant.
    assert_eq!(c.server.map_owners(grove), Some(vec![nessa.user.id]));

    // Removal makes the copy by itself.
    c.mira
        .api
        .remove_clan_member(c.id, arun.user.id)
        .await
        .unwrap();
    let arun_copy = arun
        .maps
        .list_areas()
        .await
        .unwrap()
        .into_iter()
        .find(|area| area.name == "Square (copy)")
        .expect("Arun's copy is in his maps");
    assert_eq!(arun_copy.user_id, Some(arun.user.id));
    let _ = square;
}

/// The copies of `original` in `owner`'s own maps.
fn copies_of(server: &MockHandle, owner: Uuid, original: AreaId) -> Vec<Uuid> {
    let st = server.state.lock();
    st.areas
        .values()
        .filter(|area| area.user_id == owner && area.copied_from_area_id == Some(original.0))
        .map(|area| area.id)
        .collect()
}

/// Where the exit leaving `room` of `area` toward `direction` leads.
fn exit_to(server: &MockHandle, area: Uuid, room: i32, direction: &str) -> Option<(Uuid, i32)> {
    let st = server.state.lock();
    let exit = st.areas[&area]
        .exits
        .iter()
        .find(|exit| exit.from_room_number == room && exit.from_direction == direction)
        .expect("the exit was copied");
    exit.to_area_id.zip(exit.to_room_number)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaving_copies_are_made_once_per_membership() {
    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);

    let first = nessa.api.copy_my_clan_maps(c.id).await.unwrap();
    assert_eq!(first.len(), 1);
    let again = nessa.api.copy_my_clan_maps(c.id).await.unwrap();
    assert!(again.is_empty(), "a repeat lists only the copies it made");
    assert_eq!(
        copies_of(&c.server, nessa.user.id, grove),
        [first[0].area_id.0]
    );

    // A new membership copies afresh.
    nessa
        .api
        .remove_clan_member(c.id, nessa.user.id)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    c.server.join_clan(c.id, &nessa.user);
    let rejoined = nessa.api.copy_my_clan_maps(c.id).await.unwrap();
    assert_eq!(rejoined.len(), 1);
    assert_ne!(rejoined[0].area_id, first[0].area_id);
    assert_eq!(copies_of(&c.server, nessa.user.id, grove).len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_copies_keep_exits_among_the_owners_maps_and_into_what_they_read() {
    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    let glade = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Glade", &[nessa.user.id]);
    let den = c.server.create_area(&nessa.user, "Den");
    let square = c.server.create_clan_area(c.id, c.roads.0, "Square");
    for area in [grove, glade, den, square] {
        c.server.add_room(area, 1, "One");
        c.server.add_room(area, 2, "Two");
    }
    c.server.add_exit(grove, 1, "north", Some((glade, 2)));
    c.server.add_exit(glade, 2, "south", Some((grove, 1)));
    c.server.add_exit(grove, 1, "east", Some((den, 1)));
    c.server.add_exit(grove, 1, "west", Some((square, 1)));

    // A removal judges Nessa as she reads after it: her own Den, not the
    // clan's Square; her two maps' copies lead into each other.
    c.mira
        .api
        .remove_clan_member(c.id, nessa.user.id)
        .await
        .unwrap();
    let grove_copy = copies_of(&c.server, nessa.user.id, grove)[0];
    let glade_copy = copies_of(&c.server, nessa.user.id, glade)[0];
    assert_eq!(
        exit_to(&c.server, grove_copy, 1, "north"),
        Some((glade_copy, 2))
    );
    assert_eq!(
        exit_to(&c.server, glade_copy, 2, "south"),
        Some((grove_copy, 1))
    );
    assert_eq!(exit_to(&c.server, grove_copy, 1, "east"), Some((den.0, 1)));
    assert_eq!(exit_to(&c.server, grove_copy, 1, "west"), None, "dangles");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dissolution_copies_every_map_before_deleting_any() {
    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    let glade = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Glade", &[nessa.user.id]);
    for area in [grove, glade] {
        c.server.add_room(area, 1, "One");
    }
    c.server.add_exit(grove, 1, "north", Some((glade, 1)));
    c.server.add_exit(glade, 1, "south", Some((grove, 1)));

    c.mira.api.delete_clan(c.id).await.expect("dissolve");
    let grove_copy = copies_of(&c.server, nessa.user.id, grove)[0];
    let glade_copy = copies_of(&c.server, nessa.user.id, glade)[0];
    assert_eq!(
        exit_to(&c.server, grove_copy, 1, "north"),
        Some((glade_copy, 1))
    );
    assert_eq!(
        exit_to(&c.server, glade_copy, 1, "south"),
        Some((grove_copy, 1))
    );
    assert!(row(nessa, grove).await.is_none(), "the originals went");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_being_deleted_takes_no_copy() {
    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    c.server.interrupt_next_account_deletions(1);
    assert!(nessa.api.delete_account().await.is_err(), "left partway");
    c.mira
        .api
        .remove_clan_member(c.id, nessa.user.id)
        .await
        .unwrap();
    assert!(copies_of(&c.server, nessa.user.id, grove).is_empty());
}

/// One envelope of `payload` on `area`'s map source, as `user`; the status.
async fn write_map(
    server: &MockHandle,
    user: &TestUser,
    area: AreaId,
    payload: serde_json::Value,
) -> u16 {
    let envelope = serde_json::json!({
        "operation_id": Uuid::new_v4(),
        "source": "map",
        "preconditions": [{
            "resource": "source", "id": area, "source": "map",
            "expected_rev": server.area_rev(area),
        }],
        "payload": payload,
    });
    reqwest::Client::new()
        .post(format!("{}/areas/{area}/mutations", server.base_url))
        .header("authorization", format!("Bearer {}", user.api_key))
        .json(&envelope)
        .send()
        .await
        .expect("request sends")
        .status()
        .as_u16()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_owned_maps_exits_move_no_map_they_lead_into() {
    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    c.server.clan_grant(
        c.id,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.roads.0].into()),
        &["area.add", "area.edit", "area.remove_content"],
    );
    let square = c.server.create_clan_area(c.id, c.roads.0, "Square");
    c.server.add_room(square, 1, "Fountain");
    c.server.add_room(square, 5, "Gate");
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    c.server.add_room(grove, 1, "Clearing");
    let before = c.server.area_rev(square);

    // Creating, changing and deleting an exit into Square moves nothing
    // there: Square's readers do not all read Grove.
    let exit = Uuid::new_v4();
    let create = serde_json::json!([{ "op": "create_exit", "room_number": 1, "body": {
        "id": exit, "from_direction": "North", "to_area_id": square, "to_room_number": 5,
        "to_direction": null, "is_hidden": false, "weight": 1.0,
    }}]);
    assert_eq!(write_map(&c.server, &nessa.user, grove, create).await, 200);
    let retarget = serde_json::json!([{ "op": "update_exit", "exit_id": exit,
        "body": { "to_area_id": square, "to_room_number": 1 } }]);
    assert_eq!(
        write_map(&c.server, &nessa.user, grove, retarget).await,
        200
    );
    let delete = serde_json::json!([{ "op": "delete_exit", "exit_id": exit }]);
    assert_eq!(write_map(&c.server, &nessa.user, grove, delete).await, 200);
    assert_eq!(c.server.area_rev(square), before, "no target bump");

    // A room Square does not hold is the uniform 404, created or
    // retargeted: a Member-owned map's exits make no placeholder.
    let placeholder = serde_json::json!([{ "op": "create_exit", "room_number": 1, "body": {
        "id": Uuid::new_v4(), "from_direction": "East", "to_area_id": square,
        "to_room_number": 9, "to_direction": null, "is_hidden": false, "weight": 1.0,
    }}]);
    assert_eq!(
        write_map(&c.server, &nessa.user, grove, placeholder).await,
        404
    );
    let kept = Uuid::new_v4();
    let create = serde_json::json!([{ "op": "create_exit", "room_number": 1, "body": {
        "id": kept, "from_direction": "South", "to_area_id": square, "to_room_number": 5,
        "to_direction": null, "is_hidden": false, "weight": 1.0,
    }}]);
    assert_eq!(write_map(&c.server, &nessa.user, grove, create).await, 200);
    let retarget = serde_json::json!([{ "op": "update_exit", "exit_id": kept,
        "body": { "to_area_id": square, "to_room_number": 9 } }]);
    assert_eq!(
        write_map(&c.server, &nessa.user, grove, retarget).await,
        404
    );
    assert_eq!(c.server.area_rev(square), before, "no placeholder");

    // A merge check passes over exits from a Member-owned map the writer
    // does not read, and counts them for one who does.
    c.server.add_exit(grove, 1, "West", Some((square, 5)));
    let merge = serde_json::json!([{
        "op": "assert_merge_safe", "keep_room_number": 1, "remove_room_number": 5,
    }]);
    assert_eq!(
        write_map(&c.server, &c.mira.user, square, merge.clone()).await,
        200
    );
    assert_eq!(write_map(&c.server, &nessa.user, square, merge).await, 409);

    // Deleting Grove moves nothing in Square either.
    let standing = c.server.area_rev(square);
    nessa
        .maps
        .delete_area(&grove)
        .await
        .expect("its owner deletes it");
    assert_eq!(c.server.area_rev(square), standing);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outside_shares_read_a_clan_owned_map_only() {
    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    let friend = member(&c.server, "friend");
    c.server.befriend(&c.mira.user, &friend.user);
    c.server.befriend(&nessa.user, &friend.user);
    let roads = c
        .mira
        .api
        .create_clan_area(c.id, c.roads, "Roads", MapOwnership::Clan)
        .await
        .unwrap()
        .id;

    // Without area.share_external, a member cannot share it outside.
    assert!(is_not_found(
        &nessa.api.share_outside(roads, friend.user.id).await
    ));
    assert!(is_not_found(&nessa.api.outside_shares(roads).await));
    let share = c
        .mira
        .api
        .share_outside(roads, friend.user.id)
        .await
        .expect("a clan owner shares it");
    assert!(!share.can_edit && !share.can_copy);
    let listed = c.mira.api.outside_shares(roads).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].grantor_nickname.as_deref(), Some("mira"));
    assert_eq!(listed[0].grantee_nickname.as_deref(), Some("friend"));

    // The friend reads it, view only, and nothing more.
    let seen = row(&friend, roads).await.expect("the friend reads it");
    assert_eq!(seen.clan_id, Some(c.id));
    assert!(!seen.effective_access().can_edit);
    assert!(!seen.effective_access().can_copy);

    // A Member-owned map is never shared outside.
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    assert!(is_not_found(
        &nessa.api.share_outside(grove, friend.user.id).await
    ));

    // Revoked by a clan owner, it ends.
    c.mira.api.revoke_share(share.id).await.unwrap();
    assert!(row(&friend, roads).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_outside_share_ends_for_good_when_its_sharer_loses_share_external() {
    use smudgy_cloud::clan_access::GrantBody;

    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    let friend = member(&c.server, "friend");
    c.server.befriend(&nessa.user, &friend.user);
    let roads = c
        .mira
        .api
        .create_clan_area(c.id, c.roads, "Roads", MapOwnership::Clan)
        .await
        .unwrap()
        .id;
    let scouts = c
        .mira
        .api
        .create_clan_group(c.id, "Scouts", None)
        .await
        .unwrap()
        .id;
    c.mira
        .api
        .add_clan_group_member(c.id, scouts, nessa.user.id)
        .await
        .unwrap();
    let grant = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            GrantRecipient::Group { group_id: scouts },
            &GrantScope::Atlases { ids: vec![c.roads] },
            &GrantBody::of(["area.share_external"]),
        )
        .await
        .unwrap();

    // Leaving the group that gave the action ends it; rejoining does not
    // bring it back.
    nessa
        .api
        .share_outside(roads, friend.user.id)
        .await
        .expect("Nessa shares it outside");
    assert!(row(&friend, roads).await.is_some());
    c.mira
        .api
        .remove_clan_group_member(c.id, scouts, nessa.user.id)
        .await
        .unwrap();
    assert!(row(&friend, roads).await.is_none());
    c.mira
        .api
        .add_clan_group_member(c.id, scouts, nessa.user.id)
        .await
        .unwrap();
    assert!(row(&friend, roads).await.is_none(), "it ended for good");
    assert!(c.mira.api.outside_shares(roads).await.unwrap().is_empty());

    // A grant change that drops the action ends it, and giving the action
    // back does not bring it back.
    nessa
        .api
        .share_outside(roads, friend.user.id)
        .await
        .expect("shared again");
    c.mira
        .api
        .change_clan_grant(c.id, grant.id, &GrantBody::of(["atlas.read"]))
        .await
        .unwrap();
    c.mira
        .api
        .change_clan_grant(c.id, grant.id, &GrantBody::of(["area.share_external"]))
        .await
        .unwrap();
    assert!(row(&friend, roads).await.is_none());
    assert!(c.mira.api.outside_shares(roads).await.unwrap().is_empty());

    // So does the grant's deletion.
    nessa
        .api
        .share_outside(roads, friend.user.id)
        .await
        .expect("shared again");
    c.mira.api.delete_clan_grant(c.id, grant.id).await.unwrap();
    assert!(row(&friend, roads).await.is_none());

    // And a demotion, for an owner who held it through ownership.
    c.mira
        .api
        .set_clan_owner(c.id, nessa.user.id, true)
        .await
        .unwrap();
    nessa
        .api
        .share_outside(roads, friend.user.id)
        .await
        .expect("an owner shares it");
    assert!(row(&friend, roads).await.is_some());
    c.mira
        .api
        .set_clan_owner(c.id, nessa.user.id, false)
        .await
        .unwrap();
    c.mira
        .api
        .set_clan_owner(c.id, nessa.user.id, true)
        .await
        .unwrap();
    assert!(row(&friend, roads).await.is_none());
    assert!(c.mira.api.outside_shares(roads).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_map_whose_last_owner_is_deleted_freezes() {
    let (c, people) = clan(&["nessa", "arun"]).await;
    let (nessa, arun) = (&people[0], &people[1]);
    let map = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    nessa
        .api
        .create_clan_grant(
            c.id,
            GrantRecipient::User {
                user_id: arun.user.id,
            },
            &GrantScope::Areas { ids: vec![map] },
            &[action::EDIT_AREA],
        )
        .await
        .unwrap();
    nessa
        .api
        .delete_account()
        .await
        .expect("Nessa leaves for good");

    // Its grants keep working; nobody can change who sees it, and the
    // clan's owners still do not see it.
    let seen = row(arun, map).await.expect("Arun still reads it");
    assert!(seen.clan_ownership.frozen);
    assert!(seen.can(action::EDIT_AREA));
    assert_eq!(c.server.map_owners(map), Some(Vec::new()));
    assert!(row(&c.mira, map).await.is_none());
    let grants = arun
        .api
        .clan_grants(
            c.id,
            ClanGrantFilter {
                area_id: Some(map),
                ..ClanGrantFilter::default()
            },
        )
        .await
        .unwrap();
    assert!(is_not_found(
        &arun
            .api
            .update_clan_grant(c.id, grants[0].id, &[action::READ_AREA])
            .await
    ));
}

/// `method` on `path` with `body`, as `user`: the status, the error code
/// (empty on success) and the data.
async fn send(
    server: &MockHandle,
    user: &TestUser,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, String, serde_json::Value) {
    let mut request = reqwest::Client::new()
        .request(method, format!("{}{path}", server.base_url))
        .header("authorization", format!("Bearer {}", user.api_key));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("request sends");
    let status = response.status().as_u16();
    let answer: serde_json::Value = response.json().await.unwrap_or_default();
    let code = answer["error"].as_str().unwrap_or_default().to_string();
    (status, code, answer["data"].clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dissolving_clans_owners_and_member_owned_maps_change_no_more() {
    use reqwest::Method;

    let (c, people) = clan(&["nessa", "arun", "bo"]).await;
    let (nessa, arun, bo) = (&people[0], &people[1], &people[2]);
    c.mira
        .api
        .set_clan_owner(c.id, nessa.user.id, true)
        .await
        .unwrap();
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.user.id]);
    c.server.add_room(grove, 1, "Clearing");
    let (status, _, secret) = send(
        &c.server,
        &nessa.user,
        Method::POST,
        &format!("/areas/{grove}/secrets"),
        Some(serde_json::json!({ "name": "Cache", "ownership": "members" })),
    )
    .await;
    assert_eq!(status, 201);
    let secret = secret["source"]
        .as_str()
        .expect("the Secret's ID")
        .to_string();
    nessa
        .api
        .create_clan_grant(
            c.id,
            GrantRecipient::User {
                user_id: arun.user.id,
            },
            &GrantScope::Areas { ids: vec![grove] },
            &[action::READ_AREA],
        )
        .await
        .unwrap();
    let offer = nessa
        .api
        .offer_area_ownership(grove, &[arun.user.id], MapOwnership::Members, false)
        .await
        .expect("Nessa offers Arun a share of Grove's ownership");

    // A dissolution that stops partway leaves the clan dissolving.
    c.server.interrupt_next_dissolutions(1);
    assert!(c.mira.api.delete_clan(c.id).await.is_err());

    // Its owners stay its owners.
    let demoted = c.mira.api.set_clan_owner(c.id, nessa.user.id, false).await;
    assert!(
        matches!(demoted, Err(CloudError::ClanDissolving)),
        "{demoted:?}"
    );
    let removed = c.mira.api.remove_clan_member(c.id, nessa.user.id).await;
    assert!(
        matches!(removed, Err(CloudError::ClanDissolving)),
        "{removed:?}"
    );
    let left = nessa.api.remove_clan_member(c.id, nessa.user.id).await;
    assert!(matches!(left, Err(CloudError::ClanDissolving)), "{left:?}");
    // A promotion, and a member who owns nothing leaving, still go through.
    c.mira
        .api
        .set_clan_owner(c.id, arun.user.id, true)
        .await
        .expect("promotions stay allowed");
    bo.api
        .remove_clan_member(c.id, bo.user.id)
        .await
        .expect("a member leaves");

    // Its Member-owned maps stay as their copies take them.
    let rev = c.server.area_rev(grove);
    let writes = [
        (
            Method::POST,
            format!("/areas/{grove}/mutations"),
            Some(serde_json::json!({
                "operation_id": Uuid::new_v4(),
                "source": "map",
                "preconditions": [{
                    "resource": "source", "id": grove, "source": "map", "expected_rev": rev,
                }],
                "payload": [{ "op": "upsert_room_property", "room_number": 1,
                    "name": "terrain", "value": "forest" }],
            })),
        ),
        (
            Method::POST,
            format!("/areas/{grove}/moves"),
            Some(serde_json::json!({
                "operation_id": Uuid::new_v4(),
                "from": "map",
                "to": secret,
                "preconditions": [
                    { "resource": "source", "id": grove, "source": "map", "expected_rev": rev },
                    { "resource": "source", "id": grove, "source": secret, "expected_rev": 0 },
                ],
                "rooms": [1],
            })),
        ),
        (
            Method::PUT,
            format!("/areas/{grove}"),
            Some(serde_json::json!({ "name": "Thicket" })),
        ),
        (
            Method::POST,
            format!("/areas/{grove}/secrets"),
            Some(serde_json::json!({ "name": "Stash", "ownership": "members" })),
        ),
        (
            Method::PATCH,
            format!("/secrets/{secret}"),
            Some(serde_json::json!({ "name": "Hoard" })),
        ),
        (Method::DELETE, format!("/secrets/{secret}"), None),
        (Method::DELETE, format!("/areas/{grove}"), None),
    ];
    for (method, path, body) in writes {
        let (status, code, _) = send(&c.server, &nessa.user, method.clone(), &path, body).await;
        assert_eq!(
            (status, code.as_str()),
            (409, "clan_dissolving"),
            "{method} {path}"
        );
    }
    let accepted = arun.api.accept_area_offer(grove, offer.id, None).await;
    assert!(
        matches!(accepted, Err(CloudError::ClanDissolving)),
        "{accepted:?}"
    );
    // A request that changes nothing has nothing to refuse.
    let (status, _, _) = send(
        &c.server,
        &nessa.user,
        Method::PUT,
        &format!("/areas/{grove}"),
        Some(serde_json::json!({ "name": "Grove" })),
    )
    .await;
    assert_eq!(status, 200);
    // Each request's own checks come first.
    let (status, _, _) = send(
        &c.server,
        &arun.user,
        Method::DELETE,
        &format!("/areas/{grove}"),
        None,
    )
    .await;
    assert_eq!(status, 404);

    // The owner repeating the request finishes it, with Nessa's copy of the
    // map as it stood.
    c.mira
        .api
        .delete_clan(c.id)
        .await
        .expect("dissolution finishes");
    let copies = copies_of(&c.server, nessa.user.id, grove);
    assert_eq!(copies.len(), 1);
    let copy = row(nessa, AreaId(copies[0])).await.expect("in My maps");
    assert_eq!(copy.name, "Grove (copy)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_leave_keeps_when_the_membership_began() {
    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0];
    let hall = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Hall", &[c.mira.user.id]);

    let first = c.mira.api.copy_my_clan_maps(c.id).await.unwrap();
    assert_eq!(first.len(), 1);
    // The last owner's leave is refused, and her membership goes on from
    // when it began: the copies it made are still the ones it makes.
    let refused = c.mira.api.remove_clan_member(c.id, c.mira.user.id).await;
    assert!(matches!(refused, Err(CloudError::LastOwner)), "{refused:?}");
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let again = c.mira.api.copy_my_clan_maps(c.id).await.unwrap();
    assert!(again.is_empty(), "{again:?}");

    // So does one refused while the clan dissolves.
    c.mira
        .api
        .set_clan_owner(c.id, nessa.user.id, true)
        .await
        .unwrap();
    c.server.interrupt_next_dissolutions(1);
    assert!(c.mira.api.delete_clan(c.id).await.is_err());
    let refused = c.mira.api.remove_clan_member(c.id, c.mira.user.id).await;
    assert!(
        matches!(refused, Err(CloudError::ClanDissolving)),
        "{refused:?}"
    );
    let again = c.mira.api.copy_my_clan_maps(c.id).await.unwrap();
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(
        copies_of(&c.server, c.mira.user.id, hall),
        [first[0].area_id.0]
    );
}

/// A queued write into a Member-owned map of a clan being dissolved is
/// refused `clan_dissolving` and parks for Retry or Discard, carrying that
/// refusal so the editor says why in the viewer's language.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_refused_by_a_dissolving_clan_parks_with_its_refusal() {
    use smudgy_cloud::mapper::AreaSaveStatus;
    use smudgy_cloud::mutation::AreaMutation;
    use smudgy_cloud::{CachedCloudMapper, Mapper, RoomNumber};

    let (c, people) = clan(&["nessa"]).await;
    let nessa = &people[0].user;
    let grove = c
        .server
        .create_member_owned_area(c.id, c.roads.0, "Grove", &[nessa.id]);
    c.server.add_room(grove, 1, "Clearing");

    let dir = std::env::temp_dir().join(format!("dissolving-write-{}", Uuid::new_v4()));
    let credentials = CredentialSource::new(Some(Credential::ApiKey(nessa.api_key.clone())));
    let cloud = CachedCloudMapper::new(
        CloudMapper::with_credentials(c.server.base_url.clone(), credentials),
        dir.join("cloud-cache"),
    );
    let mapper = Mapper::new(std::sync::Arc::new(cloud), dir.join("cache"));
    mapper.load_all_areas().await.expect("loads");
    assert!(mapper.get_current_atlas().get_area(&grove).is_some());

    // The dissolution stops partway: the clan stays dissolving.
    c.server.interrupt_next_dissolutions(1);
    assert!(c.mira.api.delete_clan(c.id).await.is_err());

    let submission = mapper
        .mutate_area(
            grove,
            vec![AreaMutation::UpsertRoomProperty {
                room_source: None,
                room_number: RoomNumber(1),
                name: "terrain".into(),
                value: "forest".into(),
            }],
            "Set terrain",
        )
        .expect("queued");
    let operation = submission.operation_id().expect("an operation");
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        mapper.wait_for_mutation(operation),
    )
    .await
    .expect("answered");
    assert!(outcome.is_err(), "refused: {outcome:?}");
    assert!(matches!(
        mapper.area_save_status(grove),
        AreaSaveStatus::CouldNotSave { .. }
    ));
    assert!(matches!(
        mapper.parked_error(grove),
        Some(CloudError::ClanDissolving)
    ));
    assert!(mapper.is_operation_pending(grove, operation), "kept parked");
}
