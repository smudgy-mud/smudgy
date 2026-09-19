//! Exercise queued local writes through Mapper while another session commits.

use super::*;
use async_trait::async_trait;
use smudgy_cloud::{
    Area, AreaLoadSource, AreaUpdates, AreaWithDetails, CloudResult, CompositeBackend,
    CreateAreaRequest, LocalBackend, SyncRow, backends::local::LocalSnapshot,
};
use tokio::sync::{Notify, Semaphore, watch};

/// Delay writes before the real composite backend chooses a storage tier.
/// Both dispatch entry points use the same gate, so a routing regression cannot
/// bypass the barrier and accidentally write before the other session deletes.
struct GatedBackend {
    backend: CompositeBackend,
    entered: Notify,
    permits: Semaphore,
}

impl GatedBackend {
    async fn gate(&self) {
        self.entered.notify_one();
        self.permits.acquire().await.unwrap().forget();
    }

    async fn wait_for_write(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.notified())
            .await
            .expect("the Mapper dispatched its queued write");
    }
}

#[async_trait]
impl MapperBackend for GatedBackend {
    fn local_backend(&self) -> Option<&dyn MapperBackend> {
        self.backend.local_backend()
    }

    fn local_snapshot(&self) -> Option<Arc<LocalSnapshot>> {
        self.backend.local_snapshot()
    }

    async fn subscribe_local(&self) -> CloudResult<Option<watch::Receiver<u64>>> {
        self.backend.subscribe_local().await
    }

    async fn refresh_local(&self) -> CloudResult<()> {
        self.backend.refresh_local().await
    }

    fn local_area_ids(&self) -> std::collections::HashSet<AreaId> {
        self.backend.local_area_ids()
    }

    fn supports_sync(&self) -> bool {
        self.backend.supports_sync()
    }

    fn cloud_changes(&self) -> Option<watch::Receiver<u64>> {
        self.backend.cloud_changes()
    }

    fn mutation_journal_namespace(&self) -> Option<String> {
        self.backend.mutation_journal_namespace()
    }

    async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
        self.backend.viewer_identity().await
    }

    async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
        self.backend.sync_state().await
    }

    async fn note_sync_rows(&self, rows: &[SyncRow]) {
        self.backend.note_sync_rows(rows).await;
    }

    async fn purge_area(&self, area: &AreaId) {
        self.backend.purge_area(area).await;
    }

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        self.backend.create_area(request).await
    }

    async fn create_area_at(
        &self,
        request: CreateAreaRequest,
        storage: MapStorage,
    ) -> CloudResult<Area> {
        self.backend.create_area_at(request, storage).await
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        self.backend.list_areas().await
    }

    async fn get_area(&self, area: &AreaId) -> CloudResult<AreaWithDetails> {
        self.backend.get_area(area).await
    }

    fn last_area_source(&self, area: &AreaId) -> AreaLoadSource {
        self.backend.last_area_source(area)
    }

    async fn update_area(&self, area: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.backend.update_area(area, updates).await
    }

    async fn delete_area(&self, area: &AreaId) -> CloudResult<()> {
        self.backend.delete_area(area).await
    }

    async fn execute_local_mutation(
        &self,
        area: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        self.gate().await;
        self.backend.execute_local_mutation(area, envelope).await
    }

    async fn execute_mutation(
        &self,
        area: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        self.gate().await;
        self.backend.execute_mutation(area, envelope).await
    }
}

