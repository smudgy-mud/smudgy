//! Links to deleted rooms across maps and storage tiers: deleting a room
//! clears every other map's links to it where that map is saved, through the
//! map's own edit queue, and a new room never takes a number a link still
//! names.

mod support;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use smudgy_cloud::{
    AreaId, CachedCloudMapper, CloudMapper, CompositeBackend, ExitArgs, ExitDirection, ExitId,
    LocalBackend, MapDestination, MapStorage, Mapper, MapperBackend, RoomNumber, RoomUpdates, Uuid,
    backends::EphemeralBackend,
    mapper::{RoomKey, SyncState},
    mutation::AreaMutation,
};
use support::MockServer;

/// A scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        Self(std::env::temp_dir().join(format!("smudgy-stale-links-{tag}-{}", Uuid::new_v4())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Polls `condition` every 2 ms for up to ~4 s, then asserts it.
async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..2000u32 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(condition(), "condition not met within timeout");
}

/// Waits until every edit the mapper queued has reached its store.
async fn settle(mapper: &Mapper) {
    wait_until(|| mapper.get_sync_stats().pending_operations() == 0).await;
}

fn room(number: i32) -> AreaMutation {
    AreaMutation::UpsertRoom {
        room_number: RoomNumber(number),
        body: RoomUpdates::default(),
    }
}

/// Exit `exit` from room `from` to room `to.1` of map `to.0`.
fn link(from: i32, exit: ExitId, to: (AreaId, i32)) -> AreaMutation {
    AreaMutation::CreateExit {
        room_number: RoomNumber(from),
        body: ExitArgs {
            id: Some(exit),
            from_direction: ExitDirection::North,
            to_area_id: Some(to.0),
            to_room_number: Some(RoomNumber(to.1)),
            weight: 1.0,
            ..ExitArgs::default()
        },
    }
}

/// Where `exit` of room `host` leads in the session.
fn session_destination(
    mapper: &Mapper,
    host: &RoomKey,
    exit: ExitId,
) -> (Option<AreaId>, Option<RoomNumber>) {
    mapper
        .get_current_atlas()
        .get_room(host)
        .expect("host room shown")
        .get_exits()
        .iter()
        .find(|candidate| candidate.id == exit)
        .map(|found| (found.to_area_id, found.to_room_number))
        .expect("exit shown")
}

/// Where `exit` of map `area` leads in `store`.
async fn stored_destination(
    store: &dyn MapperBackend,
    area: AreaId,
    exit: ExitId,
) -> (Option<AreaId>, Option<RoomNumber>) {
    store
        .get_area(&area)
        .await
        .expect("map stored")
        .rooms
        .iter()
        .flat_map(|room| &room.exits)
        .find(|candidate| candidate.id == exit)
        .map(|found| (found.to_area_id, found.to_room_number))
        .expect("exit stored")
}

/// A signed-in session over a local store under `dir` and the mock server,
/// once its first sync has settled.
async fn signed_in_mapper(base_url: &str, api_key: &str, dir: &Path) -> Mapper {
    let local = Arc::new(LocalBackend::new(dir.join("local")));
    let cloud = Arc::new(CachedCloudMapper::new(
        CloudMapper::new(base_url.to_string(), api_key.to_string()),
        dir.join("cloud"),
    ));
    let mapper = Mapper::new(
        Arc::new(CompositeBackend::new(local, cloud)),
        dir.join("mapper"),
    );
    wait_until(|| {
        mapper.sync_status().last_sync.is_some() && mapper.sync_status().state == SyncState::Idle
    })
    .await;
    mapper
}

