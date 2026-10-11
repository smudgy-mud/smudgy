//! Clan folders and maps and their grants to groups, end to end over real
//! HTTP against the contract-shaped mock in
//! `tests/support/{clans,clan_maps}.rs`.
#![allow(clippy::too_many_lines)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use smudgy_cloud::clan_access::{GrantBody, GrantChange};
use smudgy_cloud::clan_maps::MapOwnership;
use smudgy_cloud::clans::{
    ClanGrantFilter, GrantRecipient, GrantScope, MAP_CREATOR_ACTIONS, action,
};
use smudgy_cloud::{
    Area, AreaId, AreaUpdates, AtlasId, AtlasListItem, CachedCloudMapper, CloudApiClient,
    CloudError, CloudMapper, CreateAreaRequest, Credential, CredentialSource, MapDestination,
    MapStorage, Mapper, MapperBackend,
};
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

async fn folders(member: &Member) -> Vec<AtlasListItem> {
    member.maps.list_atlases().await.expect("list folders")
}

async fn maps(member: &Member) -> Vec<Area> {
    member.maps.list_areas().await.expect("list maps")
}

async fn find_map(member: &Member, area: AreaId) -> Option<Area> {
    maps(member).await.into_iter().find(|row| row.id == area)
}

fn holds(row: &Area, actions: &[&str]) -> bool {
    actions.iter().all(|wanted| row.can(wanted))
}

