//! End-to-end coverage for `mapper.createArea(name, { properties })` through
//! V8. Against a real tiered mapper, a local map filed into an atlas and a
//! session map start with their area properties, which `area.data`, the
//! property lookups and a re-read handle see as soon as the call resolves;
//! malformed properties, and more than a map can start with, are refused
//! before any map exists; and the local map's saved document carries them.
//! Against a cloud tier that refuses every property save, the new map is
//! deleted again, and one that cannot be deleted stays: associated with the
//! session's server like any new map, and named in the rejection. A saved map
//! named into no atlas goes in the server's default atlas for its storage.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use smudgy_cloud::{
    AREA_FORMAT_VERSION, Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, Atlas, AtlasId,
    CloudError, CloudMapper, CloudResult, CompositeBackend, CreateAreaRequest, LocalBackend,
    MapStorage, Mapper, MapperBackend, Uuid,
    mutation::{MutationEnvelope, MutationResult},
};
use smudgy_core::models::default_atlases::default_atlas;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const SERVER: &str = "MapperCreateAreaProperties";
const CLOUD_SERVER: &str = "MapperCreateAreaPropertiesCloud";

const CREATE_TS: &str = r#"
import { echo, mapper } from "smudgy:core";

const failures = [];
const check = (label, actual, expected) => {
    if (actual !== expected) failures.push(`${label}: got [${actual}] want [${expected}]`);
};
const ids = (areas) => areas.map((area) => area.id).sort().join(",");

await mapper.ready();
const atlas = await mapper.createAtlas("Nukefire", { storage: "local" });
const deathlands = await mapper.createArea("The Deathlands", {
    storage: "local",
    atlas,
    properties: { "nukefire.area": "the deathlands", "nukefire.mapper": "NukeFire.Map.Local" },
});
const zone = await mapper.createArea("NukeFire Zone 315", {
    storage: "session",
    properties: { "nukefire.zone": "315", "nukefire.mapper": "NukeFire.Map.Local" },
});
const plain = await mapper.createArea("Plain", { storage: "local", properties: {} });

// The handle createArea returns already carries them.
check("local/storage", deathlands.storage, "local");
check("local/data", deathlands.data("nukefire.area"), "the deathlands");
check("session/storage", zone.storage, "session");
check("session/data", zone.data("nukefire.zone"), "315");
check("empty/data", plain.data("nukefire.area"), undefined);

// So do the lookups, in this session, right away.
check("byProperty/local", ids(mapper.findAreasByProperty("nukefire.area", "the deathlands")), deathlands.id);
check("byProperty/session", ids(mapper.findAreasByProperty("nukefire.zone", "315")), zone.id);
check(
    "withProperty",
    ids(mapper.findAreasWithProperty("nukefire.mapper")),
    [deathlands.id, zone.id].sort().join(","),
);
check("withProperty/empty", mapper.findAreasWithProperty("nukefire.area").some((area) => area.id === plain.id), false);

// And a handle read again by id.
check("reread/local", mapper.getAreaById(deathlands.id).data("nukefire.mapper"), "NukeFire.Map.Local");
check("reread/session", mapper.getAreaById(zone.id).data("nukefire.mapper"), "NukeFire.Map.Local");

// Values that are not strings are refused before any map is created.
const before = mapper.areas.length;
for (const [label, properties] of [
    ["number", { k: 1 }],
    ["null", { k: null }],
    ["undefined", { k: undefined }],
    ["object", { k: {} }],
    ["array", ["v"]],
    ["string", "v"],
]) {
    try {
        await mapper.createArea("Refused", { storage: "local", properties });
        failures.push(`${label}: resolved`);
    } catch (error) {
        if (!(error instanceof TypeError)) failures.push(`${label}: threw ${error}`);
    }
}