async fn two_mappers(
    server: &support::MockHandle,
    directory: &Path,
) -> (Mapper, Mapper, Arc<GatedBackend>, AreaId) {
    let owner = server.create_user("queued-local@example.com", "queued-local", true);
    let make_backend = || {
        CompositeBackend::new(
            Arc::new(LocalBackend::new(directory.join("local"))),
            Arc::new(CachedCloudMapper::new(
                CloudMapper::with_credentials(
                    server.base_url.clone(),
                    CredentialSource::new(Some(Credential::ApiKey(owner.api_key.clone()))),
                ),
                directory.join("cloud"),
            )),
        )
    };
    let gate = Arc::new(GatedBackend {
        backend: make_backend(),
        entered: Notify::new(),
        permits: Semaphore::new(0),
    });
    let writer = Mapper::new(gate.clone(), directory.join("writer"));
    let other = Mapper::new(Arc::new(make_backend()), directory.join("other"));
    writer.ready().await.unwrap();
    other.ready().await.unwrap();
    wait_until(|| {
        writer.sync_status().last_sync.is_some() && other.sync_status().last_sync.is_some()
    })
    .await;
    let area = other
        .create_area_at("Local".into(), MapDestination::loose(MapStorage::Local))
        .await
        .unwrap();
    wait_until(|| writer.get_current_atlas().get_area(&area).is_some()).await;
    (writer, other, gate, area)
}

fn queue_room(mapper: &Mapper, area: AreaId, room: i32, title: &str) -> Uuid {
    mapper
        .upsert_room(
            RoomKey::new(area, RoomNumber(room)),
            RoomUpdates {
                title: Some(title.into()),
                ..RoomUpdates::default()
            },
        )
        .unwrap()
        .operation_id()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_local_edit_after_another_mapper_deletes_never_reaches_cloud() {
    let server = MockServer::spawn().await;
    let directory = TempCacheDir::new("queued-local-delete");
    let (writer, other, gate, area) = two_mappers(&server, directory.path()).await;
    let operation = queue_room(&writer, area, 1, "Pending local edit");
    gate.wait_for_write().await;
    server.state.lock().http_requests.clear();

    other.delete_area_and_wait(area).await.unwrap();
    assert!(!gate.local_snapshot().unwrap().contains_area(area));
    gate.permits.add_permits(1);
    let result = tokio::time::timeout(Duration::from_secs(5), writer.wait_for_mutation(operation))
        .await
        .expect("deleted local write settles");
    let requests = server.state.lock().http_requests.clone();
    assert!(
        requests.is_empty(),
        "a queued local edit must never reach the cloud after deletion: {requests:?}"
    );
    assert!(result.is_err(), "the deleted local area rejects the edit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_commit_with_local_edits_queued_never_decreases_displayed_revision() {
    let server = MockServer::spawn().await;
    let directory = TempCacheDir::new("queued-local-revision");
    let (writer, other, gate, area) = two_mappers(&server, directory.path()).await;
    let mut operations = Vec::new();
    for index in 0..8 {
        operations.push(queue_room(&writer, area, 1, &format!("Queued {index}")));
    }
    gate.wait_for_write().await;
    let optimistic_revision = writer
        .get_current_atlas()
        .get_area(&area)
        .unwrap()
        .get_rev();

    // A different room proves the foreign base was adopted, while room 1 must
    // still show all eight optimistic edits. No writer save can finish yet.
    let foreign = queue_room(&other, area, 2, "Foreign commit");
    other.wait_for_mutation(foreign).await.unwrap();
    wait_until(|| {
        writer
            .get_current_atlas()
            .get_room(&RoomKey::new(area, RoomNumber(2)))
            .is_some_and(|room| room.get_title() == "Foreign commit")
    })
    .await;
    let projected = writer.get_current_atlas().get_area(&area).unwrap();
    assert_eq!(
        projected.get_room(&RoomNumber(1)).unwrap().get_title(),
        "Queued 7"
    );
    assert!(
        projected.get_rev() >= optimistic_revision,
        "foreign adoption moved the displayed revision backwards: {optimistic_revision} -> {}",
        projected.get_rev()
    );

    // The first held envelope used the old base and retries after a conflict.
    gate.permits.add_permits(operations.len() + 1);
    let mut last_revision = projected.get_rev();
    for operation in operations {
        tokio::time::timeout(Duration::from_secs(5), writer.wait_for_mutation(operation))
            .await
            .expect("queued write settles")
            .unwrap();
        let revision = writer
            .get_current_atlas()
            .get_area(&area)
            .unwrap()
            .get_rev();
        assert!(
            revision >= last_revision,
            "settlement must not lower the displayed revision"
        );
        last_revision = revision;
    }
}
