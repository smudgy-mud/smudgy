//! Creating maps with their properties over the real client stack. A cloud
//! map is created and then receives its properties as one acknowledged
//! mutation. More properties than that mutation carries are refused on every
//! tier before anything is made. A refused save deletes the new map again,
//! leaving nothing on the server, in the atlas cache, on disk, or in the
//! pending queue; and when that delete fails too, the map stays, without its
//! properties, and the error names it.

use super::*;
use smudgy_cloud::mapper::AreaSaveStatus;
use smudgy_cloud::{CompositeBackend, CreateAreaError, LocalBackend, MAX_MUTATION_OPERATIONS};

fn initial_properties(count: usize) -> BTreeMap<String, String> {
    (0..count)
        .map(|index| (format!("package.key{index:03}"), format!("value {index}")))
        .collect()
}

/// Journal records left in the mapper's pending-mutation directory.
fn journal_records(cache_dir: &Path) -> Vec<PathBuf> {
    let mut records = Vec::new();
    collect_json_files(&cache_dir.join("pending-mutations"), &mut records);
    records
}

/// The areas the client has asked the server to delete, in request order.
fn deleted_area_ids(server: &support::MockHandle) -> Vec<AreaId> {
    server
        .state
        .lock()
        .http_requests
        .iter()
        .filter(|(method, _)| method == "DELETE")
        .filter_map(|(_, path)| path.strip_prefix("/areas/"))
        .map(|id| AreaId(Uuid::parse_str(id).expect("an area id")))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cloud_map_starts_with_as_many_properties_as_one_mutation_carries() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("keyed@example.com", "keyed", true);
    let cache_dir = TempCacheDir::new("create-properties");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache_dir.path()).await;

    let properties = initial_properties(MAX_MUTATION_OPERATIONS);
    let id = mapper
        .create_area_at_with_properties(
            "Keyed".to_string(),
            MapDestination::loose(MapStorage::Cloud),
            properties.clone(),
        )
        .await
        .expect("create with properties");

    assert_eq!(server.mutation_requests().len(), 1, "one envelope");
    let stored: BTreeMap<String, String> = server.state.lock().areas[&id.0]
        .properties
        .iter()
        .map(|(name, property)| (name.clone(), property.value.clone()))
        .collect();
    assert_eq!(stored, properties);
    let cached = mapper
        .get_current_atlas()
        .get_area(&id)
        .expect("the new map is cached");
    assert_eq!(cached.get_property("package.key255"), Some("value 255"));
    assert_eq!(mapper.area_save_status(id), AreaSaveStatus::Saved);
    assert!(journal_records(cache_dir.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_many_initial_properties_are_refused_before_any_tier_creates_a_map() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("crowded@example.com", "crowded", true);
    let directory = TempCacheDir::new("create-too-many");
    let mapper = Mapper::new(
        Arc::new(CompositeBackend::new(
            Arc::new(LocalBackend::new(directory.path().join("local"))),
            Arc::new(CachedCloudMapper::new(
                CloudMapper::with_credentials(
                    server.base_url.clone(),
                    CredentialSource::new(Some(Credential::ApiKey(owner.api_key.clone()))),
                ),
                directory.path().join("cloud"),
            )),
        )),
        directory.path().join("mapper"),
    );
    mapper.ready().await.expect("maps load");
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    server.state.lock().http_requests.clear();

    let too_many = initial_properties(MAX_MUTATION_OPERATIONS + 1);
    for storage in [MapStorage::Cloud, MapStorage::Local, MapStorage::Session] {
        let error = mapper
            .create_area_at_with_properties(
                format!("Crowded {storage}"),
                MapDestination::loose(storage),
                too_many.clone(),
            )
            .await
            .expect_err("too many properties");
        assert!(
            matches!(
                &error,
                CreateAreaError::NotCreated(CloudError::InvalidInput(reason))
                    if reason.contains("at most 256")
            ),
            "{storage}: {error:?}"
        );
    }
    let error = mapper
        .create_area_with_properties("Crowded".to_string(), too_many)
        .await
        .expect_err("too many properties in the default tier");
    assert_eq!(
        error.to_string(),
        "Invalid input: a new map can start with at most 256 properties, not 257"
    );

    assert_eq!(mapper.get_current_atlas().areas().count(), 0);
    assert!(mapper.local_area_ids().is_empty());
    assert!(mapper.session_area_ids().is_empty());
    assert!(server.state.lock().areas.is_empty());
    let writes: Vec<_> = server
        .state
        .lock()
        .http_requests
        .iter()
        .filter(|(method, _)| method != "GET")
        .cloned()
        .collect();
    assert!(writes.is_empty(), "no write reached the server: {writes:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_property_save_deletes_the_new_cloud_map() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("refused@example.com", "refused", true);
    let cache_dir = TempCacheDir::new("create-refused");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache_dir.path()).await;

    server.refuse_next_mutations(1);
    let error = mapper
        .create_area_with_properties("Keyed".to_string(), initial_properties(2))
        .await
        .expect_err("the property save was refused");
    let CreateAreaError::PropertiesNotSaved { reason } = &error else {
        panic!("expected the new map to be removed: {error:?}");
    };
    assert!(reason.contains("injected refusal"), "{reason}");
    assert!(
        error.to_string().ends_with("the map was removed"),
        "{error}"
    );

    let [id] = deleted_area_ids(&server)[..] else {
        panic!("expected exactly one delete");
    };
    assert!(
        server.state.lock().areas.is_empty(),
        "the server holds no map"
    );
    assert!(mapper.get_current_atlas().get_area(&id).is_none());
    assert!(cache_files_for_area(cache_dir.path(), id).is_empty());
    assert!(journal_records(cache_dir.path()).is_empty());
    assert_eq!(mapper.area_save_status(id), AreaSaveStatus::Saved);
    assert_eq!(mapper.get_sync_stats().pending_operations(), 0);

    tick(&mapper).await;
    assert_eq!(mapper.get_current_atlas().areas().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_cloud_map_that_cannot_be_deleted_stays_without_its_properties() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("kept@example.com", "kept", true);
    let cache_dir = TempCacheDir::new("create-kept");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache_dir.path()).await;

    server.refuse_next_mutations(1);
    server.fail_next_area_deletes(1);
    let error = mapper
        .create_area_at_with_properties(
            "Keyed".to_string(),
            MapDestination::loose(MapStorage::Cloud),
            initial_properties(2),
        )
        .await
        .expect_err("the property save was refused");
    let CreateAreaError::AreaKept {
        area_id: id,
        name,
        reason,
        removal,
    } = &error
    else {
        panic!("expected the new map to stay: {error:?}");
    };
    let id = *id;
    assert_eq!(error.kept_area(), Some(id));
    assert_eq!(name, "Keyed");
    assert!(reason.contains("injected refusal"), "{reason}");
    assert!(
        removal.to_string().contains("injected delete failure"),
        "{removal}"
    );
    let message = error.to_string();
    assert!(
        message.contains("\"Keyed\"") && message.contains(&id.to_string()),
        "{message}"
    );

    // The map stays as the server holds it, without the properties.
    assert!(server.state.lock().areas[&id.0].properties.is_empty());
    let cached = mapper
        .get_current_atlas()
        .get_area(&id)
        .expect("the kept map stays cached");
    assert_eq!(cached.get_property("package.key000"), None);
    assert_eq!(mapper.area_save_status(id), AreaSaveStatus::Saved);
    assert!(journal_records(cache_dir.path()).is_empty());

    // Sync settles the unconfirmed delete against the server, which still
    // holds the map, and the player can then remove it.
    tick(&mapper).await;
    assert!(mapper.get_current_atlas().get_area(&id).is_some());
    mapper
        .delete_area(id)
        .await
        .expect("the kept map can be removed");
    assert!(server.state.lock().areas.is_empty());
    assert!(mapper.get_current_atlas().get_area(&id).is_none());
}
