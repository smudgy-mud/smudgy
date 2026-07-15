//! The two-account sync-engine story.
//!
//! The grantee runs the REAL client stack — `CredentialSource` →
//! `CloudMapper::with_credentials` → `CachedCloudMapper` → `Mapper::new` —
//! over real HTTP against the contract-shaped mock in `tests/support/`. Ticks
//! are driven deterministically with `sync_now()` + `sync_status().last_sync`
//! barriers (the engine has no periodic poll — it ticks on spawn and on
//! `sync_now`).
#![allow(clippy::too_many_lines, clippy::similar_names)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use smudgy_cloud::cloud_api::{CreateShareRequest, SharePatch, ShareScope};
use smudgy_cloud::mapper::{RoomKey, SyncState};
use smudgy_cloud::{
    AreaId, AtlasId, CachedCloudMapper, CloudApiClient, CloudMapper, Credential, CredentialSource,
    ExitArgs, ExitDirection, ExitStyle, Mapper, MapperBackend, RoomNumber, RoomUpdates,
};
use support::{GrantFlags, GrantScope, MockServer};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Local test plumbing (the wait/tick helpers in mapper::sync_engine's test
// module are private to it, so they are re-implemented here).
// ---------------------------------------------------------------------------

/// Unique temp cache directory, removed on drop.
struct TempCacheDir(PathBuf);

impl TempCacheDir {
    fn new(label: &str) -> Self {
        Self(std::env::temp_dir().join(format!("smudgy-int-sync-{label}-{}", Uuid::new_v4())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempCacheDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Polls `condition` every 2ms for up to ~2s, then asserts it.
async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..1000u32 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(condition(), "condition not met within timeout");
}

/// Builds the full real client stack over an API-key credential, with a huge
/// polling interval (ticks only happen via [`Mapper::sync_now`]), and settles
/// the immediate startup tick so later server-state changes cannot race a
/// tick already in flight.
async fn new_synced_mapper(base_url: &str, api_key: &str, cache_dir: &Path) -> Mapper {
    let credentials = CredentialSource::new(Some(Credential::ApiKey(api_key.to_string())));
    let backend = CachedCloudMapper::new(
        CloudMapper::with_credentials(base_url.to_string(), credentials),
        cache_dir,
    );
    let mapper = Mapper::new(Arc::new(backend), cache_dir);
    wait_until(|| mapper.sync_status().last_sync.is_some()).await;
    mapper
}

/// Forces one full sync tick and waits for it to complete.
async fn tick(mapper: &Mapper) {
    let before = mapper.sync_status().last_sync;
    mapper.sync_now();
    wait_until(|| mapper.sync_status().last_sync != before).await;
}

/// A `CloudApiClient` bearing a fixed API key.
fn api_client(base_url: &str, api_key: &str) -> CloudApiClient {
    CloudApiClient::new(
        base_url,
        CredentialSource::new(Some(Credential::ApiKey(api_key.to_string()))),
    )
}

/// The served `/sync` rev for one area, as seen by `client`'s viewer.
async fn sync_row_rev(client: &CloudApiClient, area_id: AreaId) -> i64 {
    client
        .sync()
        .await
        .expect("/sync fetch")
        .into_iter()
        .find(|row| row.area_id == area_id)
        .expect("sync row for the area")
        .rev
}

/// Recursively collects every `.json` file under `dir`.
fn collect_json_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            out.push(path);
        }
    }
}

/// Whether ANY `.json` file under `dir` (recursively) contains `needle`.
fn any_json_contains(dir: &Path, needle: &[u8]) -> bool {
    let mut files = Vec::new();
    collect_json_files(dir, &mut files);
    files.iter().any(|path| {
        std::fs::read(path)
            .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
    })
}

/// Every cached `.json` file under `dir` belonging to `area_id`
/// (`{area_id}-{rev}-{fingerprint}.json`, possibly nested per viewer).
fn cache_files_for_area(dir: &Path, area_id: AreaId) -> Vec<PathBuf> {
    let prefix = format!("{area_id}-");
    let mut files = Vec::new();
    collect_json_files(dir, &mut files);
    files
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. A grant appears in the grantee's atlas cache via /sync, redacted.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_appears_in_grantee_atlas_via_sync() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Shared Lands");
    server.add_room(area, 1, "Plaza", false);
    server.add_room(area, 2, "Hidden Vault", true); // secret room
    server.add_room(area, 3, "Market", false);
    server.add_exit(area, 1, "North", Some((area, 3)), false);
    server.add_exit(area, 3, "South", Some((area, 1)), true); // secret exit

    let cache_dir = TempCacheDir::new("share-appears");
    let mapper = new_synced_mapper(&server.base_url, &grantee.api_key, cache_dir.path()).await;
    assert!(
        mapper.get_current_atlas().get_area(&area).is_none(),
        "no grant yet: the area must be invisible to the grantee"
    );

    // The grant goes through the real client API, as the owner.
    let owner_client = api_client(&server.base_url, &owner.api_key);
    owner_client
        .create_share(CreateShareRequest {
            grantee_id: grantee.id,
            scope: ShareScope::Area { area_id: area },
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            include_secrets: false,
            can_admin: false,
            host_hints: None,
        })
        .await
        .expect("owner shares to a friend");

    tick(&mapper).await;

    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("area lands in the grantee's atlas cache");
    assert_eq!(cached.get_name(), "Shared Lands");
    assert_eq!(cached.room_count(), 2, "the secret room is filtered out");
    assert!(cached.get_room(&RoomNumber(2)).is_none());

    let room1 = cached.get_room(&RoomNumber(1)).expect("public room 1");
    assert_eq!(room1.get_exits().len(), 1, "the public exit survives");
    let room3 = cached.get_room(&RoomNumber(3)).expect("public room 3");
    assert!(
        room3.get_exits().is_empty(),
        "the secret exit is dropped for the grantee"
    );

    let access = cached.meta().access.expect("access block present");
    assert!(!access.is_owner, "shared, not owned");
    assert!(!access.can_edit && !access.include_secrets);
}

// ---------------------------------------------------------------------------
// 1a. §4.1 atlas un-redaction: an AREA-scope grantee (no atlas-scope grant)
//     sees the un-redacted atlas_id + denormalized atlas_name on the list row
//     and the fetched area; an atlas-less area carries neither.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_area_carries_atlas_id_and_name() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    // Owner files "Ironforge" in a named atlas; "Wilds" stays loose.
    let atlas = server.create_atlas(&owner, "Cities");
    let filed = server.create_area_in_atlas(&owner, "Ironforge", atlas);
    let loose = server.create_area(&owner, "Wilds");

    // AREA-scope grants only — no atlas-scope grant covers the container.
    server.grant(&owner, &grantee, GrantScope::Area(filed), GrantFlags::VIEW_ONLY);
    server.grant(&owner, &grantee, GrantScope::Area(loose), GrantFlags::VIEW_ONLY);

    let grantee_backend = CloudMapper::new(server.base_url.clone(), grantee.api_key.clone());

    // GET /areas list rows.
    let list = grantee_backend.list_areas().await.expect("grantee area list");
    let filed_row = list.iter().find(|a| a.id == filed).expect("filed area listed");
    assert_eq!(
        filed_row.atlas_id,
        Some(AtlasId(atlas)),
        "the area-scope grantee sees the un-redacted atlas_id"
    );
    assert_eq!(filed_row.atlas_name.as_deref(), Some("Cities"));
    let loose_row = list.iter().find(|a| a.id == loose).expect("loose area listed");
    assert!(loose_row.atlas_id.is_none(), "atlas-less area has no atlas_id");
    assert!(
        loose_row.atlas_name.is_none(),
        "atlas-less area has no atlas_name"
    );

    // GET /areas/{id} fetched projection.
    let filed_detail = grantee_backend.get_area(&filed).await.expect("fetch filed");
    assert_eq!(filed_detail.area.atlas_id, Some(AtlasId(atlas)));
    assert_eq!(filed_detail.area.atlas_name.as_deref(), Some("Cities"));
    let loose_detail = grantee_backend.get_area(&loose).await.expect("fetch loose");
    assert!(loose_detail.area.atlas_id.is_none());
    assert!(loose_detail.area.atlas_name.is_none());
}

