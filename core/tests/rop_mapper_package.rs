//! End-to-end coverage of the first-party `rop-mapper` composition package.
//! The test installs only `RoP`'s root package; its local `auto-mapper` dependency is
//! code-imported into that sandbox, proving the side-effect-free engine/subpath design.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use smudgy_cloud::{
    Area, AreaId, AreaUpdates, AreaWithDetails, CloudResult, CompositeBackend, CreateAreaRequest,
    Credential, CredentialSource, ExitDirection, LocalBackend, MapStorage, Mapper, MapperBackend,
    PackageApiClient, RoomNumber,
    mutation::{AreaMutation, MutationEnvelope, MutationResult},
};
use smudgy_core::models::local_packages::packages_dir;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, TaggedSessionEvent, spawn,
};

const COMPLETION_TIMEOUT: Duration = Duration::from_mins(1);
const COMPLETION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SERVER: &str = "RopMapperTest";

/// A deterministic tier stand-in: CompositeBackend determines the public storage tier
/// from routing membership, while this backend persists acknowledged bytes in a temporary
/// LocalBackend and records envelopes so the package test can assert envelope boundaries
/// and exercise relocation without HTTP.
struct TestTierBackend {
    inner: LocalBackend,
    storage: MapStorage,
    mutations: Mutex<Vec<MutationEnvelope>>,
}

impl TestTierBackend {
    fn new(path: impl Into<std::path::PathBuf>, storage: MapStorage) -> Self {
        Self {
            inner: LocalBackend::new(path),
            storage,
            mutations: Mutex::new(Vec::new()),
        }
    }

    fn mutation_log(&self) -> Vec<MutationEnvelope> {
        self.mutations.lock().unwrap().clone()
    }
}

#[async_trait]
impl MapperBackend for TestTierBackend {
    fn local_snapshot(&self) -> Option<Arc<smudgy_cloud::backends::local::LocalSnapshot>> {
        if self.storage == MapStorage::Local {
            self.inner.local_snapshot()
        } else {
            None
        }
    }

    async fn subscribe_local(&self) -> CloudResult<Option<tokio::sync::watch::Receiver<u64>>> {
        if self.storage == MapStorage::Local {
            self.inner.subscribe_local().await
        } else {
            Ok(None)
        }
    }

    async fn refresh_local(&self) -> CloudResult<()> {
        if self.storage == MapStorage::Local {
            self.inner.refresh_local().await?;
        }
        Ok(())
    }

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        self.inner.create_area(request).await
    }

    async fn create_area_at(
        &self,
        request: CreateAreaRequest,
        storage: MapStorage,
    ) -> CloudResult<Area> {
        assert_eq!(storage, self.storage);
        self.inner.create_area(request).await
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        self.inner.list_areas().await
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.inner.get_area(area_id).await
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.inner.update_area(area_id, updates).await
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        self.inner.delete_area(area_id).await
    }

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        self.mutations.lock().unwrap().push(envelope.clone());
        self.inner.execute_mutation(area_id, envelope).await
    }
}

fn copy_package(server: &str, name: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packages")
        .join(name);
    let dest = packages_dir(server).expect("packages dir").join(name);
    std::fs::create_dir_all(&dest).unwrap();
    for entry in std::fs::read_dir(&source).unwrap_or_else(|_| panic!("read package {name}")) {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), dest.join(entry.file_name())).unwrap();
        }
    }
}

fn localize_rop_dependency(server: &str) {
    let dir = packages_dir(server)
        .expect("packages dir")
        .join("rop-mapper");
    for name in ["index.ts", "smudgy.package.json"] {
        let path = dir.join(name);
        let source = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            path,
            source.replace(
                "smudgy://official/auto-mapper",
                "smudgy://local/auto-mapper",
            ),
        )
        .unwrap();
    }

    let auto_mapper = packages_dir(server)
        .expect("packages dir")
        .join("auto-mapper");
    for name in ["engine.ts", "smudgy.package.json"] {
        let path = auto_mapper.join(name);
        let source = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            path,
            source.replace("smudgy://kapusniak/map-layout", "smudgy://local/map-layout"),
        )
        .unwrap();
    }
}