/// A session map's room 5 links to local map L's room 2. Deleting L 2 clears
/// the link in the session store as on screen, so the room L creates next,
/// which takes number 2, is not the link's destination, before or after the
/// maps reload.
#[tokio::test]
async fn a_session_maps_link_to_a_deleted_local_room_is_cleared_in_its_store() {
    let scratch = Scratch::new("session-link");
    let backend = Arc::new(CompositeBackend::new(
        Arc::new(LocalBackend::new(scratch.path().join("local"))),
        Arc::new(EphemeralBackend::new()),
    ));
    let mapper = Mapper::new(backend.clone(), scratch.path().join("cache"));
    mapper.ready().await.expect("ready");
    let l = mapper
        .create_area_at("L".into(), MapDestination::loose(MapStorage::Local))
        .await
        .expect("create L");
    let s = mapper
        .create_area_at("S".into(), MapDestination::loose(MapStorage::Session))
        .await
        .expect("create S");
    let exit = ExitId(Uuid::new_v4());
    mapper
        .mutate_area(l, vec![room(1), room(2)], "seed L")
        .expect("seed L");
    mapper
        .mutate_area(s, vec![room(5), link(5, exit, (l, 2))], "seed S")
        .expect("seed S");
    settle(&mapper).await;

    mapper
        .delete_room(RoomKey::new(l, RoomNumber(2)))
        .expect("delete L 2");
    settle(&mapper).await;

    let host = RoomKey::new(s, RoomNumber(5));
    assert_eq!(session_destination(&mapper, &host, exit), (None, None));
    assert_eq!(
        stored_destination(backend.as_ref(), s, exit).await,
        (None, None),
        "the session store holds the link cleared"
    );
    let next = mapper.next_room_number(&l).expect("L is loaded");
    assert_eq!(next, RoomNumber(2), "no link names the number any more");
    mapper
        .create_room(RoomKey::new(l, next), RoomUpdates::default())
        .expect("create a room in L");
    settle(&mapper).await;
    mapper.load_all_areas().await.expect("reload");
    assert_eq!(
        session_destination(&mapper, &host, exit),
        (None, None),
        "the new room is not the old link's destination"
    );
}

/// Local map L's room 5 links to cloud map C's room 2. Deleting C 2 sends
/// the deletion to the server and clears L's link in the local store, where
/// L is saved: the server hears only the deletion, and a new session over the
/// same store shows the link cleared.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_maps_link_to_a_deleted_cloud_room_is_cleared_in_the_local_store() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("links-local@example.com", "links-local", true);
    let c = server.create_area(&owner, "C");
    server.add_room(c, 1, "One", false);
    server.add_room(c, 2, "Two", false);
    let scratch = Scratch::new("cloud-room");
    let mapper = signed_in_mapper(&server.base_url, &owner.api_key, scratch.path()).await;
    wait_until(|| mapper.get_current_atlas().get_area(&c).is_some()).await;
    let l = mapper
        .create_area_at("L".into(), MapDestination::loose(MapStorage::Local))
        .await
        .expect("create L");
    let exit = ExitId(Uuid::new_v4());
    mapper
        .mutate_area(l, vec![room(5), link(5, exit, (c, 2))], "seed L")
        .expect("seed L");
    settle(&mapper).await;

    mapper
        .delete_room(RoomKey::new(c, RoomNumber(2)))
        .expect("delete C 2");
    settle(&mapper).await;

    assert!(
        !server.state.lock().areas[&c.0].rooms.contains_key(&2),
        "the server deleted the room"
    );
    assert_eq!(
        server.mutation_requests().len(),
        1,
        "the server hears only the deletion"
    );
    let host = RoomKey::new(l, RoomNumber(5));
    assert_eq!(session_destination(&mapper, &host, exit), (None, None));
    let local = LocalBackend::new(scratch.path().join("local"));
    local.refresh().await.expect("reread the local store");
    assert_eq!(
        stored_destination(&local, l, exit).await,
        (None, None),
        "the local store holds the link cleared"
    );
    drop(mapper);

    let reopened = signed_in_mapper(&server.base_url, &owner.api_key, scratch.path()).await;
    wait_until(|| {
        let atlas = reopened.get_current_atlas();
        atlas.get_area(&l).is_some() && atlas.get_area(&c).is_some()
    })
    .await;
    assert_eq!(
        session_destination(&reopened, &host, exit),
        (None, None),
        "the link stays cleared for a new session"
    );
}