// ---------------------------------------------------------------------------
// 1b. Room tags roundtrip through the contract: written via the REST backend,
//     normalized to UPPERCASE, projected on GET, and surfaced in the cache.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_tags_roundtrip_through_sync() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("tagowner@example.com", "tagowner", true);
    let area = server.create_area(&owner, "Tagged Area");
    server.add_room(area, 1, "Inn of the Last Home", false);

    // Write tags over the wire (lowercase input must normalize to UPPERCASE).
    let owner_backend = CloudMapper::new(server.base_url.clone(), owner.api_key.clone());
    let room = RoomKey::new(area, RoomNumber(1));
    owner_backend.add_room_tag(&room, "inn").await.expect("add inn");
    owner_backend.add_room_tag(&room, "Peace").await.expect("add peace");

    // A fresh synced client loads the area; tags arrive normalized + sorted.
    let cache_dir = TempCacheDir::new("tags-roundtrip");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache_dir.path()).await;
    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("startup tick caches the area");
    let room_cache = cached.get_room(&RoomNumber(1)).expect("room present");
    let tags: Vec<&str> = room_cache.tags().collect();
    assert_eq!(tags, vec!["INN", "PEACE"], "tags normalized to UPPERCASE and sorted");
    assert!(room_cache.has_tag("inn"), "tag membership is case-insensitive");

    // Removing a tag over the wire moves the rev; a tick refetches and the cache drops it.
    owner_backend.remove_room_tag(&room, "INN").await.expect("remove inn");
    tick(&mapper).await;
    let cached = mapper.get_current_atlas().get_area(&area).expect("area cached");
    let room_cache = cached.get_room(&RoomNumber(1)).expect("room present");
    assert!(!room_cache.has_tag("INN"), "INN gone after sync");
    assert!(room_cache.has_tag("PEACE"), "PEACE retained");
}