// So are more properties than a map can start with.
const crowded = Object.fromEntries(Array.from({ length: 257 }, (_, index) => [`k${index}`, "v"]));
try {
    await mapper.createArea("Crowded", { storage: "local", properties: crowded });
    failures.push("crowded: resolved");
} catch (error) {
    if (!String(error).includes("at most 256")) failures.push(`crowded: threw ${error}`);
}
check("refused/none created", mapper.areas.length, before);

echo(failures.length === 0 ? "CREATE_PROPERTIES_OK" : `CREATE_PROPERTIES_FAIL ${failures.join(" | ")}`);
"#;

const CLOUD_TS: &str = r#"
import { echo, mapper } from "smudgy:core";

const failures = [];
const check = (label, actual, expected) => {
    if (actual !== expected) failures.push(`${label}: got [${actual}] want [${expected}]`);
};
const named = (name) => mapper.areas.find((area) => area.name === name);

await mapper.ready();

// A new cloud map whose properties cannot be saved is deleted again.
try {
    await mapper.createArea("Removed", { properties: { "package.key": "removed" } });
    failures.push("removed: resolved");
} catch (error) {
    check("removed/message", String(error.message).endsWith("the map was removed"), true);
}
check("removed/gone", named("Removed"), undefined);

// One that cannot be deleted either stays, without them, and is named.
let message = "";
try {
    await mapper.createArea("Kept", { properties: { "package.key": "kept" } });
    failures.push("kept: resolved");
} catch (error) {
    message = String(error.message);
}
const kept = named("Kept");
check("kept/present", kept !== undefined, true);
check("kept/named", message.includes('"Kept"') && message.includes(kept?.id), true);
check("kept/data", kept?.data("package.key"), undefined);

echo(failures.length === 0 ? `CLOUD_CREATE_OK ${kept.id}` : `CLOUD_CREATE_FAIL ${failures.join(" | ")}`);
"#;

/// A single-tier cloud stand-in: every map it creates reads as cloud-owned,
/// every property save is refused, and a map named "Kept" cannot be deleted.
/// It makes atlases, since a saved map goes in one.
#[derive(Default)]
struct RefusingCloud {
    areas: Mutex<Vec<Area>>,
    atlases: Mutex<Vec<Atlas>>,
}

#[async_trait]
impl MapperBackend for RefusingCloud {
    async fn create_atlas_at(&self, name: &str, _storage: MapStorage) -> CloudResult<Atlas> {
        let atlas = Atlas {
            id: AtlasId(Uuid::new_v4()),
            user_id: None,
            clan_id: None,
            name: name.to_string(),
            created_at: Utc::now(),
            rev: 1,
        };
        self.atlases.lock().unwrap().push(atlas.clone());
        Ok(atlas)
    }

    async fn create_area_at(
        &self,
        request: CreateAreaRequest,
        _storage: MapStorage,
    ) -> CloudResult<Area> {
        self.create_area(request).await
    }

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        let area = Area {
            id: AreaId(Uuid::new_v4()),
            user_id: None,
            atlas_id: request.atlas_id,
            name: request.name,
            created_at: Utc::now(),
            rev: 1,
            access: Some(AreaAccess::OWNER),
            owner_nickname: None,
            copied_from_area_id: None,
            copied_from_rev: None,
            copied_at: None,
            family_token: None,
            clan_id: None,
            clan_name: None,
            actions: None,
            clan_ownership: smudgy_cloud::clan_maps::ClanOwnership::default(),
            atlas_name: None,
            projection_token: None,
        };
        self.areas.lock().unwrap().push(area.clone());
        Ok(area)
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        Ok(self.areas.lock().unwrap().clone())
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        let area = self
            .areas
            .lock()
            .unwrap()
            .iter()
            .find(|area| area.id == *area_id)
            .cloned()
            .ok_or(CloudError::NotFoundOrNoAccess)?;
        Ok(AreaWithDetails {
            room_data: Vec::new(),
            area,
            format_version: AREA_FORMAT_VERSION,
            properties: vec![],
            rooms: vec![],
            labels: vec![],
            shapes: vec![],
            connections: vec![],
            linked_areas: vec![],
            sources: vec![],
        })
    }

    async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
        Ok(())
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        let mut areas = self.areas.lock().unwrap();
        if areas
            .iter()
            .any(|area| area.id == *area_id && area.name == "Kept")
        {
            return Err(CloudError::PermissionDenied(
                "scripted: this map cannot be deleted".to_string(),
            ));
        }
        areas.retain(|area| area.id != *area_id);
        Ok(())
    }

    async fn execute_mutation(
        &self,
        _area_id: &AreaId,
        _envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        Err(CloudError::PermissionDenied(
            "scripted: property saves are refused".to_string(),
        ))
    }
}

