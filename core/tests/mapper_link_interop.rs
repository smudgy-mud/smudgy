//! End-to-end coverage for mapper link arguments crossing the V8/Rust boundary.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_cloud::{LocalBackend, Mapper, MapperBackend};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const SERVER: &str = "MapperLinkInterop";

const LINK_INTEROP_TS: &str = r#"
import { echo, mapper } from "smudgy:core";

const endpoint = (room_number: number, side: "East" | "West") => ({
    room_number,
    side,
    port_offset: 0.5,
    port_mode: "AutoPinned" as const,
});

const link = (areaId: readonly [number, number], from: number, to: number) => ({
    endpoint_a: endpoint(from, "East"),
    endpoint_b: endpoint(to, "West"),
    traversals: [
        {
            room_number: from,
            from_direction: "East" as const,
            to_direction: "West" as const,
            to_area_id: areaId,
            to_room_number: to,
        },
        {
            room_number: to,
            from_direction: "West" as const,
            to_direction: "East" as const,
            to_area_id: areaId,
            to_room_number: from,
        },
    ],
});

const area = await mapper.createArea("Link interop", { storage: "local" });
const rooms = [];
for (let index = 0; index < 4; index += 1) {
    rooms.push(await mapper.createRoom(area, {
        title: `Room ${index + 1}`,
        x: index,
        y: 0,
        level: 0,
    }));
}

try {
    await mapper.createLink(area, link(area.id, rooms[0], rooms[1]));
    echo("DIRECT_LINK_OK");
} catch (error) {
    echo(`DIRECT_LINK_ERROR ${error instanceof Error ? error.message : String(error)}`);
}

try {
    await mapper.mutateArea(area, async (mutation) => {
        await mutation.createLink(link(area.id, rooms[2], rooms[3]));
    }, { description: "Create batched interop link" });
    echo("BATCH_LINK_OK");
} catch (error) {
    echo(`BATCH_LINK_ERROR ${error instanceof Error ? error.message : String(error)}`);
}
"#;

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

#[tokio::test]
async fn link_creation_accepts_area_ids_returned_by_mapper() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");

    let modules = smudgy_home.join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create modules directory");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).expect("create logs directory");
    // The session's server, whose settings say where its new maps go.
    std::fs::write(
        smudgy_home.join(SERVER).join("server.json"),
        r#"{"host":"localhost","port":4000}"#,
    )
    .expect("write server settings");
    std::fs::write(modules.join("link-interop.ts"), LINK_INTEROP_TS)
        .expect("write link interop module");

    let map_root = smudgy_home.join("map-test");
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(LocalBackend::new(map_root.join("local")));
    let mapper = Mapper::new(backend, map_root.join("cache"));
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_361_u32),
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let tx = loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("timed out waiting for mapper interop module");
        let event = tokio::time::timeout(remaining, events.next())
            .await
            .expect("timed out waiting for mapper interop module")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    while !(lines.iter().any(|line| line.starts_with("DIRECT_LINK_"))
        && lines.iter().any(|line| line.starts_with("BATCH_LINK_")))
    {
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            break;
        };
        let Ok(Some(event)) = tokio::time::timeout(remaining, events.next()).await else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|line| line == "DIRECT_LINK_OK"),
        "direct mapper.createLink must accept mapper-returned area ids.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|line| line == "BATCH_LINK_OK"),
        "AreaMutator.createLink must accept mapper-returned area ids.\nTranscript:\n{transcript}"
    );

    let atlas = mapper.get_current_atlas();
    let area = atlas
        .areas()
        .find(|area| area.get_name() == "Link interop")
        .expect("script-created area");
    assert_eq!(area.get_connections().len(), 2);
    assert_eq!(
        area.get_rooms()
            .iter()
            .map(|room| room.get_exits().len())
            .sum::<usize>(),
        4
    );
}