// ---------------------------------------------------------------------------
// 1b. A per-exit ExitStyle (the `Stub` variant) survives create + projection.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_style_stub_roundtrips_through_sync() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("styleowner@example.com", "styleowner", true);
    let area = server.create_area(&owner, "Styled Area");
    server.add_room(area, 1, "Landing", false);
    server.add_room(area, 2, "Loft", false);

    // Create a Stub-styled exit over the wire; the create response echoes it.
    let owner_backend = CloudMapper::new(server.base_url.clone(), owner.api_key.clone());
    let created = owner_backend
        .create_room_exit(
            &RoomKey::new(area, RoomNumber(1)),
            ExitArgs {
                from_direction: ExitDirection::North,
                to_area_id: Some(area),
                to_room_number: Some(RoomNumber(2)),
                to_direction: Some(ExitDirection::South),
                style: Some(ExitStyle::Stub),
                ..ExitArgs::default()
            },
        )
        .await
        .expect("create stub exit");
    assert_eq!(created.style, ExitStyle::Stub, "create response carries the style");

    // A fresh synced client loads the area; the Stub style survives projection.
    let cache_dir = TempCacheDir::new("exit-style-roundtrip");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache_dir.path()).await;
    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("startup tick caches the area");
    let room = cached.get_room(&RoomNumber(1)).expect("room present");
    let exit = room
        .get_exits()
        .iter()
        .find(|e| e.id == created.id)
        .expect("exit present in projection");
    assert_eq!(
        exit.style,
        ExitStyle::Stub,
        "Stub style round-trips through the projection",
    );
}

// ---------------------------------------------------------------------------
// 2. include_secrets toggling moves the served rev and forces purge+refetch.
// ---------------------------------------------------------------------------