/// The built-in group of every member.
async fn everyone(owner: &Member, clan: Uuid) -> Uuid {
    owner
        .api
        .clan_groups(clan)
        .await
        .expect("groups")
        .into_iter()
        .find(|group| group.builtin.as_deref() == Some("members"))
        .expect("built-in members group")
        .id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clan_folders_and_maps_follow_the_clans_grants() {
    let server = MockServer::spawn().await;
    let mira = member(&server, "mira");
    let tomas = member(&server, "tomas");
    let outsider = member(&server, "outsider");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    server.join_clan(clan, &tomas.user);

    // Only a holder of atlas.create makes folders.
    let roads = mira.api.create_clan_atlas(clan, "Roads").await.unwrap();
    assert_eq!(roads.clan_id, Some(clan));
    assert_eq!(roads.user_id, None);
    assert!(is_not_found(
        &tomas.api.create_clan_atlas(clan, "Mine").await
    ));
    assert!(is_not_found(
        &outsider.api.create_clan_atlas(clan, "Mine").await
    ));

    // A clan map is created in one of the clan's folders, never loose and
    // never in someone's own folder.
    let solace = mira
        .api
        .create_clan_area(clan, roads.id, "Solace", MapOwnership::Clan)
        .await
        .unwrap();
    assert_eq!(solace.clan_id, Some(clan));
    assert_eq!(solace.user_id, None);
    assert_eq!(solace.atlas_id, Some(roads.id));
    let loose = mira
        .maps
        .create_area(CreateAreaRequest {
            name: "Loose".to_string(),
            atlas_id: None,
            clan_id: Some(clan),
            ownership: None,
            ephemeral: false,
            properties: std::collections::BTreeMap::new(),
        })
        .await;
    assert!(
        matches!(loose, Err(CloudError::InvalidInput(_))),
        "{loose:?}"
    );
    let own = mira.maps.create_atlas("Mine").await.unwrap();
    assert!(is_not_found(
        &mira
            .api
            .create_clan_area(clan, own.id, "Elsewhere", MapOwnership::Clan)
            .await
    ));
    assert!(is_not_found(
        &tomas
            .api
            .create_clan_area(clan, roads.id, "Mine", MapOwnership::Clan)
            .await
    ));

    // An owner sees every folder and map, with every action.
    let listed = folders(&mira).await;
    let folder = listed
        .iter()
        .find(|item| item.id == roads.id)
        .expect("clan folder listed");
    assert_eq!(folder.clan_name.as_deref(), Some("Lantern Company"));
    assert!(!folder.is_owner);
    assert_eq!(folder.area_count, 1);
    for held in [
        action::RENAME_ATLAS,
        action::DELETE_ATLAS,
        action::ACCEPT_FILING,
        action::CREATE_AREA,
    ] {
        assert!(folder.can(held), "an owner holds {held}");
    }
    let row = find_map(&mira, solace.id).await.expect("clan map listed");
    assert_eq!(row.clan_id, Some(clan));
    assert_eq!(row.user_id, None);
    assert_eq!(row.clan_name.as_deref(), Some("Lantern Company"));
    assert!(holds(
        &row,
        &[action::READ_AREA, action::RENAME_AREA, action::DELETE_AREA]
    ));
    let access = row.effective_access();
    assert!(!access.is_owner && access.can_edit && access.can_copy);

    // A member reads nothing until a grant reaches them.
    assert!(!folders(&tomas).await.iter().any(|item| item.id == roads.id));
    assert!(find_map(&tomas, solace.id).await.is_none());
    assert!(is_not_found(&tomas.maps.get_area(&solace.id).await));

    // A Read grant to everyone on the folder.
    let members = everyone(&mira, clan).await;
    let grant = mira
        .api
        .grant_in_clan(
            clan,
            GrantRecipient::Group { group_id: members },
            &GrantScope::Atlases {
                ids: vec![roads.id],
            },
            &GrantBody::of([action::READ_AREA]),
        )
        .await
        .unwrap();
    assert_eq!(grant.actions, [action::READ_AREA.to_string()].into());
    let seen = folders(&tomas).await;
    let folder = seen.iter().find(|item| item.id == roads.id).unwrap();
    assert_eq!(folder.area_count, 1);
    assert!(folder.actions.is_empty());
    let row = find_map(&tomas, solace.id).await.expect("readable now");
    assert!(row.can(action::READ_AREA));
    assert!(!row.can(action::EDIT_AREA));
    assert!(!row.effective_access().can_edit);
    let projection = tomas.maps.get_area(&solace.id).await.unwrap();
    assert_eq!(projection.area.clan_id, Some(clan));
    assert!(
        projection
            .area
            .actions
            .as_ref()
            .is_some_and(|held| held.contains(action::READ_AREA))
    );

    // Edit joins Read.
    let edit = GrantChange {
        add: MAP_CREATOR_ACTIONS
            .iter()
            .map(ToString::to_string)
            .collect(),
        ..GrantChange::default()
    };
    let changed = mira
        .api
        .change_clan_grant(clan, grant.id, &edit)
        .await
        .unwrap()
        .expect("the grant stays");
    assert_eq!(changed.id, grant.id);
    let row = find_map(&tomas, solace.id).await.unwrap();
    assert!(row.effective_access().can_edit);
    assert!(!row.can(action::DELETE_AREA));

    // The folder's grants are listed to the people they reach.
    let filter = ClanGrantFilter {
        atlas_id: Some(roads.id),
        ..ClanGrantFilter::default()
    };
    let listed = tomas.api.clan_grants(clan, filter).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].recipient.group(), Some(members));
    assert!(is_not_found(
        &tomas
            .api
            .grant_in_clan(
                clan,
                GrantRecipient::Group { group_id: members },
                &GrantScope::Atlases {
                    ids: vec![roads.id]
                },
                &GrantBody::of([action::READ_AREA]),
            )
            .await
    ));

    // Map access goes to groups, never to one member.
    let to_member = mira
        .api
        .grant_in_clan(
            clan,
            GrantRecipient::User {
                user_id: tomas.user.id,
            },
            &GrantScope::Atlases {
                ids: vec![roads.id],
            },
            &GrantBody::of([action::READ_AREA]),
        )
        .await;
    assert!(
        matches!(to_member, Err(CloudError::InvalidInput(_))),
        "{to_member:?}"
    );

    // Clan-wide resource grants are rejected; new folders are not implicitly shared.
    mira.api.delete_clan_grant(clan, grant.id).await.unwrap();
    assert!(find_map(&tomas, solace.id).await.is_none());
    assert!(matches!(
        mira.api
            .grant_in_clan(
                clan,
                GrantRecipient::Group { group_id: members },
                &GrantScope::Clan,
                &GrantBody::of([action::READ_AREA])
            )
            .await,
        Err(CloudError::InvalidInput(_))
    ));
    let towns = mira.api.create_clan_atlas(clan, "Towns").await.unwrap();
    let later = mira
        .api
        .create_clan_area(clan, towns.id, "Haven", MapOwnership::Clan)
        .await
        .unwrap();
    assert!(find_map(&tomas, later.id).await.is_none());
    mira.api
        .grant_in_clan(
            clan,
            GrantRecipient::Group { group_id: members },
            &GrantScope::Atlases {
                ids: vec![roads.id, towns.id],
            },
            &GrantBody::of([action::READ_AREA]),
        )
        .await
        .unwrap();
    assert!(find_map(&tomas, later.id).await.is_some());

    // A clan map renames and moves between the clan's folders, never out.
    mira.maps
        .update_area(
            &solace.id,
            AreaUpdates {
                name: Some("Solace Vale".to_string()),
                atlas_id: None,
            },
        )
        .await
        .unwrap();
    let review = mira
        .maps
        .review_filing(&solace.id, Some(towns.id), 0)
        .await
        .unwrap();
    mira.maps
        .commit_reviewed_filing(&solace.id, Some(towns.id), &review.token, 0)
        .await
        .unwrap();
    let row = find_map(&tomas, solace.id).await.unwrap();
    assert_eq!(row.name, "Solace Vale");
    assert_eq!(row.atlas_id, Some(towns.id));
    let out = mira.maps.move_area_to_atlas(&solace.id, None).await;
    assert!(matches!(out, Err(CloudError::InvalidInput(_))), "{out:?}");
    assert!(is_not_found(
        &tomas
            .maps
            .update_area(
                &solace.id,
                AreaUpdates {
                    name: Some("Mine".to_string()),
                    atlas_id: None,
                },
            )
            .await
    ));

    // A folder holding the clan's maps stays; the clan does too.
    let error = mira.maps.delete_atlas(&towns.id).await.unwrap_err();
    assert!(matches!(error, CloudError::AtlasNotEmpty), "{error:?}");
    let error = mira.api.delete_clan(clan).await.unwrap_err();
    assert!(matches!(error, CloudError::ClanNotEmpty), "{error:?}");
    assert!(is_not_found(&tomas.maps.delete_area(&later.id).await));
    mira.maps.delete_area(&later.id).await.unwrap();
    mira.maps.delete_area(&solace.id).await.unwrap();
    mira.maps.delete_atlas(&towns.id).await.unwrap();
    assert!(!folders(&mira).await.iter().any(|item| item.id == towns.id));
}

