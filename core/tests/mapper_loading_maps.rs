//! Cloud maps that are still loading, as scripts see them through V8. A map
//! the map service lists but can't serve yet is in `mapper.areas` with
//! `loading` set and no rooms; `ready({ id })` and `ready({ all: true })`
//! resolve once it has loaded; a write naming it waits for it; and
//! `areasLoaded` names it when it arrives. `ready({ id })` for an id that
//! names no map resolves too.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use smudgy_cloud::{
    AREA_FORMAT_VERSION, Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, CloudError,
    CloudResult, CompositeBackend, CreateAreaRequest, LocalBackend, Mapper, MapperBackend, Uuid,
    mutation::{MutationEnvelope, MutationResult},
};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const SERVER: &str = "MapperLoadingMaps";

const LOADING_TS: &str = r#"
import { echo, mapper } from "smudgy:core";
import { areasLoaded } from "smudgy:events/map";

const failures = [];
const check = (label, actual, expected) => {
    if (actual !== expected) failures.push(`${label}: got [${actual}] want [${expected}]`);
};
const arrived = [];
areasLoaded.on(({ areaIds }) => arrived.push(...areaIds));

// As a package does: the module finishes loading, and the checks run on, so
// the session's events reach the handler above.
void (async () => {
await mapper.ready();
check("start/keep", mapper.areas.some((area) => area.name === "Keep"), false);
check("start/tower", mapper.areas.find((area) => area.name === "Tower")?.loading, false);

// The refresh lists Keep, which the map service can't serve yet.
await mapper.refreshAreas();
const keep = mapper.areas.find((area) => area.name === "Keep");
check("listed", keep !== undefined, true);
check("loading", keep?.loading, true);
check("no rooms", keep?.roomNumbers.length, 0);
check("storage", keep?.storage, "cloud");
check("by id", mapper.getAreaById(keep.id).loading, true);

// A write naming it waits for it.
let renamed = false;
const renaming = mapper.renameArea(keep.id, "Keep Renamed").then(() => { renamed = true; });
await new Promise((resolve) => setTimeout(resolve, 200));
check("write waits", renamed, false);
check("still loading", keep.loading, true);
await mapper.ready({ id: keep.id });
const loaded = mapper.getAreaById(keep.id);
check("loaded", loaded.loading, false);
check("kept handle is live", keep.loading, false);
await renaming;
check("write landed", mapper.getAreaById(keep.id).name, "Keep Renamed");

await mapper.ready({ all: true });
await new Promise((resolve) => setTimeout(resolve, 300));
check("arrived", arrived.includes(keep.id), true);
check("tower did not arrive again", arrived.some((id) => id !== keep.id), false);

// An id that names no map resolves: it is as ready as it will be.
await mapper.ready({ id: Uuid() });

echo(failures.length === 0 ? "LOADING_MAPS_OK" : `LOADING_MAPS_FAIL ${failures.join(" | ")}`);
})().catch((error) => echo(`LOADING_MAPS_FAIL threw ${error}`));

function Uuid() {
    return "6f1c2a8e-0000-4000-8000-000000000000";
}
"#;

/// A cloud tier that lists Keep from its second list on, and answers Keep's
/// first read busy.
#[derive(Default)]
struct SlowCloud {
    areas: Mutex<Vec<Area>>,
    keep: Mutex<Option<Area>>,
    lists: AtomicUsize,
    busy_reads: AtomicUsize,
}

fn cloud_area(name: &str) -> Area {
    Area {
        id: AreaId(Uuid::new_v4()),
        user_id: Some(Uuid::new_v4()),
        atlas_id: None,
        name: name.to_string(),
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
    }
}

#[async_trait]
impl MapperBackend for SlowCloud {
    async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
        Err(CloudError::InvalidInput("unused".to_string()))
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        if self.lists.fetch_add(1, Ordering::SeqCst) == 1
            && let Some(keep) = self.keep.lock().unwrap().take()
        {
            self.areas.lock().unwrap().push(keep);
        }
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
        if area.name.starts_with("Keep") && self.busy_reads.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(CloudError::ServiceUnavailable {
                message: "Service temporarily unavailable".to_string(),
                retry_after: Some(Duration::from_secs(1)),
            });
        }
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

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        let mut areas = self.areas.lock().unwrap();
        let area = areas
            .iter_mut()
            .find(|area| area.id == *area_id)
            .ok_or(CloudError::NotFoundOrNoAccess)?;
        if let Some(name) = updates.name {
            area.name = name;
            area.rev += 1;
        }
        Ok(())
    }

    async fn delete_area(&self, _area_id: &AreaId) -> CloudResult<()> {
        Ok(())
    }

    async fn execute_mutation(
        &self,
        _area_id: &AreaId,
        _envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        Err(CloudError::InvalidInput("unused".to_string()))
    }
}

fn smudgy_home() -> PathBuf {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    smudgy_core::get_smudgy_home().expect("smudgy home")
}

#[tokio::test]
async fn scripts_see_maps_that_are_still_loading_and_wait_for_them() {
    let smudgy_home = smudgy_home();
    let modules = smudgy_home.join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).unwrap();
    std::fs::write(
        smudgy_home.join(SERVER).join("server.json"),
        r#"{"host":"localhost","port":4000}"#,
    )
    .unwrap();
    std::fs::write(modules.join("loading.ts"), LOADING_TS).unwrap();

    let cloud = Arc::new(SlowCloud::default());
    cloud.areas.lock().unwrap().push(cloud_area("Tower"));
    *cloud.keep.lock().unwrap() = Some(cloud_area("Keep"));
    let map_root = smudgy_home.join("map-test");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_402_u32),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
        graphics_decoder: None,
    });
    let mut events = Box::pin(spawn(params));
    let mut lines = Vec::new();
    let mut runtime = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !lines
        .iter()
        .any(|line: &String| line.starts_with("LOADING_MAPS_"))
    {
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            break;
        };
        let Ok(Some(event)) = tokio::time::timeout(remaining, events.next()).await else {
            break;
        };
        match event.event {
            SessionEvent::RuntimeReady(tx) => runtime = Some(tx),
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(tx) = runtime {
        tx.send(RuntimeAction::Shutdown).ok();
    }
    assert!(
        lines.iter().any(|line| line == "LOADING_MAPS_OK"),
        "scripts did not see the loading map as expected:\n{}",
        lines.join("\n")
    );
}
