//! Owner Secrets and moves over the real client stack. Each call resolves
//! once the server has accepted it and the map is republished, so the
//! atlas cache shows the result as soon as the call returns.

use super::*;
use smudgy_cloud::cloud_api::{SecretChange, SecretColorChange, SecretGrantChange};
use smudgy_cloud::mapper::AreaMutationBatch;
use smudgy_cloud::mutation::RoomRenumbering;
use smudgy_cloud::{LabelArgs, LabelId, LabelUpdates, MovedContent, SourceBundle, SourceId};

/// The bundle a published map carries for `source`.
fn bundle(mapper: &Mapper, area: AreaId, source: &SourceId) -> Option<SourceBundle> {
    mapper
        .get_current_atlas()
        .get_area(&area)?
        .meta()
        .sources
        .iter()
        .find(|bundle| &bundle.source == source)
        .cloned()
}

fn map_rooms(mapper: &Mapper, area: AreaId) -> Vec<i32> {
    let atlas = mapper.get_current_atlas();
    let mut rooms: Vec<i32> = atlas
        .get_area(&area)
        .expect("the map is loaded")
        .get_rooms()
        .iter()
        .map(|room| room.get_room_number().0)
        .collect();
    rooms.sort_unstable();
    rooms
}

/// A map with a lone room 1, and rooms 2 and 3 joined by a two-way pair.
async fn corridor(
    server: &support::MockHandle,
    owner: &support::TestUser,
    mapper: &Mapper,
) -> AreaId {
    let area = server.create_area(owner, "Corridor");
    for (number, title) in [(1, "Hall"), (2, "Study"), (3, "Vault")] {
        server.add_room(area, number, title);
    }
    tick(mapper).await;
    for (from, to, out, back) in [(2, 3, ExitDirection::East, ExitDirection::West)] {
        mapper
            .create_exit(
                RoomKey::new(area, RoomNumber(from)),
                ExitArgs {
                    from_direction: out,
                    to_area_id: Some(area),
                    to_room_number: Some(RoomNumber(to)),
                    to_direction: Some(back),
                    ..ExitArgs::default()
                },
            )
            .await
            .expect("exit out");
        mapper
            .create_exit(
                RoomKey::new(area, RoomNumber(to)),
                ExitArgs {
                    from_direction: back,
                    to_area_id: Some(area),
                    to_room_number: Some(RoomNumber(from)),
                    to_direction: Some(out),
                    ..ExitArgs::default()
                },
            )
            .await
            .expect("exit back");
    }
    assert!(
        mapper
            .wait_for_sync_completion(10)
            .await
            .expect("exits saved")
    );
    area
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_is_created_renamed_and_deleted() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("secrets@example.com", "secrets", true);
    let area = server.create_area(&owner, "Library");
    let cache = TempCacheDir::new("secret-crud");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;

    let secret = mapper
        .create_secret(area, "Behind The Bookcase", None)
        .await
        .expect("create");
    assert_eq!(secret.name, "Behind The Bookcase");
    assert_eq!(secret.ownership, "owner");
    let created = bundle(&mapper, area, &secret.source).expect("published at once");
    assert_eq!(created.name.as_deref(), Some("Behind The Bookcase"));
    assert_eq!(created.rev, 1);
    assert!(created.can("add"));

    let renamed = mapper
        .rename_secret(area, &secret.source, "Under The Stairs")
        .await
        .expect("rename");
    assert_eq!(renamed.name, "Under The Stairs");
    let published = bundle(&mapper, area, &secret.source).expect("still there");
    assert_eq!(published.name.as_deref(), Some("Under The Stairs"));
    assert_eq!(published.rev, 2, "a rename moves the Secret's revision");

    let listed = api_client(&server.base_url, &owner.api_key)
        .area_secrets(area)
        .await
        .expect("listing");
    assert_eq!(listed, vec![renamed]);

    mapper
        .delete_secret(area, &secret.source)
        .await
        .expect("delete");
    assert!(bundle(&mapper, area, &secret.source).is_none());
    assert!(server.state.lock().areas[&area.0].secrets.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_takes_a_color_and_gives_it_back_to_the_palette() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("colors@example.com", "colors", true);
    let area = server.create_area(&owner, "Library");
    let cache = TempCacheDir::new("secret-color");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let secret = mapper
        .create_secret(area, "Bookcase", None)
        .await
        .expect("create");
    assert_eq!(
        secret.color, None,
        "a new Secret leaves its color to the palette"
    );

    let colored = mapper
        .recolor_secret(area, &secret.source, Some("#3A7BD5"))
        .await
        .expect("recolor");
    assert_eq!(colored.color.as_deref(), Some("#3a7bd5"));
    assert_eq!(colored.name, "Bookcase", "the name stays");
    let published = bundle(&mapper, area, &secret.source).expect("published at once");
    assert_eq!(published.rgb(), Some([0x3a, 0x7b, 0xd5]));
    assert_eq!(
        published.rev, 2,
        "a color change moves the Secret's revision"
    );
    let layer_color = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("loaded")
        .source_layers()
        .iter()
        .find(|layer| layer.source() == secret.source)
        .and_then(smudgy_cloud::mapper::area_cache::SourceLayer::color);
    assert_eq!(layer_color, Some([0x3a, 0x7b, 0xd5]));

    mapper
        .recolor_secret(area, &secret.source, Some("#3a7bd5"))
        .await
        .expect("same color");
    assert_eq!(
        bundle(&mapper, area, &secret.source).expect("there").rev,
        2,
        "the same color changes nothing"
    );

    let cleared = mapper
        .recolor_secret(area, &secret.source, None)
        .await
        .expect("clear");
    assert_eq!(cleared.color, None);
    let published = bundle(&mapper, area, &secret.source).expect("there");
    assert_eq!(published.color, None);
    assert_eq!(published.rev, 3);

    let refused = mapper
        .recolor_secret(area, &secret.source, Some("blue"))
        .await;
    assert!(refused.is_err(), "{refused:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_is_created_in_a_color_and_updated_in_one_request() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("onecall@example.com", "onecall", true);
    let area = server.create_area(&owner, "Library");
    let cache = TempCacheDir::new("secret-one-call");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;

    let secret = mapper
        .create_secret(area, "Bookcase", Some("#3A7BD5"))
        .await
        .expect("create");
    assert_eq!(secret.color.as_deref(), Some("#3a7bd5"));
    let created = bundle(&mapper, area, &secret.source).expect("published at once");
    assert_eq!(created.rev, 1, "the color came with the create");
    assert_eq!(created.rgb(), Some([0x3a, 0x7b, 0xd5]));

    let updated = mapper
        .update_secret(
            area,
            &secret.source,
            &SecretChange {
                name: Some("Vault".to_string()),
                color: SecretColorChange::Set("#8A5CF6".to_string()),
            },
        )
        .await
        .expect("update");
    assert_eq!(
        (updated.name.as_str(), updated.color.as_deref()),
        ("Vault", Some("#8a5cf6"))
    );
    let published = bundle(&mapper, area, &secret.source).expect("still there");
    assert_eq!(published.name.as_deref(), Some("Vault"));
    assert_eq!(published.rev, 2, "one request moves the revision once");

    let cleared = mapper
        .update_secret(
            area,
            &secret.source,
            &SecretChange {
                color: SecretColorChange::Clear,
                ..SecretChange::default()
            },
        )
        .await
        .expect("clear the color");
    assert_eq!((cleared.name.as_str(), cleared.color), ("Vault", None));

    let empty = mapper
        .update_secret(area, &secret.source, &SecretChange::default())
        .await;
    assert!(
        matches!(empty, Err(CloudError::InvalidInput(_))),
        "{empty:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_owner_administers_secrets() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("keeper@example.com", "keeper", true);
    let guest = server.create_user("guest@example.com", "guest", true);
    let area = server.create_area(&owner, "Shared Keep");
    server.grant(&owner, &guest, GrantScope::Area(area), GrantFlags::edit());
    let guest_cache = TempCacheDir::new("secret-guest");
    let guest_mapper =
        new_synced_mapper(&server.base_url, &guest.api_key, guest_cache.path()).await;

    let refused = guest_mapper.create_secret(area, "Mine", None).await;
    assert!(
        matches!(refused, Err(CloudError::NotFoundOrNoAccess)),
        "{refused:?}"
    );
}

/// A Secret shared through the mapper reaches the grantee's synced map with
/// the granted actions, and leaves it when the grant is revoked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_shared_through_the_mapper_reaches_the_grantee() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("sharer@example.com", "sharer", true);
    let friend = server.create_user("tomas@example.com", "tomas", true);
    server.befriend(&owner, &friend);
    let area = server.create_area(&owner, "DV");
    server.grant(
        &owner,
        &friend,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );
    let owner_cache = TempCacheDir::new("secret-share-owner");
    let owner_mapper =
        new_synced_mapper(&server.base_url, &owner.api_key, owner_cache.path()).await;
    let friend_cache = TempCacheDir::new("secret-share-friend");
    let friend_mapper =
        new_synced_mapper(&server.base_url, &friend.api_key, friend_cache.path()).await;

    let secret = owner_mapper
        .create_secret(area, "Bookcase", None)
        .await
        .expect("create")
        .source;
    let grant = owner_mapper
        .grant_secret(area, &secret, friend.id, &["edit", "remove"])
        .await
        .expect("share the Secret");
    let listed = owner_mapper
        .secret_grants(area, &secret)
        .await
        .expect("list the grants");
    assert_eq!(listed.iter().map(|g| g.id).collect::<Vec<_>>(), [grant.id]);

    tick(&friend_mapper).await;
    let shared = bundle(&friend_mapper, area, &secret).expect("the Secret reaches the friend");
    assert!(shared.can("edit") && shared.can("remove") && !shared.can("add"));
    assert!(!shared.can("manage_access"));

    owner_mapper
        .update_secret_grant(
            area,
            &secret,
            grant.id,
            &SecretGrantChange {
                add: ["add".to_string()].into(),
                remove: ["edit".to_string(), "remove".to_string()].into(),
            },
        )
        .await
        .expect("change the grant");
    tick(&friend_mapper).await;
    let changed = bundle(&friend_mapper, area, &secret).expect("still shared");
    assert!(changed.can("add") && !changed.can("edit"));

    owner_mapper
        .revoke_secret_grant(area, &secret, grant.id)
        .await
        .expect("revoke the grant");
    tick(&friend_mapper).await;
    assert!(bundle(&friend_mapper, area, &secret).is_none());

    // The map and Private have no grants.
    assert!(matches!(
        owner_mapper.secret_grants(area, &SourceId::map()).await,
        Err(CloudError::InvalidInput(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rooms_move_into_a_secret_with_their_links_and_back() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("mover@example.com", "mover", true);
    let cache = TempCacheDir::new("secret-move");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let area = corridor(&server, &owner, &mapper).await;
    let label = server.add_label(area, "X marks the spot");
    tick(&mapper).await;
    let secret = mapper
        .create_secret(area, "Hidden", None)
        .await
        .expect("create")
        .source;
    let map_rev = mapper
        .get_current_atlas()
        .get_area(&area)
        .unwrap()
        .get_rev();
    let remaps = mapper.subscribe_room_remaps();
    let secret_area = match secret {
        SourceId::Secret(id) => AreaId(id),
        _ => panic!("a Secret's source"),
    };
    let moved_rooms =
        |remaps: Vec<smudgy_cloud::mapper::MapperEvent>| -> Vec<(AreaId, Vec<RoomKey>)> {
            remaps
                .into_iter()
                .filter_map(|event| match event {
                    smudgy_cloud::mapper::MapperEvent::AreasMerged {
                        into,
                        deleted,
                        rooms,
                    } => {
                        assert!(deleted.is_empty(), "a move deletes no area");
                        assert!(rooms.iter().all(|moved| moved.to == moved.from.room_number));
                        Some((into, rooms.into_iter().map(|moved| moved.from).collect()))
                    }
                    _ => None,
                })
                .collect()
        };

    let moved = mapper
        .move_content(
            area,
            SourceId::map(),
            secret.clone(),
            MovedContent {
                rooms: vec![RoomNumber(2), RoomNumber(3)],
                labels: vec![smudgy_cloud::LabelId(label)],
                ..MovedContent::default()
            },
        )
        .await
        .expect("move");
    let revisions: Vec<(SourceId, i64)> = moved
        .versions
        .iter()
        .map(|version| (version.source.clone(), version.rev))
        .collect();
    assert_eq!(
        revisions,
        vec![(SourceId::map(), map_rev + 1), (secret.clone(), 2)]
    );

    assert_eq!(map_rooms(&mapper, area), vec![1]);
    assert_eq!(
        moved_rooms(remaps.take()),
        vec![(
            secret_area,
            vec![
                RoomKey::new(area, RoomNumber(2)),
                RoomKey::new(area, RoomNumber(3))
            ]
        )],
        "handles on the moved rooms follow them into the Secret's area"
    );
    let hidden = bundle(&mapper, area, &secret).expect("the Secret is published");
    assert_eq!(hidden.rev, 2);
    let numbers: Vec<i32> = hidden.rooms.iter().map(|room| room.room_number.0).collect();
    assert_eq!(numbers, vec![2, 3]);
    let exit = &hidden.rooms[0].exits[0];
    assert_eq!(exit.to_room_number, Some(RoomNumber(3)));
    assert_eq!(exit.to_source.as_ref(), Some(&secret));
    assert_eq!(
        hidden.connections.len(),
        1,
        "the pair's connection moved with it"
    );
    assert_eq!(hidden.labels.len(), 1);
    assert!(
        mapper
            .get_current_atlas()
            .get_area(&area)
            .unwrap()
            .get_labels()
            .is_empty()
    );

    // The map's next edit stands on the revision the move produced.
    let edit = mapper
        .upsert_room(
            RoomKey::new(area, RoomNumber(1)),
            RoomUpdates {
                title: Some("Grand Hall".into()),
                ..RoomUpdates::default()
            },
        )
        .unwrap();
    mapper
        .wait_for_mutation(edit.operation_id().unwrap())
        .await
        .expect("the edit after a move is not a conflict");

    // One end can move alone; the link retains its qualified other endpoint.
    let split = mapper
        .move_content(
            area,
            secret.clone(),
            SourceId::map(),
            MovedContent {
                rooms: vec![RoomNumber(3)],
                ..MovedContent::default()
            },
        )
        .await;
    split.expect("one endpoint can move alone");
    assert_eq!(bundle(&mapper, area, &secret).unwrap().rooms.len(), 1);
    assert_eq!(map_rooms(&mapper, area), vec![1, 3]);
    assert_eq!(
        moved_rooms(remaps.take()),
        vec![(area, vec![RoomKey::new(secret_area, RoomNumber(3))])]
    );

    mapper
        .move_content(
            area,
            secret.clone(),
            SourceId::map(),
            MovedContent {
                rooms: vec![RoomNumber(2)],
                ..MovedContent::default()
            },
        )
        .await
        .expect("moving the other endpoint back");
    assert_eq!(map_rooms(&mapper, area), vec![1, 2, 3]);
    assert_eq!(
        moved_rooms(remaps.take()),
        vec![(area, vec![RoomKey::new(secret_area, RoomNumber(2))])]
    );
    assert!(bundle(&mapper, area, &secret).unwrap().rooms.is_empty());
    let exported = mapper.export_area(area).await.expect("export");
    assert_eq!(exported.connections.len(), 1);
}

/// Each handle-holder notice a move sent, as `(destination area, [(room as
/// it was, number it took)])`.
fn remapped(events: Vec<smudgy_cloud::mapper::MapperEvent>) -> Vec<(AreaId, Vec<(RoomKey, i32)>)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            smudgy_cloud::mapper::MapperEvent::AreasMerged { into, rooms, .. } => Some((
                into,
                rooms
                    .into_iter()
                    .map(|moved| (moved.from, moved.to.0))
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

fn renumbering(from: i32, to: i32) -> RoomRenumbering {
    RoomRenumbering {
        from: RoomNumber(from),
        to: RoomNumber(to),
    }
}

fn titled(title: &str) -> RoomUpdates {
    RoomUpdates {
        title: Some(title.to_string()),
        ..RoomUpdates::default()
    }
}

/// Creates map room `number` through the mapper, as the editor and scripts
/// do, and waits for the server to take it.
async fn create_map_room(mapper: &Mapper, area: AreaId, number: i32, title: &str) {
    let created = mapper
        .create_room(RoomKey::new(area, RoomNumber(number)), titled(title))
        .expect("queued");
    mapper
        .wait_for_mutation(created.operation_id().expect("a cloud write"))
        .await
        .expect("the server takes the room");
}

/// Each place numbers its own rooms: once room 2 has moved into the Secret,
/// the map's next room is 2 again, and the server takes a map room 2 beside
/// the Secret's own. Moving that one in as well finds 2 taken, so it lands
/// as the Secret's room 3 and the result says so. A stale revision is still
/// refused, and moves nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_renumbers_a_taken_number_and_refuses_a_stale_revision() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("stale@example.com", "stale", true);
    let area = server.create_area(&owner, "Annex");
    server.add_room(area, 1, "Annex");
    server.add_room(area, 2, "Closet");
    let cache = TempCacheDir::new("secret-move-refusals");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let secret = mapper
        .create_secret(area, "Hidden", None)
        .await
        .expect("create")
        .source;
    let one = |room| MovedContent {
        rooms: vec![RoomNumber(room)],
        ..MovedContent::default()
    };

    let kept = mapper
        .move_content(area, SourceId::map(), secret, one(2))
        .await
        .expect("room 2 moves");
    assert!(kept.renumbered.is_empty(), "the Secret had no room 2");
    assert_eq!(
        mapper.try_next_room_number(&area).expect("a number"),
        RoomNumber(2),
        "the map numbers its own rooms"
    );
    create_map_room(&mapper, area, 2, "A new room 2").await;
    let taken = mapper
        .move_content(area, SourceId::map(), secret, one(2))
        .await
        .expect("a taken number renumbers the room");
    assert_eq!(taken.renumbered, vec![renumbering(2, 3)]);
    let titles: Vec<(i32, String)> = bundle(&mapper, area, &secret)
        .expect("the Secret is published")
        .rooms
        .iter()
        .map(|room| (room.room_number.0, room.title.clone()))
        .collect();
    assert_eq!(
        titles,
        vec![(2, "Closet".to_string()), (3, "A new room 2".to_string())]
    );

    // Someone else's write to the Secret that this session has not seen.
    server.state.lock().areas.get_mut(&area.0).unwrap().secrets[0].rev += 1;
    let stale = mapper
        .move_content(area, SourceId::map(), secret, one(1))
        .await;
    assert!(
        matches!(stale, Err(CloudError::RevisionConflict { .. })),
        "{stale:?}"
    );
    {
        let st = server.state.lock();
        let stored = &st.areas[&area.0];
        assert_eq!(stored.rooms.keys().copied().collect::<Vec<_>>(), vec![1]);
        assert_eq!(
            stored.secrets[0].rooms.keys().copied().collect::<Vec<_>>(),
            vec![2, 3],
            "the refusal moved nothing"
        );
    }
    assert_eq!(
        bundle(&mapper, area, &secret).unwrap().rev,
        server.state.lock().areas[&area.0].secrets[0].rev,
        "the refusal republished the map as the server holds it"
    );

    // So the same move, asked again, stands on that.
    mapper
        .move_content(area, SourceId::map(), secret, one(1))
        .await
        .expect("the move asked again after a conflict");
    assert_eq!(
        server.state.lock().areas[&area.0].secrets[0]
            .rooms
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

/// Moves both ways, with and without a clash. A linked pair moves into the
/// Secret keeping its numbers; the map then numbers new rooms 2 and 3 of its
/// own. Moving the map's room 3 in finds 3 taken and lands as 4. Moving the
/// pair back, 3 first, keeps 3 (the map has none) and gives 2, which the map
/// has, the map's next number, 4; the pair's exits and link follow the new
/// number. Every move tells handle holders where each room landed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moves_renumber_rooms_whose_number_the_destination_uses_both_ways() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("renumber@example.com", "renumber", true);
    let cache = TempCacheDir::new("secret-move-renumber");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let area = corridor(&server, &owner, &mapper).await;
    let secret = mapper
        .create_secret(area, "Hidden", None)
        .await
        .expect("create")
        .source;
    let secret_area = match secret {
        SourceId::Secret(id) => AreaId(id),
        _ => panic!("a Secret's source"),
    };
    let remaps = mapper.subscribe_room_remaps();
    let rooms = |numbers: &[i32]| MovedContent {
        rooms: numbers.iter().copied().map(RoomNumber).collect(),
        ..MovedContent::default()
    };
    let secret_rooms = |mapper: &Mapper| -> Vec<(i32, String)> {
        bundle(mapper, area, &secret)
            .expect("the Secret is published")
            .rooms
            .iter()
            .map(|room| (room.room_number.0, room.title.clone()))
            .collect()
    };

    // Into the Secret, nothing taken.
    let moved = mapper
        .move_content(area, SourceId::map(), secret, rooms(&[2, 3]))
        .await
        .expect("the pair moves");
    assert!(moved.renumbered.is_empty());
    assert_eq!(
        remapped(remaps.take()),
        vec![(
            secret_area,
            vec![
                (RoomKey::new(area, RoomNumber(2)), 2),
                (RoomKey::new(area, RoomNumber(3)), 3)
            ]
        )]
    );

    // The map numbers its own rooms, beside the Secret's 2 and 3.
    for (number, title) in [(2, "Annex"), (3, "Cellar")] {
        assert_eq!(
            mapper.try_next_room_number(&area).expect("a number"),
            RoomNumber(number)
        );
        create_map_room(&mapper, area, number, title).await;
    }
    assert_eq!(
        mapper.try_next_room_number(&secret_area).expect("a number"),
        RoomNumber(4)
    );

    // Into the Secret, 3 taken.
    let moved = mapper
        .move_content(area, SourceId::map(), secret, rooms(&[3]))
        .await
        .expect("the Cellar moves");
    assert_eq!(moved.renumbered, vec![renumbering(3, 4)]);
    assert_eq!(
        remapped(remaps.take()),
        vec![(secret_area, vec![(RoomKey::new(area, RoomNumber(3)), 4)])]
    );
    assert_eq!(map_rooms(&mapper, area), vec![1, 2]);
    assert_eq!(
        secret_rooms(&mapper),
        vec![
            (2, "Study".to_string()),
            (3, "Vault".to_string()),
            (4, "Cellar".to_string())
        ]
    );

    // Back to the map, 3 free and 2 taken.
    let moved = mapper
        .move_content(area, secret, SourceId::map(), rooms(&[3, 2]))
        .await
        .expect("the pair moves back");
    assert_eq!(moved.renumbered, vec![renumbering(2, 4)]);
    assert_eq!(
        remapped(remaps.take()),
        vec![(
            area,
            vec![
                (RoomKey::new(secret_area, RoomNumber(3)), 3),
                (RoomKey::new(secret_area, RoomNumber(2)), 4)
            ]
        )]
    );
    assert_eq!(map_rooms(&mapper, area), vec![1, 2, 3, 4]);
    let atlas = mapper.get_current_atlas();
    let room = |number| {
        atlas
            .get_room(&RoomKey::new(area, RoomNumber(number)))
            .expect("a map room")
    };
    assert_eq!(room(4).get_title(), "Study");
    assert_eq!(room(3).get_title(), "Vault");
    assert_eq!(room(2).get_title(), "Annex");
    let leads_to = |number| {
        room(number)
            .get_exits()
            .iter()
            .map(|exit| (exit.to_area_id, exit.to_room_number))
            .collect::<Vec<_>>()
    };
    assert_eq!(leads_to(4), vec![(Some(area), Some(RoomNumber(3)))]);
    assert_eq!(leads_to(3), vec![(Some(area), Some(RoomNumber(4)))]);
    {
        let st = server.state.lock();
        let stored = &st.areas[&area.0];
        let link = stored
            .connections
            .iter()
            .find(|connection| connection.endpoint_b.is_some())
            .expect("the pair's link moved back");
        let mut ends = [
            link.endpoint_a.room_number,
            link.endpoint_b.as_ref().unwrap().room_number,
        ];
        ends.sort_unstable();
        assert_eq!(ends, [3, 4], "the link follows the new number");
        assert!(stored.secrets[0].connections.is_empty());
    }

    // Back to the map, 4 taken.
    let moved = mapper
        .move_content(area, secret, SourceId::map(), rooms(&[4]))
        .await
        .expect("the Cellar moves back");
    assert_eq!(moved.renumbered, vec![renumbering(4, 5)]);
    assert_eq!(
        remapped(remaps.take()),
        vec![(area, vec![(RoomKey::new(secret_area, RoomNumber(4)), 5)])]
    );
    assert_eq!(map_rooms(&mapper, area), vec![1, 2, 3, 4, 5]);
    assert!(secret_rooms(&mapper).is_empty());
    assert_eq!(
        mapper.try_next_room_number(&secret_area).expect("a number"),
        RoomNumber(1),
        "an emptied Secret numbers from 1 again"
    );
}

/// Undo and redo ask for the numbers rooms had. The map's room 3 moves into
/// a Secret that has a room 3 of its own and becomes the Secret's 4; moved
/// back asking for 3, it is the map's 3 again, and the result says 4 became
/// 3. Moved in again asking for 4, it is the Secret's 4. Once the map has a
/// new room 3, asking for 3 gets the map's next number instead, as any
/// renumbering does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moved_room_asks_for_its_old_number_back() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("asks@example.com", "asks", true);
    let area = server.create_area(&owner, "Kitchen");
    for (number, title) in [(1, "Hall"), (2, "Closet"), (3, "Pantry"), (5, "Attic")] {
        server.add_room(area, number, title);
    }
    let cache = TempCacheDir::new("secret-move-asks");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let secret = mapper
        .create_secret(area, "Hidden", None)
        .await
        .expect("create")
        .source;
    let secret_area = match secret {
        SourceId::Secret(id) => AreaId(id),
        _ => panic!("a Secret's source"),
    };
    let remaps = mapper.subscribe_room_remaps();
    let one = |room: i32| MovedContent {
        rooms: vec![RoomNumber(room)],
        ..MovedContent::default()
    };
    let asking = |room: i32, asks: i32| MovedContent {
        rooms: vec![RoomNumber(room)],
        asked: [(RoomNumber(room), RoomNumber(asks))].into_iter().collect(),
        ..MovedContent::default()
    };
    let secret_rooms = |mapper: &Mapper| -> Vec<(i32, String)> {
        bundle(mapper, area, &secret)
            .expect("the Secret is published")
            .rooms
            .iter()
            .map(|room| (room.room_number.0, room.title.clone()))
            .collect()
    };
    let title = |mapper: &Mapper, number: i32| {
        mapper
            .get_current_atlas()
            .get_room(&RoomKey::new(area, RoomNumber(number)))
            .expect("a map room")
            .get_title()
            .to_string()
    };

    // The Secret gets a room 3 of its own, and the map a new room 3.
    mapper
        .move_content(area, SourceId::map(), secret, one(3))
        .await
        .expect("the Pantry moves");
    create_map_room(&mapper, area, 3, "Larder").await;

    // Map #3 becomes Secret #4.
    let moved = mapper
        .move_content(area, SourceId::map(), secret, one(3))
        .await
        .expect("the Larder moves");
    assert_eq!(moved.renumbered, vec![renumbering(3, 4)]);
    assert_eq!(
        secret_rooms(&mapper),
        vec![(3, "Pantry".to_string()), (4, "Larder".to_string())]
    );
    assert_eq!(
        remapped(remaps.take()).pop(),
        Some((secret_area, vec![(RoomKey::new(area, RoomNumber(3)), 4)]))
    );

    // Undo: Secret #4 asks to be map #3, which is free.
    let undone = mapper
        .move_content(area, secret, SourceId::map(), asking(4, 3))
        .await
        .expect("undo");
    assert_eq!(undone.renumbered, vec![renumbering(4, 3)]);
    assert_eq!(map_rooms(&mapper, area), vec![1, 2, 3, 5]);
    assert_eq!(title(&mapper, 3), "Larder");
    assert_eq!(secret_rooms(&mapper), vec![(3, "Pantry".to_string())]);
    assert_eq!(
        remapped(remaps.take()),
        vec![(area, vec![(RoomKey::new(secret_area, RoomNumber(4)), 3)])],
        "handles follow the room back to its old number"
    );

    // Redo: map #3 asks to be Secret #4 again.
    let redone = mapper
        .move_content(area, SourceId::map(), secret, asking(3, 4))
        .await
        .expect("redo");
    assert_eq!(redone.renumbered, vec![renumbering(3, 4)]);
    assert_eq!(
        secret_rooms(&mapper),
        vec![(3, "Pantry".to_string()), (4, "Larder".to_string())]
    );

    // A taken number falls back to the map's next one: 6, above the Attic.
    create_map_room(&mapper, area, 3, "Scullery").await;
    let undone = mapper
        .move_content(area, secret, SourceId::map(), asking(4, 3))
        .await
        .expect("undo onto a taken number");
    assert_eq!(undone.renumbered, vec![renumbering(4, 6)]);
    assert_eq!(map_rooms(&mapper, area), vec![1, 2, 3, 5, 6]);
    assert_eq!(title(&mapper, 3), "Scullery");
    assert_eq!(title(&mapper, 6), "Larder");
}

/// Two rooms asking for the same number: the first listed gets it, and the
/// second is renumbered above everything the destination then holds. A room
/// asking for its own number is no renumbering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_room_asking_for_a_number_gets_it() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("first@example.com", "first", true);
    let area = server.create_area(&owner, "Stairs");
    for (number, title) in [(1, "Landing"), (2, "Step"), (7, "Top")] {
        server.add_room(area, number, title);
    }
    let cache = TempCacheDir::new("secret-move-first-asks");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let secret = mapper
        .create_secret(area, "Hidden", None)
        .await
        .expect("create")
        .source;
    let moved = mapper
        .move_content(
            area,
            SourceId::map(),
            secret,
            MovedContent {
                rooms: vec![RoomNumber(2), RoomNumber(7), RoomNumber(1)],
                asked: [
                    (RoomNumber(2), RoomNumber(9)),
                    (RoomNumber(7), RoomNumber(9)),
                ]
                .into_iter()
                .collect(),
                ..MovedContent::default()
            },
        )
        .await
        .expect("the stairs move");
    assert_eq!(
        moved.renumbered,
        vec![renumbering(2, 9), renumbering(7, 10)]
    );
    let numbers: Vec<i32> = bundle(&mapper, area, &secret)
        .expect("the Secret is published")
        .rooms
        .iter()
        .map(|room| room.room_number.0)
        .collect();
    assert_eq!(numbers, vec![1, 9, 10]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_needs_two_sources_and_something_to_move() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("empty@example.com", "empty", true);
    let area = server.create_area(&owner, "Empty");
    let cache = TempCacheDir::new("secret-move-input");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let secret = mapper
        .create_secret(area, "Hidden", None)
        .await
        .expect("create")
        .source;

    let same = mapper
        .move_content(
            area,
            secret.clone(),
            secret.clone(),
            MovedContent::default(),
        )
        .await;
    assert!(matches!(same, Err(CloudError::InvalidInput(_))), "{same:?}");
    let empty = mapper
        .move_content(area, SourceId::map(), secret, MovedContent::default())
        .await;
    assert!(
        matches!(empty, Err(CloudError::InvalidInput(_))),
        "{empty:?}"
    );
    assert!(
        server.state.lock().mutation_receipts.is_empty(),
        "nothing was sent"
    );
}

/// Queues `operation` as a write to `source` and returns its operation id.
fn write_to(mapper: &Mapper, area: AreaId, source: SourceId, operation: AreaMutation) -> Uuid {
    mapper
        .mutate_batches(vec![
            AreaMutationBatch::strict(area, vec![operation], "Label").in_source(source),
        ])
        .expect("queued")
        .pop()
        .expect("one submission")
        .operation_id()
        .expect("a cloud write")
}

fn labels_in(mapper: &Mapper, area: AreaId, source: &SourceId) -> Vec<LabelId> {
    if source.is_map() {
        let atlas = mapper.get_current_atlas();
        let map = atlas.get_area(&area).expect("the map is loaded");
        return map.get_labels().iter().map(|label| label.id).collect();
    }
    bundle(mapper, area, source)
        .map(|bundle| bundle.labels.iter().map(|label| label.id).collect())
        .unwrap_or_default()
}

/// A label moves between Secrets right after it was drawn or edited there,
/// and back and forth: each move stands on the revisions the edits and the
/// moves before it produced, not on the ones the map was last read at.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_label_moves_between_secrets_right_after_edits_and_moves() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("rehome@example.com", "rehome", true);
    let area = server.create_area(&owner, "Gallery");
    server.add_room(area, 1, "Foyer");
    let cache = TempCacheDir::new("secret-label-move");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache.path()).await;
    let first = mapper
        .create_secret(area, "First", None)
        .await
        .expect("create")
        .source;
    let second = mapper
        .create_secret(area, "Second", None)
        .await
        .expect("create")
        .source;
    let label = LabelId(Uuid::new_v4());
    let only = || MovedContent {
        labels: vec![label],
        ..MovedContent::default()
    };
    let edit = |text: &str| AreaMutation::UpdateLabel {
        label_id: label,
        body: LabelUpdates {
            text: Some(text.to_string()),
            ..LabelUpdates::default()
        },
    };

    // Drawn in the first Secret, then moved on as soon as it is saved.
    let drawn = write_to(
        &mapper,
        area,
        first,
        AreaMutation::CreateLabel {
            body: LabelArgs {
                id: Some(label),
                text: "Ledger".to_string(),
                width: 40.0,
                height: 10.0,
                color: "#ffffff".to_string(),
                font_size: 12,
                font_weight: 400,
                ..LabelArgs::default()
            },
        },
    );
    mapper.wait_for_mutation(drawn).await.expect("drawn");
    mapper
        .move_content(area, first, second, only())
        .await
        .expect("a label drawn in a Secret moves on at once");
    assert_eq!(labels_in(&mapper, area, &second), vec![label]);

    // Edited where it landed, then moved back at once.
    let edited = write_to(&mapper, area, second, edit("Ledger, torn"));
    mapper.wait_for_mutation(edited).await.expect("edited");
    mapper
        .move_content(area, second, first, only())
        .await
        .expect("a label edited in a Secret moves on at once");

    // Edited, then moved before the edit is saved: the move lands it first.
    write_to(&mapper, area, first, edit("Ledger, mended"));
    mapper
        .move_content(area, first, second, only())
        .await
        .expect("a move right after an unsaved edit");

    // Back and forth, through the map too.
    for (from, to) in [
        (second, first),
        (first, second),
        (second, SourceId::map()),
        (SourceId::map(), first),
        (first, second),
    ] {
        mapper
            .move_content(area, from, to, only())
            .await
            .unwrap_or_else(|error| panic!("{from:?} to {to:?}: {error:?}"));
        assert_eq!(labels_in(&mapper, area, &to), vec![label], "{to:?}");
        assert!(labels_in(&mapper, area, &from).is_empty(), "{from:?}");
    }
    let st = server.state.lock();
    let stored = &st.areas[&area.0].secrets[1];
    assert_eq!(
        stored
            .labels
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>(),
        ["Ledger, mended"]
    );
}