// ---------------------------------------------------------------------------

struct TempCacheDir(PathBuf);

impl TempCacheDir {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("smudgy-int-clan-maps-{}", Uuid::new_v4())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempCacheDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..1000u32 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(condition(), "condition not met within timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mapper_makes_a_map_in_a_clan_folder_in_the_clan() {
    let server = MockServer::spawn().await;
    let mira = member(&server, "mira");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    let roads = mira.api.create_clan_atlas(clan, "Roads").await.unwrap();

    let cache = TempCacheDir::new();
    let credentials = CredentialSource::new(Some(Credential::ApiKey(mira.user.api_key.clone())));
    let backend = CachedCloudMapper::new(
        CloudMapper::with_credentials(server.base_url.clone(), credentials),
        cache.path(),
    );
    let mapper = Mapper::new(Arc::new(backend), cache.path());
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;

    // The inventory teaches the mapper which folders are the clan's.
    let listed = mapper.list_atlases().await.unwrap();
    assert!(listed.iter().any(|item| item.clan_id == Some(clan)));
    let area = mapper
        .create_area_at(
            "Solace".to_string(),
            MapDestination::in_atlas(MapStorage::Cloud, AtlasId(roads.id.0)),
        )
        .await
        .unwrap();
    {
        let st = server.state.lock();
        let record = st.areas.get(&area.0).expect("created on the server");
        assert_eq!(record.clan_id, Some(clan));
        assert_eq!(record.atlas_id, Some(roads.id.0));
    }

    // The mapper shows the map as the server serves it to the creator, not
    // as the bare create reply describes it: the clan's, never theirs.
    let row = find_map(&mira, area).await.expect("listed to its creator");
    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("published once created");
    assert!(!cached.is_owned(), "a clan's map is nobody's own");
    assert!(!cached.effective_access().is_owner);
    assert_eq!(cached.meta().clan_id, Some(clan));
    assert!(
        row.actions
            .as_ref()
            .is_some_and(|actions| !actions.is_empty())
    );
    assert_eq!(cached.meta().actions, row.actions);
    assert_eq!(cached.meta().access, row.access);
    assert_eq!(cached.meta().atlas_name.as_deref(), Some("Roads"));
    assert!(cached.meta().projection_token.is_some());
}

/// A new clan map assumes its creator's access, Editor, until the server's
/// copy arrives: it is never shown read-only or as the creator's own. Here
/// the server's read right after the create fails, so the map shows as the
/// creator's Editor copy; the next sync replaces it with what the server
/// says the creator may do (here, read only, once the clan's owner removed
/// the Editor grant the creator started with).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_clan_map_assumes_its_creators_access_until_the_servers_copy() {
    let server = MockServer::spawn().await;
    let mira = member(&server, "mira");
    let tomas = member(&server, "tomas");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    server.join_clan(clan, &tomas.user);
    let roads = mira.api.create_clan_atlas(clan, "Roads").await.unwrap();
    let members = everyone(&mira, clan).await;
    mira.api
        .grant_in_clan(
            clan,
            GrantRecipient::Group { group_id: members },
            &GrantScope::Atlases {
                ids: vec![roads.id],
            },
            &GrantBody::of([action::READ_AREA, action::CREATE_AREA]),
        )
        .await
        .unwrap();

