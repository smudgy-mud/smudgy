//! End-to-end smoke coverage for the authored `nukefire-mapper` package.
//! A minimal local `nukefire-gmcp` fixture exposes the same retained-tree and
//! per-message helpers the mapper consumes; the real mapper and map-layout
//! sources run sandboxed under their manifests.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::StreamExt;
use smudgy_cloud::{
    CloudMapper, CompositeBackend, Credential, CredentialSource, LocalBackend, MapStorage, Mapper,
    MapperBackend, PackageApiClient,
};
use smudgy_core::models::local_packages::packages_dir;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);
const SERVER: &str = "tdome.nukefire.org";
const MAPPER_SPEC: &str = "smudgy://local/nukefire-mapper";

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|candidate| candidate == name) {
            return Some(path);
        }
    }
    None
}

fn copy_package(server: &str, name: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packages")
        .join(name);
    let destination = packages_dir(server).expect("packages dir").join(name);
    std::fs::create_dir_all(&destination).expect("create package directory");
    for entry in std::fs::read_dir(&source).unwrap_or_else(|_| panic!("read package {name}")) {
        let entry = entry.expect("package entry");
        if entry.file_type().expect("entry type").is_file() {
            std::fs::copy(entry.path(), destination.join(entry.file_name()))
                .expect("copy package source");
        }
    }
}

fn localize_mapper_dependencies(server: &str) {
    let directory = packages_dir(server)
        .expect("packages dir")
        .join("nukefire-mapper");
    for entry in std::fs::read_dir(&directory).expect("read mapper package") {
        let entry = entry.expect("mapper package entry");
        let path = entry.path();
        let is_source = path.extension().is_some_and(|extension| extension == "ts");
        let is_manifest = path
            .file_name()
            .is_some_and(|name| name == "smudgy.package.json");
        if !is_source && !is_manifest {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read mapper source");
        let localized = source
            .replace(
                "smudgy://kapusniak/nukefire-gmcp",
                "smudgy://local/nukefire-gmcp",
            )
            .replace("smudgy://kapusniak/map-layout", "smudgy://local/map-layout");
        std::fs::write(path, localized).expect("localize mapper dependency");
    }
}

fn write_gmcp_fixture(server: &str) {
    let directory = packages_dir(server)
        .expect("packages dir")
        .join("nukefire-gmcp");
    std::fs::create_dir_all(&directory).expect("create GMCP fixture");
    std::fs::write(
        directory.join("smudgy.package.json"),
        r#"{
          "version": "0.0.0-test",
          "entry": "index.ts",
          "permissions": { "smudgy": { "interop": ["read"] } }
        }"#,
    )
    .expect("write GMCP fixture manifest");
    std::fs::write(
        directory.join("index.ts"),
        r#"import gmcp from "smudgy:state/gmcp";
export const nukefire = gmcp;
export function watchMessage(name: string, handler: (payload: any) => void) {
  return nukefire.watch(name, handler);
}
export function onMessage(name: string, handler: (payload: any) => void) {
  return nukefire.onWrite(name, (path: string, snapshot: any) => {
    if (path.toLowerCase() === name.toLowerCase() && snapshot !== undefined) handler(snapshot);
  });
}
"#,
    )
    .expect("write GMCP fixture source");
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

#[tokio::test]
async fn nukefire_snapshot_creates_one_local_area_inside_the_nukefire_atlas() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).unwrap();
    copy_package(SERVER, "map-layout");
    copy_package(SERVER, "nukefire-mapper");
    write_gmcp_fixture(SERVER);
    localize_mapper_dependencies(SERVER);
    shared_packages::install_package(SERVER, MAPPER_SPEC, UpdateMode::Auto, true)
        .expect("install NukeFire mapper");
    shared_packages::save_param_value(
        SERVER,
        MAPPER_SPEC,
        "debugMappingDecisions",
        serde_json::json!(true),
    )
    .expect("enable mapper decision log");

    let map_root = smudgy_home.join("map-test");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9360_u32),
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
    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 100, "name": "Central Plaza", "area": "Tek Angeles",
          "zone": 42, "terrain": "city", "exits": {},
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    let snapshot = r#"{
      "version": 1, "source": "bigmap+gps", "center": 100,
      "zone": 30, "plane": 0,
      "rooms": [{
        "vnum": 100, "name": "Central Plaza", "zone": 30,
        "terrain": "city", "x": 0, "y": 0, "z": 0,
        "current": true, "route": false, "destination": false
      }],
      "links": [],
      "gps": {
        "active": false, "type": "none", "target": -1,
        "description": "", "steps": 0, "route_raw": ""
      },
      "truncated": false
    }"#;
    tx.send(gmcp("NukeFire.Map.Local", snapshot)).unwrap();
    tx.send(gmcp("NukeFire.Map.Local", snapshot)).unwrap();

    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    let transcript = lines.join("\n");
    assert!(
        !transcript.contains("[nukefire-mapper] failed")
            && !transcript.contains("[nukefire-mapper] smudgy:"),
        "mapper reported an error:\n{transcript}"
    );

    let atlases = mapper.list_atlases().await.expect("list atlases");
    let nukefire_atlases: Vec<_> = atlases
        .iter()
        .filter(|atlas| atlas.name == "Nukefire")
        .collect();
    assert_eq!(nukefire_atlases.len(), 1, "atlas upsert is idempotent");
    let atlas_id = nukefire_atlases[0].id;
    assert_eq!(mapper.atlas_storage(&atlas_id), Some(MapStorage::Local));

    let atlas = mapper.get_current_atlas();
    let areas: Vec<_> = atlas
        .areas()
        .filter(|area| area.meta().atlas_id == Some(atlas_id))
        .collect();
    assert_eq!(areas.len(), 1, "repeat snapshots reuse the zone area");
    assert_eq!(areas[0].get_name(), "Tek Angeles");
    assert_eq!(areas[0].get_property("nukefire.zone"), Some("30"));
    let (room, _) = atlas
        .find_room_by_external_id("100")
        .unwrap_or_else(|| panic!("room 100 was mapped; transcript:\n{transcript}"));
    assert_eq!(room.area_id, *areas[0].get_id());
    assert_eq!(mapper.area_storage(&room.area_id), MapStorage::Local);

    let decision_log = find_file(&smudgy_home.join(SERVER), "mapping-decisions.jsonl")
        .expect("debug decision log was created");
    let records: Vec<serde_json::Value> = std::fs::read_to_string(decision_log)
        .expect("read mapper decision log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid decision record"))
        .collect();
    let mutation_id = records
        .iter()
        .find(|record| record["kind"] == "mutation-start" && record["api"] == "mutateArea")
        .and_then(|record| record["mutationId"].as_u64())
        .expect("batched area mutation start is logged");
    assert!(
        records
            .iter()
            .any(|record| { record["kind"] == "mutation-start" && record["api"] == "createAtlas" })
    );
    assert!(
        records
            .iter()
            .any(|record| { record["kind"] == "mutation-start" && record["api"] == "createArea" })
    );
    assert!(records.iter().any(|record| {
        record["kind"] == "mutation-draft-complete"
            && record["mutationId"].as_u64() == Some(mutation_id)
    }));
    assert!(records.iter().any(|record| {
        record["kind"] == "mutation-complete" && record["mutationId"].as_u64() == Some(mutation_id)
    }));
    assert!(
        records
            .iter()
            .any(|record| record["kind"] == "current-location")
    );

    tx.send(RuntimeAction::Shutdown).ok();
}