/// The shared test home. The override is process-wide and set once, so each
/// test isolates its modules under its own server name.
fn smudgy_home() -> PathBuf {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    smudgy_core::get_smudgy_home().expect("smudgy home")
}

/// What a session reported while its module ran.
struct Transcript {
    lines: Vec<String>,
    /// The maps the session asked the UI to associate with its server.
    associated: Vec<AreaId>,
    /// The atlases it asked the UI to show on its server.
    associated_atlases: Vec<AtlasId>,
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

/// Runs `module` in a new session over `mapper` until it echoes a line that
/// starts with `sentinel`, then collects what the session still reports.
async fn run_module(
    server: &str,
    session: u32,
    module: &str,
    mapper: &Mapper,
    sentinel: &str,
) -> Transcript {
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let modules = smudgy_home.join(server).join("modules");
    std::fs::create_dir_all(&modules).expect("create modules directory");
    std::fs::create_dir_all(smudgy_home.join(server).join("logs")).expect("create logs directory");
    // The session's server, whose settings say where its new maps go.
    std::fs::write(
        smudgy_home.join(server).join("server.json"),
        r#"{"host":"localhost","port":4000}"#,
    )
    .expect("write server settings");
    std::fs::write(modules.join("create.ts"), module).expect("write create module");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut transcript = Transcript {
        lines: Vec::new(),
        associated: Vec::new(),
        associated_atlases: Vec::new(),
    };
    let mut runtime = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    // After the sentinel, a short quiet period lets actions the module queued
    // before it (such as associations) reach the stream.
    let mut quiet_after_sentinel = None;
    loop {
        if quiet_after_sentinel.is_none()
            && transcript
                .lines
                .iter()
                .any(|line| line.starts_with(sentinel))
        {
            quiet_after_sentinel = Some(Duration::from_millis(250));
        }
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            break;
        };
        let wait = quiet_after_sentinel.map_or(remaining, |quiet| quiet.min(remaining));
        let Ok(Some(event)) = tokio::time::timeout(wait, events.next()).await else {
            break;
        };
        match event.event {
            SessionEvent::RuntimeReady(tx) => runtime = Some(tx),
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut transcript.lines),
            SessionEvent::MapAreaCreated(area_id) => transcript.associated.push(area_id),
            SessionEvent::MapAtlasCreated(atlas_id) => transcript.associated_atlases.push(atlas_id),
            _ => {}
        }
    }
    if let Some(tx) = runtime {
        tx.send(RuntimeAction::Shutdown).ok();
    }
    transcript
}

