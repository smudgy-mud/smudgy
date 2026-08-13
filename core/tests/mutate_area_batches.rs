//! End-to-end coverage of the scripted `mutateArea` batching contract over the
//! >256-operation envelope split: local validation is staged all-or-nothing
//! (an invalid op anywhere publishes nothing), and a backend failure after
//! earlier envelopes were acknowledged surfaces the committed prefix on the
//! thrown error's `committedOperations` property.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use std::sync::Mutex;
use smudgy_cloud::{
    AREA_FORMAT_VERSION, Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, CloudError,
    CloudResult, CreateAreaRequest, MapStorage, Mapper, MapperBackend, Uuid,
    mutation::{AreaMutation, MutationEnvelope, MutationResult, ResourceKind, VersionInfo},
};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// A single-tier local-flavored backend serving areas from memory. Envelope
/// execution succeeds for a budgeted number of calls, then fails permanently
/// with a scripted server verdict — the shape a read-only share produces
/// mid-sequence. Content application is not modeled; the assertions read the
/// script-visible outcome (thrown error payload and the optimistic cache).
struct BudgetedBackend {
    areas: Mutex<Vec<Area>>,
    allowed_mutations: AtomicUsize,
    /// When set, any envelope carrying a `create_room` op is refused with
    /// the server's must-not-exist verdict (409 `structural_conflict`,
    /// `room_number_exists`) — the cross-client collision shape.
    refuse_create_rooms: bool,
}

impl BudgetedBackend {
    fn new(allowed_mutations: usize) -> Self {
        Self {
            areas: Mutex::new(Vec::new()),
            allowed_mutations: AtomicUsize::new(allowed_mutations),
            refuse_create_rooms: false,
        }
    }

    fn refusing_create_rooms() -> Self {
        Self {
            refuse_create_rooms: true,
            ..Self::new(usize::MAX)
        }
    }
}

fn empty_details(area: Area) -> AreaWithDetails {
    AreaWithDetails {
        area,
        format_version: AREA_FORMAT_VERSION,
        content_hash: None,
        properties: vec![],
        rooms: vec![],
        labels: vec![],
        shapes: vec![],
        connections: vec![],
        linked_areas: vec![],
    }
}

#[async_trait]
impl MapperBackend for BudgetedBackend {
    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        self.create_area_at(request, MapStorage::Local).await
    }

    async fn create_area_at(
        &self,
        request: CreateAreaRequest,
        _storage: MapStorage,
    ) -> CloudResult<Area> {
        let area = Area {
            id: AreaId(Uuid::new_v4()),
            user_id: None,
            atlas_id: None,
            name: request.name,
            created_at: Utc::now(),
            rev: 1,
            access: Some(AreaAccess::OWNER),
            owner_nickname: None,
            copied_from_area_id: None,
            copied_from_rev: None,
            copied_at: None,
            family_token: None,
            atlas_name: None,
        };
        self.areas.lock().unwrap().push(area.clone());
        Ok(area)
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        Ok(self.areas.lock().unwrap().clone())
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.areas
            .lock().unwrap()
            .iter()
            .find(|area| area.id == *area_id)
            .cloned()
            .map(empty_details)
            .ok_or(CloudError::NotFoundOrNoAccess)
    }

    async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
        Ok(())
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        self.areas.lock().unwrap().retain(|area| area.id != *area_id);
        Ok(())
    }

    fn local_area_ids(&self) -> std::collections::HashSet<AreaId> {
        self.areas.lock().unwrap().iter().map(|area| area.id).collect()
    }

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        let remaining = self.allowed_mutations.load(Ordering::SeqCst);
        if remaining == 0 {
            return Err(CloudError::PermissionDenied(
                "scripted verdict: no further envelopes".to_string(),
            ));
        }
        if self.refuse_create_rooms
            && envelope
                .payload
                .iter()
                .any(|op| matches!(op, AreaMutation::CreateRoom { .. }))
        {
            return Err(CloudError::StructuralConflict(
                "room_number_exists".to_string(),
            ));
        }
        self.allowed_mutations.store(remaining - 1, Ordering::SeqCst);
        let rev = {
            let mut areas = self.areas.lock().unwrap();
            let area = areas
                .iter_mut()
                .find(|area| area.id == *area_id)
                .ok_or(CloudError::NotFoundOrNoAccess)?;
            area.rev += 1;
            area.rev
        };
        Ok(MutationResult {
            operation_id: envelope.operation_id,
            versions: vec![VersionInfo {
                resource: ResourceKind::Area,
                id: area_id.0,
                rev,
                deleted: false,
            }],
            data: vec![],
        })
    }
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

async fn run_module(server: &str, session: u32, module: &str, command: &str) -> Vec<String> {
    run_module_on(BudgetedBackend::new(1), server, session, module, command).await
}

