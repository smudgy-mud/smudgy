//! Copies and moves carry a map's places (its Secrets and the caller's
//! Private additions) into cloud storage over the real client stack, on
//! both the service's copy route and the replay.

use super::*;
use smudgy_cloud::{CompositeBackend, LocalBackend, RelocationMode, SourceBundle, SourceId};
use support::SecretPlace;

/// The real client stack over local storage and the mock's cloud.
async fn composite_mapper(base_url: &str, api_key: &str, dir: &Path) -> Mapper {
    let credentials = CredentialSource::new(Some(Credential::ApiKey(api_key.to_string())));
    let cloud = CachedCloudMapper::new(
        CloudMapper::with_credentials(base_url.to_string(), credentials),
        dir.join("cloud-cache"),
    );
    let backend = CompositeBackend::new(
        Arc::new(LocalBackend::new(dir.join("local"))),
        Arc::new(cloud),
    );
    let mapper = Mapper::new(Arc::new(backend), dir.join("cache"));
    mapper.load_all_areas().await.expect("loads");
    // Loading documents and activating the cloud identity run independently.
    // Relocations that replay edits require the startup sync to have settled.
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    mapper
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_local_secrets_upload_as_private_and_are_never_shared() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("legacy-owner@example.com", "legacy-owner", true);
    let reader = server.create_user("legacy-reader@example.com", "legacy-reader", true);
    let dir = TempCacheDir::new("legacy-private-upload");
    let id = AreaId(Uuid::new_v4());
    let areas = dir.path().join("local").join("areas-v2");
    std::fs::create_dir_all(&areas).unwrap();
    let room = |number, private| {
        serde_json::json!({
            "room_number": number, "title": format!("room {number}"), "description": "",
            "x": 0, "y": 0, "level": 0, "color": "", "is_secret": private,
            "properties": [{"name": "private-note", "value": "keep private", "is_secret": true}],
            "exits": [], "tags": []
        })
    };
    let legacy = serde_json::json!({
        "id": id, "user_id": null, "atlas_id": null, "name": "Legacy map",
        "created_at": "2026-08-01T00:00:00Z", "rev": 3, "format_version": 2,
        "properties": [{"name": "private-map-note", "value": "keep private", "is_secret": true}],
        "rooms": [room(1, false), room(2, true)], "labels": [], "shapes": [], "connections": []
    });
    std::fs::write(
        areas.join(format!("{id}.json")),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let mapper = composite_mapper(&server.base_url, &owner.api_key, dir.path()).await;
    let uploaded = mapper
        .relocate_areas(
            vec![id],
            MapDestination::loose(MapStorage::Cloud),
            RelocationMode::Copy,
        )
        .await
        .expect("upload preserves Private")
        .destination_ids[0];
    let owner_view = CloudMapper::new(server.base_url.clone(), owner.api_key.clone())
        .get_area(&uploaded)
        .await
        .unwrap();
    let private = owner_view
        .sources
        .iter()
        .find(|source| source.source == SourceId::Private)
        .unwrap();
    assert_eq!(private.rooms[0].room_number, RoomNumber(2));
    assert_eq!(private.properties[0].name, "private-map-note");
    assert_eq!(private.room_data[0].properties[0].name, "private-note");
    server.grant(
        &owner,
        &reader,
        GrantScope::Area(uploaded),
        GrantFlags::VIEW_ONLY,
    );
    let reader_view = CloudMapper::new(server.base_url.clone(), reader.api_key.clone())
        .get_area(&uploaded)
        .await
        .unwrap();
    assert!(reader_view.sources.is_empty());
    assert_eq!(reader_view.rooms.len(), 1);
    assert_eq!(reader_view.rooms[0].room_number, RoomNumber(1));
    assert!(reader_view.properties.is_empty() && reader_view.rooms[0].properties.is_empty());
    assert!(
        mapper.get_current_atlas().get_area(&id).is_some(),
        "copy retains original"
    );
}

fn secrets(mapper: &Mapper, area: AreaId) -> Vec<SourceBundle> {
    mapper
        .get_current_atlas()
        .get_area(&area)
        .map(|area| {
            area.meta()
                .sources
                .iter()
                .filter(|bundle| bundle.source.is_secret())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The owner's cloud map "Keep": map room 1, and a Secret "Bookcase" with
/// its own room 2 and a link from map room 1 into it.
fn keep_with_a_secret(server: &support::MockHandle, owner: &support::TestUser) -> AreaId {
    let keep = server.create_area(owner, "Keep");
    server.add_room(keep, 1, "Library");
    let bookcase = server.add_secret(keep, "Bookcase");
    server.add_secret_room(keep, bookcase, 2, "Vault");
    server.add_secret_exit(
        keep,
        bookcase,
        SecretPlace::Map(1),
        "East",
        Some(SecretPlace::Own(2)),
    );
    keep
}

fn assert_carries_the_bookcase(mapper: &Mapper, area: AreaId) {
    let secrets = secrets(mapper, area);
    assert_eq!(secrets.len(), 1, "one Secret: {secrets:?}");
    let bookcase = &secrets[0];
    assert_eq!(bookcase.name.as_deref(), Some("Bookcase"));
    assert_eq!(bookcase.ownership.as_deref(), Some("owner"));
    assert!(
        bookcase
            .rooms
            .iter()
            .any(|room| room.room_number == RoomNumber(2) && room.title == "Vault"),
        "its room: {bookcase:?}"
    );
    assert!(
        bookcase.room_data.iter().any(|data| {
            data.room_number == RoomNumber(1)
                && data
                    .exits
                    .iter()
                    .any(|exit| exit.to_room_number == Some(RoomNumber(2)))
        }),
        "its link from the map's room: {bookcase:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_to_local_never_deletes_an_uncopied_secret_edit() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("safe-move@example.com", "mover", true);
    let area = keep_with_a_secret(&server, &owner);
    let dir = TempCacheDir::new("guarded-local-move");
    let mapper = composite_mapper(&server.base_url, &owner.api_key, dir.path()).await;
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    let SourceId::Secret(secret) = secrets(&mapper, area)[0].source else {
        panic!("Secret")
    };
    // Use the wire mutation: the fixture's add_secret_room is a seed helper
    // and deliberately does not advance the Secret revision.
    CloudMapper::new(server.base_url.clone(), owner.api_key.clone())
        .execute_mutation(
            &area,
            &MutationEnvelope {
                operation_id: Uuid::new_v4(),
                source: SourceId::Secret(secret),
                preconditions: vec![Precondition::source(
                    area.0,
                    SourceId::Secret(secret),
                    server.secret_rev(area, secret),
                )],
                payload: vec![AreaMutation::UpsertRoom {
                    room_number: RoomNumber(3),
                    room_source: Some(SourceId::Secret(secret)),
                    body: RoomUpdates {
                        title: Some("New work on another device".into()),
                        ..RoomUpdates::default()
                    },
                }],
            },
        )
        .await
        .expect("second device writes the Secret");
    let moved = mapper
        .relocate_areas(
            vec![area],
            MapDestination::loose(MapStorage::Local),
            RelocationMode::Move,
        )
        .await;
    match moved {
        Ok(done) => assert!(
            secrets(&mapper, done.destination_ids[0])
                .iter()
                .flat_map(|source| &source.rooms)
                .any(|room| room.room_number == RoomNumber(3))
        ),
        Err(failure) => {
            assert!(
                matches!(failure.error, CloudError::RevisionConflict { .. }),
                "{failure:?}"
            );
            assert!(
                failure.completed.is_some(),
                "the existing local copy is reported"
            );
            let live = CloudMapper::new(server.base_url.clone(), owner.api_key.clone())
                .get_area(&area)
                .await
                .expect("original preserved");
            assert!(
                live.sources
                    .iter()
                    .flat_map(|source| &source.rooms)
                    .any(|room| room.room_number == RoomNumber(3))
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_a_shared_personal_map_requires_its_review() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("shared-move@example.com", "owner", true);
    let reader = server.create_user("shared-reader@example.com", "reader", true);
    let area = keep_with_a_secret(&server, &owner);
    server.befriend(&owner, &reader);
    server.grant(
        &owner,
        &reader,
        GrantScope::Area(area),
        GrantFlags::default(),
    );
    let dir = TempCacheDir::new("shared-local-move");
    let mapper = composite_mapper(&server.base_url, &owner.api_key, dir.path()).await;
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    let dest = MapDestination::loose(MapStorage::Local);
    let refusal = mapper
        .relocate_areas(vec![area], dest, RelocationMode::Move)
        .await
        .expect_err("confirmation required");
    assert!(refusal.completed.is_none());
    let reviews = mapper.review_local_moves(&[area]).await.expect("review");
    assert!(reviews[0].has_shares);
    let second_reader = server.create_user("new-reader@example.com", "new reader", true);
    server.befriend(&owner, &second_reader);
    server.grant(
        &owner,
        &second_reader,
        GrantScope::Area(area),
        GrantFlags::default(),
    );
    let stale = mapper
        .relocate_areas_reviewed(vec![area], dest, RelocationMode::Move, &reviews)
        .await
        .expect_err("new shares need a fresh review");
    assert!(stale.completed.is_none() && stale.partial.is_none());
    assert_eq!(
        mapper.get_current_atlas().areas().count(),
        1,
        "no copy before review"
    );
    let reviews = mapper
        .review_local_moves(&[area])
        .await
        .expect("fresh review");
    let moved = mapper
        .relocate_areas_reviewed(vec![area], dest, RelocationMode::Move, &reviews)
        .await
        .expect("confirmed move");
    assert_carries_the_bookcase(&mapper, moved.destination_ids[0]);
    assert!(matches!(
        CloudMapper::new(server.base_url.clone(), owner.api_key.clone())
            .get_area(&area)
            .await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_atlas_moves_only_after_review_and_finishes_through_the_empty_guard() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("atlas-move@example.com", "owner", true);
    let reader = server.create_user("atlas-reader@example.com", "reader", true);
    let folder = server.create_atlas(&owner, "Shared atlas");
    let area = server.create_area_in_atlas(&owner, "Keep", folder);
    server.add_room(area, 1, "Gate");
    server.befriend(&owner, &reader);
    server.grant(
        &owner,
        &reader,
        GrantScope::Atlas(folder),
        GrantFlags::default(),
    );
    let cloud = CloudMapper::new(server.base_url.clone(), owner.api_key.clone());
    assert!(matches!(
        cloud.finish_local_atlas_move(&AtlasId(folder), 0).await,
        Err(CloudError::AtlasNotEmpty)
    ));
    let dir = TempCacheDir::new("shared-atlas-move");
    let mapper = composite_mapper(&server.base_url, &owner.api_key, dir.path()).await;
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    let denied = mapper
        .relocate_atlas(AtlasId(folder), MapStorage::Local, RelocationMode::Move)
        .await
        .expect_err("shared map needs review");
    assert!(denied.completed.is_none());
    let reviews = mapper.review_local_moves(&[area]).await.unwrap();
    let moved = mapper
        .relocate_atlas_reviewed(
            AtlasId(folder),
            MapStorage::Local,
            RelocationMode::Move,
            &reviews,
        )
        .await
        .expect("reviewed atlas move");
    assert_eq!(
        mapper.atlas_storage(&moved.destination_atlas_id),
        Some(MapStorage::Local)
    );
    assert!(mapper.get_current_atlas().get_area(&area).is_none());
    assert!(
        cloud
            .list_atlases()
            .await
            .unwrap()
            .iter()
            .all(|atlas| atlas.id != AtlasId(folder))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_cloud_deletion_keeps_both_the_source_and_the_reported_local_copy() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("failed-move@example.com", "owner", true);
    let area = keep_with_a_secret(&server, &owner);
    let dir = TempCacheDir::new("failed-local-move");
    let mapper = composite_mapper(&server.base_url, &owner.api_key, dir.path()).await;
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    server.fail_next_area_deletes(1);
    let failure = mapper
        .relocate_areas(
            vec![area],
            MapDestination::loose(MapStorage::Local),
            RelocationMode::Move,
        )
        .await
        .expect_err("injected deletion failure");
    let completed = failure.completed.expect("report the durable copy");
    assert_carries_the_bookcase(&mapper, completed.destination_ids[0]);
    assert_carries_the_bookcase(&mapper, area);
    CloudMapper::new(server.base_url.clone(), owner.api_key.clone())
        .get_area(&area)
        .await
        .expect("source still on the server");
}

/// A local copy of a cloud map holds the map's Secret; moving that copy to
/// the cloud brings the Secret along, content and link, before the local
/// map goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_maps_secret_survives_a_move_to_the_cloud() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("mover@example.com", "mover", true);
    let keep = keep_with_a_secret(&server, &owner);
    let dir = TempCacheDir::new("move-places");
    let mapper = composite_mapper(&server.base_url, &owner.api_key, dir.path()).await;

    let local = mapper
        .relocate_areas(
            vec![keep],
            MapDestination::loose(MapStorage::Local),
            RelocationMode::Copy,
        )
        .await
        .expect("copies to local storage")
        .destination_ids[0];
    assert_carries_the_bookcase(&mapper, local);

    let moved = mapper
        .relocate_areas(
            vec![local],
            MapDestination::loose(MapStorage::Cloud),
            RelocationMode::Move,
        )
        .await
        .expect("moves to the cloud")
        .destination_ids[0];
    assert!(mapper.get_current_atlas().get_area(&local).is_none());
    assert_eq!(mapper.area_storage(&moved), MapStorage::Cloud);
    assert_carries_the_bookcase(&mapper, moved);
    let secret = secrets(&mapper, moved)[0].source;
    let SourceId::Secret(id) = secret else {
        panic!("a Secret");
    };
    assert!(
        server.secret_rev(moved, id) > 0,
        "the service holds the moved map's Secret"
    );
}

/// A cloud map with a link into another map takes the service's copy,
/// which carries the Secret the owner holds `copy` on and keeps the link
/// into the map the owner reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cloud_copy_with_a_link_out_takes_the_services_copy_with_its_secret() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("copier@example.com", "copier", true);
    let keep = keep_with_a_secret(&server, &owner);
    let road = server.create_area(&owner, "Road");
    server.add_room(road, 5, "Gate");
    server.add_exit(keep, 1, "West", Some((road, 5)));
    let dir = TempCacheDir::new("server-copy-places");
    let mapper = composite_mapper(&server.base_url, &owner.api_key, dir.path()).await;

    let copy = mapper
        .relocate_areas(
            vec![keep],
            MapDestination::loose(MapStorage::Cloud),
            RelocationMode::Copy,
        )
        .await
        .expect("copies")
        .destination_ids[0];
    let atlas = mapper.get_current_atlas();
    let copied = atlas.get_area(&copy).expect("the copy is loaded");
    assert_eq!(
        copied.meta().copied_from_area_id,
        Some(keep),
        "the service made the copy"
    );
    let room = copied.get_room(&RoomNumber(1)).expect("the map's room");
    assert!(
        room.get_exits()
            .iter()
            .any(|exit| exit.to_area_id == Some(road) && exit.to_room_number == Some(RoomNumber(5))),
        "the link into the readable map stays"
    );
    drop(atlas);
    assert_carries_the_bookcase(&mapper, copy);
}

/// Two linked cloud maps copied together take the replay, which leads the
/// link into the other copy; the copies still carry the Secret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replayed_cloud_copy_carries_the_secrets_the_copier_may_copy() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("pair@example.com", "pair", true);
    let keep = keep_with_a_secret(&server, &owner);
    let road = server.create_area(&owner, "Road");
    server.add_room(road, 5, "Gate");
    server.add_exit(keep, 1, "West", Some((road, 5)));
    let dir = TempCacheDir::new("replay-places");
    let mapper = composite_mapper(&server.base_url, &owner.api_key, dir.path()).await;

    let copies = mapper
        .relocate_areas(
            vec![keep, road],
            MapDestination::loose(MapStorage::Cloud),
            RelocationMode::Copy,
        )
        .await
        .expect("copies")
        .destination_ids;
    let (keep_copy, road_copy) = (copies[0], copies[1]);
    let atlas = mapper.get_current_atlas();
    let copied = atlas.get_area(&keep_copy).expect("the copy is loaded");
    assert_eq!(
        copied.meta().copied_from_area_id,
        None,
        "the replay made the copy"
    );
    let room = copied.get_room(&RoomNumber(1)).expect("the map's room");
    assert!(
        room.get_exits()
            .iter()
            .any(|exit| exit.to_area_id == Some(road_copy)),
        "the link leads into the other copy"
    );
    drop(atlas);
    assert_carries_the_bookcase(&mapper, keep_copy);
    assert_carries_the_bookcase(&mapper, keep);
}
