//! Real host delivery to idle sessions with independent local mapper caches.

mod support;

use std::time::Duration;

use smudgy_cloud::{
    AreaId, LocalBackend, MapDestination, MapStorage, Mapper, MapperBackend, RoomNumber,
    RoomUpdates, mapper::RoomKey,
};
use support::nukefire::{Session, local_mapper, smudgy_home, start_session};

async fn seed_rooms(mapper: &Mapper, area: AreaId, rooms: &[(i32, f32)]) {
    for &(number, x) in rooms {
        let submission = mapper
            .create_room(
                RoomKey::new(area, RoomNumber(number)),
                RoomUpdates {
                    title: Some(format!("Room {number}")),
                    x: Some(x),
                    y: Some(0.0),
                    level: Some(0),
                    ..RoomUpdates::default()
                },
            )
            .unwrap();
        mapper
            .wait_for_mutation(submission.operation_id().unwrap())
            .await
            .unwrap();
    }
}

async fn expect_line(session: &mut Session, text: &str) {
    assert!(
        session
            .wait_until(|lines| lines.iter().any(|line| line == text))
            .await,
        "{}",
        session.transcript()
    );
}

async fn expect_migration(session: &mut Session, remap: &str, marker: &str, total: usize) {
    expect_line(session, marker).await;
    let merged_index = session.lines.iter().position(|line| line == remap).unwrap();
    let marker_index = session
        .lines
        .iter()
        .position(|line| line == marker)
        .unwrap();
    assert!(
        merged_index < marker_index,
        "remap listeners must run before map:room: {}",
        session.transcript()
    );
    assert_eq!(
        session
            .lines
            .iter()
            .filter(|line| line.starts_with("MERGED "))
            .count(),
        total,
        "{}",
        session.transcript()
    );
}