async fn run_module_on(
    backend: BudgetedBackend,
    server: &str,
    session: u32,
    module: &str,
    command: &str,
) -> Vec<String> {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(server).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(server).join("logs")).unwrap();
    std::fs::write(
        smudgy_home.join(server).join("modules").join("batch.ts"),
        module,
    )
    .unwrap();

    let backend: Arc<dyn MapperBackend + Send + Sync> = Arc::new(backend);
    let mapper = Mapper::new(backend, smudgy_home.join("map-cache"));

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
    let mut lines: Vec<String> = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(60), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };

    tx.send(RuntimeAction::Send(Arc::new(command.to_string())))
        .unwrap();
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();
    drop(events);
    lines
}

/// A later envelope of a split batch fails at the backend after the first was
/// acknowledged: the thrown error carries exactly the committed prefix.
#[tokio::test]
async fn later_envelope_failure_reports_the_committed_prefix() {
    const MODULE: &str = r#"
import { createAlias, echo, mapper } from "smudgy:core";

createAlias("^gobatch$", async () => {
    try {
        const area = await mapper.createArea("Batchland", { storage: "local" });
        try {
            await mapper.mutateArea(area, async (m) => {
                for (let i = 0; i < 300; i += 1) {
                    await m.createRoom({ title: "room " + i });
                }
            });
            echo("BATCH_OK");
        } catch (error) {
            const committed = (error as any).committedOperations;
            echo("BATCH_ERR committed=" +
                (Array.isArray(committed) ? committed.length : "missing"));
        }
    } catch (error) {
        echo("BATCH_SETUP_FAIL " + error);
    }
});
"#;
    let lines = run_module("MutatePrefixTest", 9401, MODULE, "gobatch").await;
    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|line| line == "BATCH_ERR committed=1"),
        "300 drafts split into two envelopes; the first is acknowledged and \
         rides the error, the second fails.\n{transcript}"
    );
}

/// An invalid operation landing in the second envelope of a split batch
/// publishes nothing: staged validation is all-or-nothing across the split.
#[tokio::test]
async fn staged_validation_failure_publishes_no_envelope() {
    const MODULE: &str = r#"
import { createAlias, echo, mapper } from "smudgy:core";

createAlias("^gostage$", async () => {
    try {
        const area = await mapper.createArea("Stageland", { storage: "local" });
        try {
            await mapper.mutateArea(area, async (m) => {
                for (let i = 0; i < 299; i += 1) {
                    await m.createRoom({ title: "room " + i });
                }
                // Lands in the second envelope; no such exit exists.
                await m.setRoomExit(1, [1, 2] as any, { is_hidden: true });
            });
            echo("STAGE_OK");
        } catch (error) {
            const committed = (error as any).committedOperations;
            const rooms = mapper.getAreaById(area.id).room_numbers.length;
            echo("STAGE_ERR rooms=" + rooms + " committed=" +
                (Array.isArray(committed) ? committed.length : "missing"));
        }
    } catch (error) {
        echo("STAGE_SETUP_FAIL " + error);
    }
});
"#;
    let lines = run_module("MutateStageTest", 9402, MODULE, "gostage").await;
    let transcript = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|line| line == "STAGE_ERR rooms=0 committed=missing"),
        "an invalid op in the second envelope must publish neither envelope.\n{transcript}"
    );
}

/// A `createRoom` draft whose number the backend refuses as already taken
/// (the server-side must-not-exist verdict, i.e. another client won the
/// race): the envelope rejection surfaces through the ordinary
/// `mutateArea` failure — a thrown error naming `room_number_exists` with
/// an empty `committedOperations` prefix — never a silent merge.
#[tokio::test]
async fn create_room_collision_verdict_surfaces_through_the_thrown_error() {
    const MODULE: &str = r#"
import { createAlias, echo, mapper } from "smudgy:core";

createAlias("^gocollide$", async () => {
    try {
        const area = await mapper.createArea("Collideland", { storage: "local" });
        try {
            await mapper.mutateArea(area, async (m) => {
                await m.createRoom({ title: "contested" });
            });
            echo("COLLIDE_OK");
        } catch (error) {
            const committed = (error as any).committedOperations;
            const named = String(error).includes("room_number_exists");
            echo("COLLIDE_ERR named=" + named + " committed=" +
                (Array.isArray(committed) ? committed.length : "missing"));
        }
    } catch (error) {
        echo("COLLIDE_SETUP_FAIL " + error);
    }
});
"#;
    let lines = run_module_on(
        BudgetedBackend::refusing_create_rooms(),
        "MutateCollideTest",
        9403,
        MODULE,
        "gocollide",
    )
    .await;
    let transcript = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|line| line == "COLLIDE_ERR named=true committed=0"),
        "the refusal must read as the room-number collision with no committed prefix.\n{transcript}"
    );
}