/// Cloud map X's room 5 links to cloud map C's room 2. Deleting C 2 leaves
/// X's link to the server, which clears it in the deletion's own
/// transaction: the server hears only the deletion, the session shows the
/// link cleared at once, and a sync keeps it cleared with nothing in
/// conflict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cloud_maps_link_to_a_deleted_cloud_room_is_left_to_the_server() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("links-cloud@example.com", "links-cloud", true);
    let c = server.create_area(&owner, "C");
    server.add_room(c, 1, "One", false);
    server.add_room(c, 2, "Two", false);
    let x = server.create_area(&owner, "X");
    server.add_room(x, 5, "Five", false);
    let exit = ExitId(server.add_exit(x, 5, "North", Some((c, 2)), false));
    let scratch = Scratch::new("cloud-cloud");
    let mapper = signed_in_mapper(&server.base_url, &owner.api_key, scratch.path()).await;
    let host = RoomKey::new(x, RoomNumber(5));
    wait_until(|| mapper.get_current_atlas().get_room(&host).is_some()).await;
    assert_eq!(
        session_destination(&mapper, &host, exit),
        (Some(c), Some(RoomNumber(2)))
    );

    mapper
        .delete_room(RoomKey::new(c, RoomNumber(2)))
        .expect("delete C 2");
    assert_eq!(
        session_destination(&mapper, &host, exit),
        (None, None),
        "the session mirrors the server's cascade at once"
    );
    settle(&mapper).await;

    assert_eq!(
        server.mutation_requests().len(),
        1,
        "the server hears only the deletion"
    );
    let stored = server.state.lock().areas[&x.0]
        .exits
        .iter()
        .find(|candidate| ExitId(candidate.id) == exit)
        .map(|found| (found.to_area_id, found.to_room_number))
        .expect("exit stored");
    assert_eq!(stored, (None, None), "the server cleared the link itself");
    let before = mapper.sync_status().last_sync;
    mapper.sync_now();
    wait_until(|| mapper.sync_status().last_sync != before).await;
    assert_eq!(session_destination(&mapper, &host, exit), (None, None));
    assert!(mapper.conflicted_operation_id(x).is_none());
    assert!(mapper.failed_operation_id(x).is_none());
}

/// One exit naming the highest room number map A could hold takes nothing
/// from A's allocation: A holds rooms 1 and 2, and its next room and a
/// draft's reservation both take 3.
#[tokio::test]
async fn one_exit_naming_the_top_room_number_leaves_an_area_its_numbers() {
    let scratch = Scratch::new("top-number");
    let mapper = Mapper::new(
        Arc::new(LocalBackend::new(scratch.path().join("local"))),
        scratch.path().join("cache"),
    );
    mapper.ready().await.expect("ready");
    let local = || MapDestination::loose(MapStorage::Local);
    let a = mapper
        .create_area_at("A".into(), local())
        .await
        .expect("create A");
    let b = mapper
        .create_area_at("B".into(), local())
        .await
        .expect("create B");
    mapper
        .mutate_area(a, vec![room(1), room(2)], "seed A")
        .expect("seed A");
    mapper
        .mutate_area(
            b,
            vec![room(1), link(1, ExitId(Uuid::new_v4()), (a, i32::MAX))],
            "seed B",
        )
        .expect("seed B");
    settle(&mapper).await;

    assert_eq!(mapper.next_room_number(&a), Some(RoomNumber(3)));
    let draft = Uuid::new_v4();
    assert_eq!(
        mapper
            .reserve_room_number(&a, draft)
            .expect("a number is left"),
        RoomNumber(3)
    );
    mapper.release_room_reservations(&a, draft);
    assert_eq!(
        mapper.try_next_room_number(&a).expect("a number is left"),
        RoomNumber(3)
    );
}