#[tokio::test]
async fn create_area_starts_local_and_session_maps_with_their_properties() {
    let smudgy_home = smudgy_home();

    // A tiered mapper: session maps exist only behind the composite. The
    // cloud tier is never reached.
    let map_root = smudgy_home.join("map-test");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    let transcript = run_module(SERVER, 9_385, CREATE_TS, &mapper, "CREATE_PROPERTIES_").await;
    assert!(
        transcript
            .lines
            .iter()
            .any(|line| line == "CREATE_PROPERTIES_OK"),
        "createArea did not start the maps with their properties:\n{}",
        transcript.lines.join("\n")
    );

    // A saved map named into no atlas goes in the server's default atlas for
    // its storage, made as "Loose maps" and stored in the server's settings;
    // a session map is in none, and a named atlas keeps its map.
    let atlas = mapper.get_current_atlas();
    let folder_of = |name: &str| {
        atlas
            .areas()
            .find(|area| area.get_name() == name)
            .unwrap_or_else(|| panic!("{name}"))
            .meta()
            .atlas_id
    };
    let loose = folder_of("Plain").expect("Plain is filed");
    let listed = mapper.list_atlases().await.expect("folders");
    let name_of = |id| {
        listed
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.name.clone())
    };
    assert_eq!(name_of(loose).as_deref(), Some("Loose maps"));
    assert_eq!(
        default_atlas(SERVER, MapStorage::Local).expect("server settings"),
        Some(loose)
    );
    assert_eq!(folder_of("NukeFire Zone 315"), None);
    assert_eq!(
        name_of(folder_of("The Deathlands").expect("filed")).as_deref(),
        Some("Nukefire")
    );

    // The local map's saved document carries them: an explicit refresh
    // rescans the map files on disk.
    let deathlands = *mapper
        .get_current_atlas()
        .areas()
        .find(|area| area.get_name() == "The Deathlands")
        .expect("the local map")
        .get_id();
    let reopened = LocalBackend::new(map_root.join("local"));
    reopened
        .refresh()
        .await
        .expect("read the local maps from disk");
    let saved = reopened
        .get_area(&deathlands)
        .await
        .expect("saved local map");
    let mut properties: Vec<(&str, &str)> = saved
        .properties
        .iter()
        .map(|property| (property.name.as_str(), property.value.as_str()))
        .collect();
    properties.sort_unstable();
    assert_eq!(
        properties,
        [
            ("nukefire.area", "the deathlands"),
            ("nukefire.mapper", "NukeFire.Map.Local"),
        ]
    );
}

#[tokio::test]
async fn a_cloud_map_whose_properties_fail_is_deleted_or_associated_and_named() {
    let smudgy_home = smudgy_home();
    let backend = Arc::new(RefusingCloud::default());
    let mapper = Mapper::new(backend.clone(), smudgy_home.join("map-refusing-cloud"));

    let transcript = run_module(CLOUD_SERVER, 9_386, CLOUD_TS, &mapper, "CLOUD_CREATE_").await;
    let kept = transcript
        .lines
        .iter()
        .find_map(|line| line.strip_prefix("CLOUD_CREATE_OK "))
        .unwrap_or_else(|| {
            panic!(
                "createArea did not undo or report its cloud maps:\n{}",
                transcript.lines.join("\n")
            )
        });
    let kept = AreaId(Uuid::parse_str(kept).expect("the kept map's id"));

    assert_eq!(
        transcript.associated,
        [kept],
        "only the map that stayed is associated with the server"
    );
    let names: Vec<String> = backend
        .areas
        .lock()
        .unwrap()
        .iter()
        .map(|area| area.name.clone())
        .collect();
    assert_eq!(names, ["Kept"], "the other map was deleted again");

    // Both went in the server's cloud default, made once for the first map,
    // named for the server, stored in its settings and shown on it.
    let atlases = backend.atlases.lock().unwrap().clone();
    assert_eq!(atlases.len(), 1, "one default atlas for both maps");
    assert_eq!(
        atlases[0].name,
        format!("Loose maps ({CLOUD_SERVER})"),
        "the cloud default carries the server's name"
    );
    assert_eq!(
        default_atlas(CLOUD_SERVER, MapStorage::Cloud).expect("server settings"),
        Some(atlases[0].id)
    );
    assert_eq!(transcript.associated_atlases, [atlases[0].id]);
    let kept_atlas = backend
        .areas
        .lock()
        .unwrap()
        .iter()
        .find(|area| area.id == kept)
        .and_then(|area| area.atlas_id);
    assert_eq!(kept_atlas, Some(atlases[0].id));
}
