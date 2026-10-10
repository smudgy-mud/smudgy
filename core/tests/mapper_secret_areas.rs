//! End-to-end coverage for a Secret read as an area through V8: against a
//! cloud map whose Secret keeps a door on map room 1 into its own room 2,
//! which leads on to map room 3, `getAreaById` returns the Secret's area with
//! its `mapId`, `mapper.areas` stays maps only, a route runs through the
//! Secret, and `setCurrentLocation` takes the Secret's room.

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use smudgy_cloud::{
    AREA_FORMAT_VERSION, Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, CloudError,
    CloudResult, CreateAreaRequest, Mapper, MapperBackend, SourceBundle, Uuid,
    mutation::{MutationEnvelope, MutationResult},
};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const SERVER: &str = "MapperSecretAreas";
const MAP: Uuid = Uuid::from_u128(0x5ec0_0000_0000_4000_8000_0000_0000_0001);
const SECRET: Uuid = Uuid::from_u128(0x5ec0_0000_0000_4000_8000_0000_0000_0002);

fn module() -> String {
    format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";

const failures = [];
const check = (label, actual, expected) => {{
    if (actual !== expected) failures.push(`${{label}}: got [${{actual}}] want [${{expected}}]`);
}};

await mapper.ready();
const map = mapper.getAreaById("{MAP}");
check("map/mapId", map.mapId, undefined);
const secret = mapper.getAreaById("{SECRET}");
check("secret/name", secret.name, "Bookcase");
check("secret/mapId", secret.mapId, map.id);
check("secret/room", secret.room(2)?.title, "Vault");
check("areas/maps only", mapper.areas.some((area) => area.id === secret.id), false);
check(
    "route",
    JSON.stringify(mapper.getPathBetweenRooms(map.id, 1, map.id, 3)),
    JSON.stringify([[map.id, 1], [secret.id, 2], [map.id, 3]]),
);
mapper.setCurrentLocation(secret.id, 2);
const here = mapper.getCurrentLocation();
check("location/area", here?.area, secret.id);
check("location/room", here?.room, 2);

echo(failures.length === 0 ? "SECRET_AREAS_OK" : `SECRET_AREAS_FAIL ${{failures.join(" | ")}}`);
"#
    )
}

/// One cloud map with one Secret, served as the cloud serves it.
struct SecretCloud {
    details: AreaWithDetails,
}

impl SecretCloud {
    fn new() -> Self {
        let map = AreaId(MAP);
        let room = |number: i32, title: &str, x: f32| {
            serde_json::json!({
                "room_number": number, "title": title, "description": "", "color": "",
                "level": 0, "x": x, "y": 0.0, "properties": [], "exits": [], "tags": []
            })
        };
        let exit = |id: u128, to_room: i32, to_source: Option<Uuid>| {
            serde_json::json!({
                "id": Uuid::from_u128(id), "from_direction": "East",
                "to_area_id": map, "to_room_number": to_room, "to_source": to_source,
                "to_direction": null, "to_unknown": false, "path": "", "command": "",
                "weight": 1.0, "connection_id": Uuid::from_u128(id + 0x100),
                "is_hidden": true, "door": null
            })
        };
        let mut vault = room(2, "Vault", 1.0);
        vault["exits"] = serde_json::json!([exit(2, 3, None)]);
        let secret: SourceBundle = serde_json::from_value(serde_json::json!({
            "source": SECRET, "name": "Bookcase", "ownership": "owner", "rev": 1,
            "actions": ["read", "add", "edit", "remove"],
            "rooms": [vault],
            "room_data": [{ "room_number": 1, "exits": [exit(1, 2, Some(SECRET))] }],
        }))
        .expect("the Secret parses");
        let rooms = serde_json::from_value(serde_json::json!([
            room(1, "Library", 0.0),
            room(3, "Garden", 2.0)
        ]))
        .expect("the map's rooms parse");
        Self {
            details: AreaWithDetails {
                room_data: Vec::new(),
                area: Area {
                    id: map,
                    user_id: None,
                    atlas_id: None,
                    name: "Library".to_string(),
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
                    projection_token: Some("p_secret".to_string()),
                },
                format_version: AREA_FORMAT_VERSION,
                properties: vec![],
                rooms,
                labels: vec![],
                shapes: vec![],
                connections: vec![],
                linked_areas: vec![],
                sources: vec![secret],
            },
        }
    }
}

#[async_trait]
impl MapperBackend for SecretCloud {
    async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
        Err(CloudError::PermissionDenied(
            "scripted: no new maps".to_string(),
        ))
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        Ok(vec![self.details.area.clone()])
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        if *area_id == self.details.area.id {
            Ok(self.details.clone())
        } else {
            Err(CloudError::NotFoundOrNoAccess)
        }
    }

    async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
        Ok(())
    }

    async fn delete_area(&self, _area_id: &AreaId) -> CloudResult<()> {
        Err(CloudError::PermissionDenied(
            "scripted: no deletes".to_string(),
        ))
    }

    async fn execute_mutation(
        &self,
        _area_id: &AreaId,
        _envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        Err(CloudError::PermissionDenied(
            "scripted: no writes".to_string(),
        ))
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
async fn a_secret_reads_as_an_area_in_scripts() {
    let smudgy_home = smudgy_home();
    let modules = smudgy_home.join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create modules directory");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).expect("create logs directory");
    std::fs::write(modules.join("secret.ts"), module()).expect("write the module");
    let mapper = Mapper::new(
        Arc::new(SecretCloud::new()),
        smudgy_home.join("map-secret-cloud"),
    );

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_387),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let mut lines = Vec::new();
    let mut located = Vec::new();
    let mut runtime = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut quiet = None;
    loop {
        if quiet.is_none()
            && lines
                .iter()
                .any(|line: &String| line.starts_with("SECRET_AREAS_"))
        {
            quiet = Some(Duration::from_millis(250));
        }
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            break;
        };
        let wait = quiet.map_or(remaining, |quiet: Duration| quiet.min(remaining));
        let Ok(Some(event)) = tokio::time::timeout(wait, events.next()).await else {
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
            SessionEvent::SetCurrentLocation(area_id, room) => located.push((area_id, room)),
            _ => {}
        }
    }
    if let Some(tx) = runtime {
        tx.send(RuntimeAction::Shutdown).ok();
    }

    assert!(
        lines.iter().any(|line| line == "SECRET_AREAS_OK"),
        "a Secret did not read as an area:\n{}",
        lines.join("\n")
    );
    assert_eq!(
        located,
        [(AreaId(SECRET), Some(2))],
        "the marker moves to the Secret's room"
    );
}