fn gmcp(name: &str, data: &str) -> RuntimeAction {
    RuntimeAction::GmcpMessage {
        name: Arc::from(name),
        data: Some(Arc::from(data)),
    }
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

/// Wait for the operation's observable postcondition while continuing to collect session output.
/// Durable mapper work can legitimately produce no events for seconds, so event-stream silence is
/// never treated as completion.
async fn wait_until<S, F>(events: &mut S, lines: &mut Vec<String>, description: &str, mut ready: F)
where
    S: Stream<Item = TaggedSessionEvent> + Unpin,
    F: FnMut(&[String]) -> bool,
{
    let completed = tokio::time::timeout(COMPLETION_TIMEOUT, async {
        loop {
            if ready(lines) {
                return true;
            }
            tokio::select! {
                event = events.next() => {
                    let Some(event) = event else {
                        return false;
                    };
                    if let SessionEvent::UpdateBuffer(updates) = &event.event {
                        collect(updates, lines);
                    }
                }
                () = tokio::time::sleep(COMPLETION_POLL_INTERVAL) => {}
            }
        }
    })
    .await;

    assert!(
        matches!(completed, Ok(true)),
        "timed out waiting for {description}.\n{}",
        lines.join("\n")
    );
}

/// Envelope commits ride the mapper's own queue, which keeps working after the session's
/// event stream goes quiet; backend mutation counts are only comparable once it drains.
async fn drain_mutation_queue(mapper: &Mapper) {
    match mapper.wait_for_sync_completion(60).await {
        Ok(true) => {}
        Ok(false) => panic!("mutation queue still pending after 60 seconds"),
        Err(()) => panic!("mutation queue reported failed operations"),
    }
}

fn room_exists(mapper: &Mapper, external_id: &str) -> bool {
    mapper
        .get_current_atlas()
        .find_room_by_external_id(external_id)
        .is_some()
}

fn room_title_is(mapper: &Mapper, external_id: &str, title: &str) -> bool {
    mapper
        .get_current_atlas()
        .find_room_by_external_id(external_id)
        .is_some_and(|(_, room)| room.get_title() == title)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn rop_mapper_imports_engine_and_provisions_authoritative_neighborhood() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).unwrap();
    copy_package(SERVER, "map-layout");
    copy_package(SERVER, "auto-mapper");
    copy_package(SERVER, "rop-mapper");
    localize_rop_dependency(SERVER);
    shared_packages::install_package(SERVER, "smudgy://local/rop-mapper", UpdateMode::Auto, true)
        .unwrap();

    let map_root = smudgy_home.join("map-test");
    let local = Arc::new(TestTierBackend::new(
        map_root.join("local"),
        MapStorage::Local,
    ));
    let cloud = Arc::new(TestTierBackend::new(
        map_root.join("cloud"),
        MapStorage::Cloud,
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local.clone(), cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9350_u32),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: Some(PackageApiClient::new(
            "http://127.0.0.1:0",
            CredentialSource::new(Some(Credential::ApiKey("test".into()))),
        )),
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    tx.send(RuntimeAction::GmcpEnabled).unwrap();

    // Room.Info uses RoP's structured exit values. The north room is first inferred at
    // (0,-1); Room.Map then authoritatively places it northwest and provisions two more
    // rooms that Room.Info did not mention.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 5539, "name": "The Bishop's Private Room",
             "zone": "Rites of Passage", "terrain": "inside",
             "exits": {
                 "n": { "v": 5540, "door": 0 },
                 "e": { "v": 5541, "door": 2 }
             } }"#,
    ))
    .unwrap();

    // The engine commits Room.Info through separate mutateArea gestures with host
    // round-trips between them, so the mapper queue is momentarily idle between the
    // rooms gesture and the topology gesture; a drain alone can return inside that gap
    // and leak Room.Info's exits envelope into the Room.Map window below. Completion
    // is therefore Room.Info's LAST effect: its topology reaching the backend log.
    let exit_committed = |direction: ExitDirection| {
        local
            .mutation_log()
            .iter()
            .flat_map(|envelope| envelope.payload.iter())
            .any(|op| {
                matches!(
                    op,
                    AreaMutation::CreateExit { body, .. } if body.from_direction == direction
                )
            })
    };
    wait_until(
        &mut events,
        &mut lines,
        "Room.Info to provision the neighborhood and commit its exit topology",
        |_| {
            room_exists(&mapper, "5539")
                && room_exists(&mapper, "5540")
                && room_exists(&mapper, "5541")
                && exit_committed(ExitDirection::North)
                && exit_committed(ExitDirection::East)
        },
    )
    .await;
    // Drain before the snapshot so the log index below is a CAUSAL boundary:
    // every envelope recorded past it was caused by Room.Map alone. (The atlas
    // publishes only after enqueue, so anything atlas-visible above is flushed here.)
    drain_mutation_queue(&mapper).await;
    let mutations_before_room_map = local.mutation_log().len();

    tx.send(gmcp(
        "Room.Map",
        r#"{ "px": 10, "py": 10, "rooms": [
              { "x": 10, "y": 10, "v": 5539, "e": "ne", "s": 0, "i": 1, "d": 0 },
              { "x": 9,  "y": 9,  "v": 5540, "e": "s",  "s": 1, "i": 0, "d": 0 },
              { "x": 11, "y": 10, "v": 5541, "e": "w",  "s": 1, "i": 0, "d": 0 },
              { "x": 10, "y": 11, "v": 5542, "e": "n",  "s": 2, "i": 0, "d": 0 },
              { "x": 12, "y": 11, "v": 5550, "e": "",   "s": 3, "i": 0, "d": 0 }
            ] }"#,
    ))
    .unwrap();

    wait_until(
        &mut events,
        &mut lines,
        "Room.Map to provision and link the authoritative neighborhood",
        |_| {
            room_exists(&mapper, "5550")
                && mapper
                    .get_current_atlas()
                    .find_room_by_external_id("5542")
                    .is_some_and(|(_, room)| !room.get_exits().is_empty())
        },
    )
    .await;
    drain_mutation_queue(&mapper).await;

    // The engine commits a Room.Map snapshot through separate mutateArea gestures with
    // host round-trips between them, so counting envelopes inside a TIME window is
    // unsound — but the drains above and below are causal boundaries, so the log
    // DELTA is exactly Room.Map's commits. Content identifies which envelope is
    // which; the window length restores the "and nothing else" half a content
    // filter cannot see (a spurious extra envelope, or topology split in two).
    let log = local.mutation_log();
    let window = &log[mutations_before_room_map..];
    assert_eq!(
        window.len(),
        2,
        "Room.Map commits exactly one rooms envelope + one topology envelope:\n{window:#?}"
    );
    let creates_reported_room = |envelope: &MutationEnvelope| {
        envelope.payload.iter().any(|op| {
            matches!(
                op,
                AreaMutation::CreateRoom { body, .. }
                    if matches!(&body.external_id, Some(Some(id)) if id == "5542" || id == "5550")
            )
        })
    };
    let room_envelope_indices: Vec<usize> = window
        .iter()
        .enumerate()
        .filter(|(_, envelope)| creates_reported_room(envelope))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        room_envelope_indices.len(),
        1,
        "Room.Map provisions its new rooms in one room/property envelope"
    );
    let rooms_envelope = &window[room_envelope_indices[0]];
    let created_numbers: Vec<RoomNumber> = rooms_envelope
        .payload
        .iter()
        .filter_map(|op| match op {
            AreaMutation::CreateRoom { room_number, .. } => Some(*room_number),
            _ => None,
        })
        .collect();
    for number in &created_numbers {
        assert!(
            rooms_envelope.payload.iter().any(|op| matches!(
                op,
                AreaMutation::UpsertRoomProperty { room_number, name, .. }
                    if room_number == number && name == "unvisited"
            )),
            "provisioned rooms carry their properties in the same envelope"
        );
    }
    assert!(
        rooms_envelope
            .payload
            .iter()
            .all(|op| !matches!(op, AreaMutation::CreateExit { .. })),
        "the room/property envelope carries no topology"
    );
    let atlas_after_map = mapper.get_current_atlas();
    let (key5542, _) = atlas_after_map
        .find_room_by_external_id("5542")
        .expect("provisioned room mapped");
    let exit_envelope_indices: Vec<usize> = window
        .iter()
        .enumerate()
        .filter(|(_, envelope)| {
            envelope.payload.iter().any(|op| {
                matches!(
                    op,
                    AreaMutation::CreateExit { room_number, .. }
                        if *room_number == key5542.room_number
                )
            })
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        exit_envelope_indices.len(),
        1,
        "Room.Map links its same-area topology in one envelope"
    );
    assert!(
        room_envelope_indices[0] < exit_envelope_indices[0],
        "rooms and properties commit before the topology that links them"
    );

    // The north command is refused without a Room.Info acknowledgement. The following
    // east move must still resolve east from server ids, never as a north traversal.
    tx.send(RuntimeAction::Send(Arc::new("n".to_string())))
        .unwrap();
    tx.send(RuntimeAction::Send(Arc::new("e".to_string())))
        .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 5541, "name": "The Training Grounds",
             "zone": "Rites of Passage", "terrain": "city",
             "exits": { "w": { "v": 5539, "door": 0 } } }"#,
    ))
    .unwrap();

    wait_until(
        &mut events,
        &mut lines,
        "the east move's Room.Info to retitle the east room",
        |_| room_title_is(&mapper, "5541", "The Training Grounds"),
    )
    .await;

    let local_atlas = mapper.get_current_atlas();
    let (local_key, _) = local_atlas
        .find_room_by_external_id("5539")
        .expect("center room mapped into the initial local area");
    assert_eq!(
        mapper.area_storage(&local_key.area_id),
        MapStorage::Local,
        "RoP mapping begins in durable local storage"
    );

    tx.send(RuntimeAction::Send(Arc::new("ropmap upgrade".to_string())))
        .unwrap();
    wait_until(
        &mut events,
        &mut lines,
        "ropmap upgrade to relocate the map into cloud storage",
        |_| {
            mapper
                .get_current_atlas()
                .find_room_by_external_id("5539")
                .is_some_and(|(key, _)| mapper.area_storage(&key.area_id) == MapStorage::Cloud)
        },
    )
    .await;

    // A post-upgrade revisit must write through the rebound cloud area, proving the
    // mapper did not retain its deleted local-area handle after relocation.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 5539, "name": "The Bishop's Private Room (cloud)",
             "zone": "Rites of Passage", "terrain": "inside",
             "exits": {
                 "n": { "v": 5540, "door": 0 },
                 "e": { "v": 5541, "door": 2 }
             } }"#,
    ))
    .unwrap();
    wait_until(
        &mut events,
        &mut lines,
        "the post-upgrade revisit to retitle the center room",
        |_| room_title_is(&mapper, "5539", "The Bishop's Private Room (cloud)"),
    )
    .await;
    let transcript = lines.join("\n");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("[auto-mapper] active"))
            .count(),
        1,
        "the imported engine starts exactly once; transcript:\n{transcript}"
    );

    let atlas = mapper.get_current_atlas();
    let (key5539, room5539) = atlas
        .find_room_by_external_id("5539")
        .unwrap_or_else(|| panic!("center room mapped; transcript:\n{transcript}"));
    let (_, room5540) = atlas
        .find_room_by_external_id("5540")
        .unwrap_or_else(|| panic!("north room provisioned; transcript:\n{transcript}"));
    let (key5541, room5541) = atlas
        .find_room_by_external_id("5541")
        .unwrap_or_else(|| panic!("east room provisioned; transcript:\n{transcript}"));
    let (_, room5550) = atlas.find_room_by_external_id("5550").unwrap_or_else(|| {
        panic!("unconnected Room.Map room provisioned; transcript:\n{transcript}")
    });
    let area = atlas.get_area(&key5539.area_id).expect("RoP area");
    assert_eq!(
        mapper.area_storage(&key5539.area_id),
        MapStorage::Cloud,
        "ropmap upgrade moves the bound local map into cloud storage"
    );
    assert_eq!(
        room5539.get_title(),
        "The Bishop's Private Room (cloud)",
        "mapping continues with cloud autosave after the upgrade"
    );
    assert_eq!(
        area.room_count(),
        5,
        "Room.Map provisions every reported room, not only Room.Info exits"
    );
    assert!(
        (room5540.get_x() - (room5539.get_x() - 1.0)).abs() < 0.01
            && (room5540.get_y() - (room5539.get_y() - 1.0)).abs() < 0.01,
        "Room.Map coordinates replace the earlier north-only inference"
    );
    assert!(
        (room5541.get_x() - (room5539.get_x() + 1.0)).abs() < 0.01
            && (room5541.get_y() - room5539.get_y()).abs() < 0.01,
        "the successful room remains east of the center"
    );
    assert_eq!(
        room5550.get_property("unvisited"),
        Some("true"),
        "provisioned rooms remain unvisited"
    );

    let east = room5539
        .get_exits()
        .iter()
        .find(|exit| exit.from_direction == ExitDirection::East)
        .unwrap_or_else(|| panic!("structured east exit parsed; transcript:\n{transcript}"));
    assert_eq!(east.to_room_number, Some(key5541.room_number));
    assert!(east.is_closed, "RoP door state is preserved");
    assert!(
        !room5539.get_exits().iter().any(|exit| {
            exit.from_direction == ExitDirection::North
                && exit.to_room_number == Some(key5541.room_number)
        }),
        "the failed north command cannot be attributed to the east room"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}