const SECRET_TITLE: &str = "ZZ-SECRET-VAULT-XYZZY";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn include_secrets_toggle_moves_rev_and_refetches() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Revved Area");
    server.add_room(area, 1, "Plaza", false);

    // The secret room is inserted through the API so the full rev diverges
    // from the public rev (secret-only writes bump only the full rev).
    let owner_mapper = CloudMapper::new(server.base_url.clone(), owner.api_key.clone());
    owner_mapper
        .update_room(
            &RoomKey::new(area, RoomNumber(7)),
            RoomUpdates {
                title: Some(SECRET_TITLE.to_string()),
                is_secret: Some(true),
                ..RoomUpdates::default()
            },
        )
        .await
        .expect("owner upserts the secret room");

    let grant_id = server.grant(&owner, &grantee, GrantScope::Area(area), GrantFlags::VIEW_ONLY);

    let cache_dir = TempCacheDir::new("secrets-toggle");
    let mapper = new_synced_mapper(&server.base_url, &grantee.api_key, cache_dir.path()).await;

    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("startup tick caches the shared area");
    assert_eq!(cached.room_count(), 1, "view-only: secret room hidden");
    assert!(cached.get_room(&RoomNumber(7)).is_none());

    let grantee_client = api_client(&server.base_url, &grantee.api_key);
    let rev_view_only = sync_row_rev(&grantee_client, area).await;

    // Owner raises include_secrets (root, Area-scope grant: allowed).
    let owner_client = api_client(&server.base_url, &owner.api_key);
    let patched = owner_client
        .update_share(
            grant_id,
            SharePatch {
                include_secrets: Some(true),
                ..SharePatch::default()
            },
        )
        .await
        .expect("include_secrets raisable on a root area-scope grant");
    assert!(patched.include_secrets);

    tick(&mapper).await;

    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("area still cached");
    let room7 = cached
        .get_room(&RoomNumber(7))
        .expect("fingerprint change forced a purge+refetch; secret room now visible");
    assert_eq!(room7.get_title(), SECRET_TITLE);

    let rev_with_secrets = sync_row_rev(&grantee_client, area).await;
    assert_ne!(
        rev_with_secrets, rev_view_only,
        "served rev moves when secrets enter the projection (opaque: inequality only)"
    );

    // Sanity-check the disk scanner actually sees the secret bytes while the
    // grantee is cleared — otherwise the final assertion would be vacuous.
    assert!(
        any_json_contains(cache_dir.path(), SECRET_TITLE.as_bytes()),
        "cleared grantee's disk cache holds the secret room"
    );

    // Owner lowers include_secrets again.
    owner_client
        .update_share(
            grant_id,
            SharePatch {
                include_secrets: Some(false),
                ..SharePatch::default()
            },
        )
        .await
        .expect("lowering include_secrets");

    tick(&mapper).await;

    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("area still cached");
    assert!(
        cached.get_room(&RoomNumber(7)).is_none(),
        "secret room gone from the atlas cache after the flag drops"
    );

    let rev_back = sync_row_rev(&grantee_client, area).await;
    assert_ne!(
        rev_back, rev_with_secrets,
        "served rev moves again on the downgrade (opaque: inequality only)"
    );

    assert!(
        !any_json_contains(cache_dir.path(), SECRET_TITLE.as_bytes()),
        "no on-disk cache file may retain the secret room's bytes"
    );
}

// ---------------------------------------------------------------------------
// 3. Revoking the grant purges the atlas cache AND the on-disk cache.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_purges_cache_and_disk() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Borrowed Realm");
    server.add_room(area, 1, "Atrium", false);
    let grant_id = server.grant(&owner, &grantee, GrantScope::Area(area), GrantFlags::VIEW_ONLY);

    let cache_dir = TempCacheDir::new("revoke");
    let mapper = new_synced_mapper(&server.base_url, &grantee.api_key, cache_dir.path()).await;

    assert!(
        mapper.get_current_atlas().get_area(&area).is_some(),
        "startup tick caches the shared area"
    );
    assert!(
        !cache_files_for_area(cache_dir.path(), area).is_empty(),
        "the refetch wrote a disk cache file for the area"
    );

    let owner_client = api_client(&server.base_url, &owner.api_key);
    owner_client
        .revoke_share(grant_id)
        .await
        .expect("owner revokes the grant");

    tick(&mapper).await;

    assert!(
        mapper.get_current_atlas().get_area(&area).is_none(),
        "revoked area must leave the atlas cache"
    );
    assert!(
        cache_files_for_area(cache_dir.path(), area).is_empty(),
        "no disk cache file for the revoked area may remain in the grantee's viewer dir"
    );
}