    let cache = TempCacheDir::new();
    let credentials = CredentialSource::new(Some(Credential::ApiKey(tomas.user.api_key.clone())));
    let backend = CachedCloudMapper::new(
        CloudMapper::with_credentials(server.base_url.clone(), credentials),
        cache.path(),
    );
    let mapper = Mapper::new(Arc::new(backend), cache.path());
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    mapper.list_atlases().await.unwrap();

    // Reads fail until the copy is checked, the sync the failure asks for
    // included.
    server.fail_next_area_reads(u32::MAX);
    let area = mapper
        .create_area_at(
            "Wayside".to_string(),
            MapDestination::in_atlas(MapStorage::Cloud, AtlasId(roads.id.0)),
        )
        .await
        .unwrap();
    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("published once created");
    let access = cached.effective_access();
    assert!(access.can_edit, "the creator starts as Editor");
    assert!(!access.is_owner && !cached.is_owned(), "nobody owns it");
    assert!(!access.can_admin && !access.can_reshare && !access.can_copy);
    assert_eq!(cached.meta().clan_id, Some(clan));
    let editor: std::collections::BTreeSet<String> = MAP_CREATOR_ACTIONS
        .iter()
        .map(|held| (*held).to_string())
        .collect();
    assert_eq!(cached.meta().actions.as_ref(), Some(&editor));

