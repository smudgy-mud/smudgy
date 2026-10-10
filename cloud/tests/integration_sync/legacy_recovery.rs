use super::*;
use sha2::{Digest, Sha256};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_failure_allows_fresh_sync_and_account_switch_hides_recovery_immediately() {
    let server = MockServer::spawn().await;
    let user = server.create_user("recovery-failure@example.com", "recovery-failure", true);
    let other = server.create_user("recovery-other@example.com", "recovery-other", true);
    let area = server.create_area(&user, "Imported map");
    server.add_room(area, 1, "Imported room");
    let cache = TempCacheDir::new("legacy-recovery-failure");
    let namespace = hex::encode(Sha256::digest(server.base_url.as_bytes()));
    let old_queue = cache
        .path()
        .join("pending-mutations/servers")
        .join(namespace)
        .join("viewers")
        .join(user.id.to_string());
    std::fs::create_dir_all(&old_queue).unwrap();
    std::fs::write(old_queue.join("old.json"), b"private draft").unwrap();
    std::fs::write(old_queue.join("recovery"), b"blocks archive creation").unwrap();
    let credentials = CredentialSource::new(Some(Credential::ApiKey(user.api_key.clone())));
    let backend = CachedCloudMapper::new(
        CloudMapper::with_credentials(server.base_url.clone(), credentials.clone()),
        cache.path(),
    );
    let mapper = Mapper::new(Arc::new(backend), cache.path());
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    assert!(mapper.legacy_cloud_recovery().unwrap().incomplete);
    assert!(server.mutation_requests().is_empty());
    let operation = mapper
        .upsert_room(
            RoomKey::new(area, RoomNumber(1)),
            RoomUpdates {
                title: Some("Fresh edit despite archive failure".into()),
                ..RoomUpdates::default()
            },
        )
        .unwrap();
    mapper
        .wait_for_mutation(operation.operation_id().unwrap())
        .await
        .unwrap();
    assert_eq!(server.mutation_requests().len(), 1);
    assert_eq!(
        std::fs::read(old_queue.join("old.json")).unwrap(),
        b"private draft"
    );
    credentials.set(Some(Credential::ApiKey(other.api_key.clone())));
    assert!(
        mapper.legacy_cloud_recovery().is_none(),
        "hide the old viewer before the next sync tick"
    );
    tick(&mapper).await;
    assert!(mapper.legacy_cloud_recovery().is_none());
    assert_eq!(
        std::fs::read(old_queue.join("old.json")).unwrap(),
        b"private draft"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_reloads_cloud_maps_without_old_overlays_and_new_edits_sync() {
    let server = MockServer::spawn().await;
    let user = server.create_user("upgrade@example.com", "upgrade", true);
    let area = server.create_area(&user, "Imported map");
    server.add_room(area, 1, "Imported room");
    let cache = TempCacheDir::new("legacy-recovery");
    let namespace = hex::encode(Sha256::digest(server.base_url.as_bytes()));
    let old_queue = cache
        .path()
        .join("pending-mutations/servers")
        .join(namespace)
        .join("viewers")
        .join(user.id.to_string());
    std::fs::create_dir_all(&old_queue).unwrap();
    let raw = br#"{"schema_version":3,"ops":[{"upsert_room":{"room_number":1,"is_secret":true,"title":"Unsent private draft"}}]}"#;
    std::fs::write(old_queue.join("old.json"), raw).unwrap();
    let legacy_cache = cache.path().join("v2").join(user.id.to_string());
    std::fs::create_dir_all(&legacy_cache).unwrap();
    std::fs::write(legacy_cache.join("old-map.json"), b"cached private draft").unwrap();

    let mapper = new_synced_mapper(&server.base_url, &user.api_key, cache.path()).await;
    let notice = mapper.legacy_cloud_recovery().unwrap();
    assert!(!notice.incomplete);
    assert_eq!(
        std::fs::read(notice.folder.join("journal/old.json")).unwrap(),
        raw
    );
    assert_eq!(
        std::fs::read(notice.folder.join("cache/v2/old-map.json")).unwrap(),
        b"cached private draft"
    );
    assert_eq!(
        mapper
            .get_current_atlas()
            .get_room(&RoomKey::new(area, RoomNumber(1)))
            .unwrap()
            .get_title(),
        "Imported room"
    );
    assert!(
        server.mutation_requests().is_empty(),
        "no legacy mutation reaches the service"
    );
    let operation = mapper
        .upsert_room(
            RoomKey::new(area, RoomNumber(1)),
            RoomUpdates {
                title: Some("Fresh edit".into()),
                ..RoomUpdates::default()
            },
        )
        .unwrap();
    mapper
        .wait_for_mutation(operation.operation_id().unwrap())
        .await
        .unwrap();
    assert_eq!(server.mutation_requests().len(), 1);
    assert!(mapper.wait_for_sync_completion(5).await.unwrap());
    drop(mapper);

    let restarted = new_synced_mapper(&server.base_url, &user.api_key, cache.path()).await;
    assert_eq!(restarted.legacy_cloud_recovery(), Some(notice));
    assert_eq!(
        restarted
            .get_current_atlas()
            .get_room(&RoomKey::new(area, RoomNumber(1)))
            .unwrap()
            .get_title(),
        "Fresh edit"
    );
    assert_eq!(
        server.mutation_requests().len(),
        1,
        "restart sends no old queue"
    );
}