// ---------------------------------------------------------------------------
// 4. An unknown cross-area link resolves when the neighbour gets shared,
//    without the host area's own rev moving.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_link_resolves_when_neighbour_shared() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area_a = server.create_area(&owner, "Alpha");
    let area_b = server.create_area(&owner, "Beta");
    server.add_room(area_a, 1, "Gatehouse", false);
    server.add_room(area_b, 1, "Far Side", false);
    server.add_exit(area_a, 1, "West", Some((area_b, 1)), false);
    server.grant(&owner, &grantee, GrantScope::Area(area_a), GrantFlags::VIEW_ONLY);

    let cache_dir = TempCacheDir::new("unknown-link");
    let mapper = new_synced_mapper(&server.base_url, &grantee.api_key, cache_dir.path()).await;

    let atlas = mapper.get_current_atlas();
    assert!(atlas.get_area(&area_b).is_none(), "B is hidden");
    let cached_a = atlas.get_area(&area_a).expect("A is shared");
    let room = cached_a.get_room(&RoomNumber(1)).expect("room 1");
    let exit = &room.get_exits()[0];
    assert!(exit.to_unknown, "hidden destination presents as unknown");
    assert!(exit.to_area_id.is_none(), "real target id is withheld");
    let token = exit
        .to_area_token
        .as_deref()
        .expect("hidden destination carries a token");
    assert!(token.starts_with("u_"));

    let grantee_client = api_client(&server.base_url, &grantee.api_key);
    let rev_a_before = sync_row_rev(&grantee_client, area_a).await;

    // Owner shares B as well; A's content does not change at all.
    server.grant(&owner, &grantee, GrantScope::Area(area_b), GrantFlags::VIEW_ONLY);

    tick(&mapper).await;

    let atlas = mapper.get_current_atlas();
    assert!(atlas.get_area(&area_b).is_some(), "B appears in the atlas");
    let cached_a = atlas.get_area(&area_a).expect("A still cached");
    let room = cached_a.get_room(&RoomNumber(1)).expect("room 1");
    let exit = &room.get_exits()[0];
    assert!(!exit.to_unknown, "the unknown link resolved");
    assert_eq!(exit.to_area_id, Some(area_b), "real B id revealed");
    assert!(exit.to_area_token.is_none(), "token gone once resolved");

    let rev_a_after = sync_row_rev(&grantee_client, area_a).await;
    assert_eq!(
        rev_a_after, rev_a_before,
        "A's own rev never moved — the row-set change alone drove the refetch"
    );
}

// ---------------------------------------------------------------------------
// 5. The legacy solo API-key path is unchanged.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_api_key_path_unchanged() {
    let server = MockServer::spawn().await;
    let solo = server.create_user("solo@example.com", "solo", true);

    let cache_dir = TempCacheDir::new("legacy");
    let mapper = new_synced_mapper(&server.base_url, &solo.api_key, cache_dir.path()).await;

    // Create through the mapper (waits for the backend-assigned id).
    let area_id = mapper
        .create_area("Homestead".to_string())
        .await
        .expect("create_area over the legacy API-key credential");

    // List + get via load_all_areas.
    let summary = mapper.load_all_areas().await.expect("load_all_areas");
    assert_eq!(summary.areas.len(), 1);
    assert_eq!(summary.areas[0].area_id, area_id);
    assert_eq!(summary.areas[0].name, "Homestead");

    let cached = mapper
        .get_current_atlas()
        .get_area(&area_id)
        .expect("area in the atlas cache");
    assert_eq!(cached.get_name(), "Homestead");
    assert!(cached.effective_access().is_owner, "solo areas are owned");

    // The sync engine settles in Idle and keeps the area.
    tick(&mapper).await;
    let status = mapper.sync_status();
    assert_eq!(status.state, SyncState::Idle);
    assert!(status.last_error.is_none());
    assert!(
        mapper.get_current_atlas().get_area(&area_id).is_some(),
        "the area survives a sync tick"
    );
}