    // The server's copy replaces it: here the clan's owner took the
    // creator's Editor grant away meanwhile, so it reads only.
    server.fail_next_area_reads(0);
    let creator_grants = mira
        .api
        .clan_grants(
            clan,
            ClanGrantFilter {
                user_id: Some(tomas.user.id),
                area_id: Some(area),
                ..ClanGrantFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(creator_grants.len(), 1, "the creator starts with a grant");
    assert_eq!(
        creator_grants[0].scope,
        GrantScope::Areas { ids: vec![area] }
    );
    mira.api
        .delete_clan_grant(clan, creator_grants[0].id)
        .await
        .unwrap();
    let row = find_map(&tomas, area).await.expect("listed to its creator");
    assert!(!holds(&row, &[action::EDIT_AREA]), "{:?}", row.actions);
    mapper.sync_now();
    wait_until(|| {
        mapper
            .get_current_atlas()
            .get_area(&area)
            .is_some_and(|cached| cached.meta().actions == row.actions)
    })
    .await;
    let cached = mapper.get_current_atlas().get_area(&area).unwrap();
    assert!(!cached.effective_access().can_edit, "as the server says");
    assert!(!cached.is_owned());
    assert!(cached.meta().projection_token.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mapper_makes_a_member_owned_map_its_creator_owns() {
    let server = MockServer::spawn().await;
    let mira = member(&server, "mira");
    let tomas = member(&server, "tomas");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    server.join_clan(clan, &tomas.user);
    let roads = mira.api.create_clan_atlas(clan, "Roads").await.unwrap();
    let everyone = everyone(&mira, clan).await;
    mira.api
        .grant_in_clan(
            clan,
            GrantRecipient::Group { group_id: everyone },
            &GrantScope::Atlases {
                ids: vec![roads.id],
            },
            &GrantBody::of([action::READ_AREA, action::CREATE_MEMBER_OWNED_AREA]),
        )
        .await
        .unwrap();

    let cache = TempCacheDir::new();
    let credentials = CredentialSource::new(Some(Credential::ApiKey(tomas.user.api_key.clone())));
    let backend = CachedCloudMapper::new(
        CloudMapper::with_credentials(server.base_url.clone(), credentials),
        cache.path(),
    );
    let mapper = Mapper::new(Arc::new(backend), cache.path());
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    mapper.list_atlases().await.unwrap();

    let area = mapper
        .create_clan_area_at(
            "Grove".to_string(),
            MapDestination::in_atlas(MapStorage::Cloud, roads.id),
            MapOwnership::Members,
        )
        .await
        .expect("Tomas may make Member-owned maps in Roads");
    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("published once created");
    assert!(cached.meta().clan_ownership.member_owned());
    assert!(cached.meta().clan_ownership.owned_by_me);
    assert_eq!(server.map_owners(area), Some(vec![tomas.user.id]));
    // Its clan's owner does not see it.
    assert!(find_map(&mira, area).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_map_enters_a_clan_while_it_dissolves() {
    let server = MockServer::spawn().await;
    let mira = member(&server, "mira");
    let tomas = member(&server, "tomas");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    server.join_clan(clan, &tomas.user);
    let roads = mira.api.create_clan_atlas(clan, "Roads").await.unwrap();

    // Tomas offers the clan his Member-owned map, and one of his own.
    let kept = server.create_member_owned_area(clan, roads.id.0, "Kept", &[tomas.user.id]);
    let offer = tomas
        .api
        .offer_area_ownership(kept, &[], MapOwnership::Clan, false)
        .await
        .expect("offer to the clan");
    let den = server.create_area(&tomas.user, "Den");
    let transfer = server
        .legacy_clan_transfer(&tomas.user, Some(den), None, clan, MapOwnership::Clan)
        .await
        .expect("transfer offer");

    // A dissolution that stops partway leaves the clan dissolving.
    server.interrupt_next_dissolutions(1);
    assert!(mira.api.delete_clan(clan).await.is_err());

    let created = mira
        .api
        .create_clan_area(clan, roads.id, "Late", MapOwnership::Clan)
        .await;
    assert!(
        matches!(created, Err(CloudError::ClanDissolving)),
        "{created:?}"
    );
    let accepted = mira
        .api
        .accept_transfer(transfer.id, None, Some(roads.id))
        .await;
    assert!(
        matches!(accepted, Err(CloudError::ClanDissolving)),
        "{accepted:?}"
    );
    let made_clan_owned = mira
        .api
        .accept_area_offer(kept, offer.id, Some(roads.id))
        .await;
    assert!(
        matches!(made_clan_owned, Err(CloudError::ClanDissolving)),
        "{made_clan_owned:?}"
    );
    // Each request's own checks come first.
    assert!(is_not_found(
        &tomas
            .api
            .create_clan_area(clan, roads.id, "Mine", MapOwnership::Clan)
            .await
    ));

    // The owner repeating the request finishes it; the map stayed out.
    mira.api
        .delete_clan(clan)
        .await
        .expect("dissolution finishes");
    assert!(
        maps(&tomas).await.iter().any(|row| row.id == den),
        "the offered map is still Tomas's own"
    );
}