#[tokio::test]
async fn startup_markers_follow_commits_before_dispatch_and_preserve_reused_rooms() {
    let server = "MapperQueuedRoomRemaps";
    let home = smudgy_home();
    let mapper = local_mapper(&home.join("maps").join(server));
    mapper.ready().await.unwrap();
    let into = mapper
        .create_area_at(
            "Destination".into(),
            MapDestination::loose(MapStorage::Local),
        )
        .await
        .unwrap();
    let source = mapper
        .create_area_at("Source".into(), MapDestination::loose(MapStorage::Local))
        .await
        .unwrap();
    for area in [into, source] {
        seed_rooms(&mapper, area, &[(1, 0.0)]).await;
    }
    let modules = home.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::write(modules.join("queued-remaps.ts"), format!(r#"
import {{ createAlias, echo, mapper }} from "smudgy:core";
import {{ merged, room }} from "smudgy:events/map";
await mapper.ready();
const destination = "{into}";
const source = "{source}";
const tag = (area) => area === destination ? "D" : "S";
merged.on((payload) => echo("MERGED " + payload.rooms.map((r) => `${{tag(r.from.area)}}${{r.from.room}}>${{tag(payload.into)}}${{r.to}}`).join(",")));
room.on((payload) => {{
    const current = mapper.getCurrentLocation();
    echo(`ROOM ${{tag(payload.areaId)}}${{payload.roomNumber}} CURRENT ${{tag(current.area)}}${{current.room}}`);
}});
// All markers remain queued while startup awaits these two durable commits.
mapper.setCurrentLocation(source, 1);
await mapper.mergeAreas(destination, [source]);
await mapper.mergeRooms(destination, 1, 2);
const reused = await mapper.createRoom(destination, {{ title: "Reused", x: 1, y: 0, level: 0 }});
mapper.setCurrentLocation(destination, reused);
createAlias(/^queued-remap-read$/, () => {{
    const current = mapper.getCurrentLocation();
    echo(`FINAL ${{tag(current.area)}}${{current.room}} REUSED ${{reused}}`);
}});
"#)).unwrap();
    let mut session = start_session(server, 9534, &mapper).await;
    session.send("queued-remap-read");
    expect_line(&mut session, "FINAL D2 REUSED 2").await;
    let merged: Vec<_> = session
        .lines
        .iter()
        .filter(|line| line.starts_with("MERGED "))
        .map(String::as_str)
        .collect();
    assert_eq!(
        merged,
        ["MERGED S1>D2", "MERGED D2>D1"],
        "{}",
        session.transcript()
    );
    let last_merge = session
        .lines
        .iter()
        .rposition(|line| line.starts_with("MERGED "))
        .unwrap();
    let first_marker = session
        .lines
        .iter()
        .position(|line| line.starts_with("ROOM "))
        .unwrap();
    assert!(last_merge < first_marker, "{}", session.transcript());
    assert!(
        !session.lines.iter().any(|line| line.starts_with("ROOM S")),
        "{}",
        session.transcript()
    );
    assert!(
        session
            .lines
            .iter()
            .filter(|line| line.starts_with("ROOM "))
            .all(|line| line == "ROOM D1 CURRENT D1" || line == "ROOM D2 CURRENT D2"),
        "{}",
        session.transcript()
    );
    session.shutdown();
}

#[tokio::test]
async fn independent_sessions_follow_local_merges_and_joins_in_order() {
    let server = "MapperRoomRemaps";
    let home = smudgy_home();
    let root = home.join("maps").join(server);
    let owner = local_mapper(&root);
    owner.ready().await.unwrap();
    let into = owner
        .create_area_at(
            "Destination".into(),
            MapDestination::loose(MapStorage::Local),
        )
        .await
        .unwrap();
    let from = owner
        .create_area_at("Source".into(), MapDestination::loose(MapStorage::Local))
        .await
        .unwrap();
    seed_rooms(&owner, into, &[(1, 1.0), (2, 2.0), (3, 3.0)]).await;
    seed_rooms(&owner, from, &[(1, 1.0), (2, 2.0)]).await;
    let observer = local_mapper(&root);
    observer.ready().await.unwrap();
    // Identical ids in an unrelated store must never receive the migration.
    let unrelated_root = home.join("maps").join("UnrelatedRemapStore");
    let documents = vec![
        owner.export_area(into).await.unwrap(),
        owner.export_area(from).await.unwrap(),
    ];
    let backend = LocalBackend::new(unrelated_root.join("local"));
    for document in documents {
        backend.import_local_area(document).await.unwrap();
    }
    let unrelated = local_mapper(&unrelated_root);
    unrelated.ready().await.unwrap();

    let modules = home.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::write(modules.join("remaps.ts"), format!(r#"
import {{ createAlias, echo, mapper }} from "smudgy:core";
import {{ merged, room }} from "smudgy:events/map";
await mapper.ready();
const into = "{into}";
const source = "{from}";
const tag = (area) => area === into ? "D" : area === source ? "S" : "?";
merged.on((payload) => echo("MERGED " + payload.rooms.map((r) => `${{tag(r.from.area)}}${{r.from.room}}>${{tag(payload.into)}}${{r.to}}`).join(",")));
room.on((payload) => echo(`ROOM ${{tag(payload.areaId)}}${{payload.roomNumber}}`));
mapper.setCurrentLocation(source, 2);
createAlias(/^remap-merge$/, async () => {{
    await mapper.mergeAreas(into, [source]);
    const current = mapper.getCurrentLocation();
    echo(`MERGE-READ ${{tag(current?.area)}}${{current?.room}}`);
}});
createAlias(/^remap-join$/, async () => {{
    await mapper.mergeRooms(into, 1, 5);
    const current = mapper.getCurrentLocation();
    echo(`JOIN-READ ${{tag(current?.area)}}${{current?.room}}`);
}});
createAlias(/^remap-read$/, () => {{
    const current = mapper.getCurrentLocation();
    echo(`READ ${{tag(current?.area)}}${{current?.room}}`);
}});
"#)).unwrap();

    let mut owner_session = start_session(server, 9531, &owner).await;
    let mut observer_session = start_session(server, 9532, &observer).await;
    let mut unrelated_session = start_session(server, 9533, &unrelated).await;
    for session in [
        &mut owner_session,
        &mut observer_session,
        &mut unrelated_session,
    ] {
        expect_line(session, "ROOM S2").await;
        session.lines.clear();
    }
    owner_session.send("remap-merge");
    for session in [&mut owner_session, &mut observer_session] {
        expect_migration(session, "MERGED S1>D4,S2>D5", "ROOM D5", 1).await;
    }
    expect_line(&mut owner_session, "MERGE-READ D5").await;
    owner_session.send("remap-join");
    for session in [&mut owner_session, &mut observer_session] {
        expect_migration(session, "MERGED D5>D1", "ROOM D1", 2).await;
    }
    expect_line(&mut owner_session, "JOIN-READ D1").await;
    unrelated_session.send("remap-read");
    expect_line(&mut unrelated_session, "READ S2").await;
    unrelated_session.run_for(Duration::from_millis(100)).await;
    assert!(
        !unrelated_session
            .lines
            .iter()
            .any(|line| line.starts_with("MERGED ")),
        "{}",
        unrelated_session.transcript()
    );
    assert!(unrelated.get_current_atlas().get_area(&from).is_some());
    for session in [&owner_session, &observer_session, &unrelated_session] {
        session.shutdown();
    }
    // Keep all cache owners alive through host shutdown; none of the sessions
    // shares the owner Mapper's cache or its event subscriber.
    let _owners: [Mapper; 3] = [owner, observer, unrelated];
}
