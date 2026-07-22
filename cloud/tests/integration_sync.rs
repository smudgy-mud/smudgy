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
use smudgy_cloud::mutation::{AreaMutation, MutationEnvelope, MutationResult, Precondition, ResourceKind};
use smudgy_cloud::{
    AreaId, AtlasId, CachedCloudMapper, CloudApiClient, CloudError, CloudMapper, ConnectionDash,
    ConnectionKind, ConnectionRouting, Credential, CredentialSource, ExitArgs, ExitDirection,
    ExitUpdates, Mapper, MapperBackend, PortMode, RoomNumber, RoomSide, RoomUpdates,
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

/// Executes one compound envelope as `backend`'s viewer, preconditioned on
/// the viewer's freshly-fetched projected revision — the direct-wire way to
/// drive `POST /areas/{id}/mutations` outside a `Mapper`.
async fn execute_ops(
    backend: &CloudMapper,
    area: AreaId,
    payload: Vec<AreaMutation>,
) -> MutationResult {
    let current = backend.get_area(&area).await.expect("fetch for the precondition");
    backend
        .execute_mutation(
            &area,
            &MutationEnvelope {
                operation_id: Uuid::new_v4(),
                preconditions: vec![Precondition {
                    resource: ResourceKind::Area,
                    id: area.0,
                    expected_rev: current.area.rev,
                    access_fingerprint: current.area.access.map(|access| access.fingerprint()),
                }],
                payload,
            },
        )
        .await
        .expect("envelope accepted")
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
    // A secret SELF-LOOP: reciprocal with the public exit it would form one
    // Connection, and the §6 closure would then scrub both members. This
    // fixture is about per-exit secrecy, so it stays its own group.
    server.add_exit(area, 3, "South", Some((area, 3)), true); // secret exit

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
// 1b. Room tags roundtrip through the contract: written through a Mapper
//     (envelopes on the compound endpoint), normalized to UPPERCASE,
//     projected on GET, and surfaced in a second client's cache.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_tags_roundtrip_through_sync() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("tagowner@example.com", "tagowner", true);
    let area = server.create_area(&owner, "Tagged Area");
    server.add_room(area, 1, "Inn of the Last Home", false);

    // Write tags through the mapper (lowercase input must normalize to
    // UPPERCASE); the writes ride envelopes on the compound endpoint.
    let writer_dir = TempCacheDir::new("tags-writer");
    let writer = new_synced_mapper(&server.base_url, &owner.api_key, writer_dir.path()).await;
    let room = RoomKey::new(area, RoomNumber(1));
    writer.add_room_tag(room.clone(), "inn".to_string());
    writer.add_room_tag(room.clone(), "Peace".to_string());
    assert!(
        writer
            .wait_for_sync_completion(10)
            .await
            .expect("tag writes acknowledged"),
        "pending queue drains"
    );

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

    // Removing a tag moves the rev; the reader's tick refetches and drops it.
    writer.remove_room_tag(room, "INN".to_string());
    assert!(
        writer
            .wait_for_sync_completion(10)
            .await
            .expect("tag removal acknowledged"),
        "pending queue drains"
    );
    tick(&mapper).await;
    let cached = mapper.get_current_atlas().get_area(&area).expect("area cached");
    let room_cache = cached.get_room(&RoomNumber(1)).expect("room present");
    assert!(!room_cache.has_tag("INN"), "INN gone after sync");
    assert!(room_cache.has_tag("PEACE"), "PEACE retained");
}

// ---------------------------------------------------------------------------
// 1c. The v2 Connection contract through the real stack: a bidirectional
//     pair created through the Mapper arrives via sync as ONE Connection
//     with two member exits, linkage intact and geometry populated.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_pair_roundtrips_as_one_connection() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("pairowner@example.com", "pairowner", true);
    let area = server.create_area(&owner, "Paired Area");
    server.add_room(area, 1, "Landing", false);
    server.add_room(area, 2, "Loft", false);

    // Create both directions through the mapper (client-minted ids; the
    // wire lands on the compound endpoint). The server auto-pairs the
    // second exit onto the first exit's Connection.
    let writer_dir = TempCacheDir::new("pair-writer");
    let writer = new_synced_mapper(&server.base_url, &owner.api_key, writer_dir.path()).await;
    let out_id = writer
        .create_exit(
            RoomKey::new(area, RoomNumber(1)),
            ExitArgs {
                from_direction: ExitDirection::North,
                to_area_id: Some(area),
                to_room_number: Some(RoomNumber(2)),
                to_direction: Some(ExitDirection::South),
                ..ExitArgs::default()
            },
        )
        .await
        .expect("create resolves immediately with a minted id");
    let back_id = writer
        .create_exit(
            RoomKey::new(area, RoomNumber(2)),
            ExitArgs {
                from_direction: ExitDirection::South,
                to_area_id: Some(area),
                to_room_number: Some(RoomNumber(1)),
                to_direction: Some(ExitDirection::North),
                ..ExitArgs::default()
            },
        )
        .await
        .expect("reverse create resolves immediately");
    assert!(
        writer
            .wait_for_sync_completion(10)
            .await
            .expect("exit creates acknowledged"),
        "pending queue drains"
    );

    // A fresh synced client sees ONE Connection with both exits as members.
    let cache_dir = TempCacheDir::new("pair-roundtrip");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache_dir.path()).await;
    let exported = mapper.export_area(area).await.expect("projection fetch");
    assert_eq!(
        exported.connections.len(),
        1,
        "the reciprocal creates auto-paired into one Connection"
    );
    let connection = &exported.connections[0];

    let exits: Vec<_> = exported
        .rooms
        .iter()
        .flat_map(|room| room.exits.iter())
        .collect();
    assert_eq!(exits.len(), 2);
    for id in [out_id, back_id] {
        let exit = exits
            .iter()
            .find(|exit| exit.id == id)
            .expect("client-minted exit id survives the roundtrip");
        assert_eq!(
            exit.connection_id, connection.id,
            "every member's connection_id resolves into `connections`"
        );
    }

    // Geometry-relevant fields: canonical order (lower room is endpoint A),
    // §1.5 anchors from each member's from_direction, §4.3 solo-wall slots,
    // creation defaults for routing/appearance.
    assert_eq!(connection.endpoint_a.room_number, RoomNumber(1));
    assert_eq!(connection.endpoint_a.side, RoomSide::North);
    assert!((connection.endpoint_a.port_offset - 0.5).abs() < 1e-6);
    assert_eq!(connection.endpoint_a.port_mode, PortMode::AutoPinned);
    let endpoint_b = connection.endpoint_b.expect("two-ender has endpoint B");
    assert_eq!(endpoint_b.room_number, RoomNumber(2));
    assert_eq!(endpoint_b.side, RoomSide::South);
    assert_eq!(connection.kind, ConnectionKind::Internal);
    assert_eq!(connection.routing, ConnectionRouting::Simple);
    assert_eq!(connection.dash, ConnectionDash::Solid);
    assert!(connection.route_points.is_empty());

    // The cache resolves it to one rendered, bidirectional link.
    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("startup tick caches the area");
    let rendered = cached.get_room_connections();
    assert_eq!(rendered.len(), 1, "one rendered Connection");
    assert!(rendered[0].is_bidirectional, "two members, no arrow");
}

// ---------------------------------------------------------------------------
// 1c². §3.2 on the wire: a pair member cannot be retargeted in place — the
//      server refuses with 409 structural_conflict "unlink_before_edit",
//      and traversal-only edits stay legal.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_member_retarget_is_refused_with_unlink_before_edit() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("unlinkowner@example.com", "unlinkowner", true);
    let area = server.create_area(&owner, "Linked Lands");
    server.add_room(area, 1, "Here", false);
    server.add_room(area, 2, "There", false);
    server.add_room(area, 3, "Elsewhere", false);
    let out_id = server.add_exit(area, 1, "East", Some((area, 2)), false);
    server.add_exit(area, 2, "West", Some((area, 1)), false); // auto-pairs

    let backend = CloudMapper::new(server.base_url.clone(), owner.api_key.clone());
    let current = backend.get_area(&area).await.expect("fetch");
    assert_eq!(current.connections.len(), 1, "the seed pair shares one Connection");

    let envelope = |payload| MutationEnvelope {
        operation_id: Uuid::new_v4(),
        preconditions: vec![Precondition {
            resource: ResourceKind::Area,
            id: area.0,
            expected_rev: current.area.rev,
            access_fingerprint: current.area.access.map(|access| access.fingerprint()),
        }],
        payload,
    };

    // Retargeting a pair member is a structural edit: refused.
    let err = backend
        .execute_mutation(
            &area,
            &envelope(vec![AreaMutation::UpdateExit {
                exit_id: smudgy_cloud::ExitId(out_id),
                body: ExitUpdates {
                    to_room_number: Some(RoomNumber(3)),
                    ..ExitUpdates::default()
                },
            }]),
        )
        .await
        .expect_err("a pair-breaking retarget must be refused");
    match err {
        CloudError::StructuralConflict(reason) => assert_eq!(reason, "unlink_before_edit"),
        other => panic!("expected StructuralConflict, got {other:?}"),
    }

    // A traversal-only edit on the same member applies cleanly.
    backend
        .execute_mutation(
            &area,
            &envelope(vec![AreaMutation::UpdateExit {
                exit_id: smudgy_cloud::ExitId(out_id),
                body: ExitUpdates {
                    weight: Some(4.0),
                    ..ExitUpdates::default()
                },
            }]),
        )
        .await
        .expect("traversal-only fields stay editable on a pair");
}

// ---------------------------------------------------------------------------
// 1d. §6.4 closure through the real stack: one secret member in a pair
//     hides BOTH exits and the Connection from an uncleared editor's
//     projection (no to_unknown trace), and clearing the secret reveals the
//     group and moves the editor's projected rev.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secret_pair_member_hides_the_whole_group_from_an_uncleared_editor() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("closureowner@example.com", "closureowner", true);
    let editor = server.create_user("closureeditor@example.com", "closureeditor", true);
    server.befriend(&owner, &editor);

    let area = server.create_area(&owner, "Closure Realm");
    server.add_room(area, 1, "Gate", false);
    server.add_room(area, 2, "Yard", false);

    // Owner builds the bidirectional pair through a real Mapper.
    let owner_dir = TempCacheDir::new("closure-owner");
    let owner_mapper = new_synced_mapper(&server.base_url, &owner.api_key, owner_dir.path()).await;
    let out_id = owner_mapper
        .create_exit(
            RoomKey::new(area, RoomNumber(1)),
            ExitArgs {
                from_direction: ExitDirection::East,
                to_area_id: Some(area),
                to_room_number: Some(RoomNumber(2)),
                to_direction: Some(ExitDirection::West),
                ..ExitArgs::default()
            },
        )
        .await
        .expect("outbound create");
    let back_id = owner_mapper
        .create_exit(
            RoomKey::new(area, RoomNumber(2)),
            ExitArgs {
                from_direction: ExitDirection::West,
                to_area_id: Some(area),
                to_room_number: Some(RoomNumber(1)),
                to_direction: Some(ExitDirection::East),
                ..ExitArgs::default()
            },
        )
        .await
        .expect("reverse create");
    // Mark ONE member secret (a traversal-only edit — legal on a pair).
    owner_mapper.update_exit(
        RoomKey::new(area, RoomNumber(1)),
        out_id,
        ExitUpdates {
            is_secret: Some(true),
            ..ExitUpdates::default()
        },
    );
    assert!(
        owner_mapper
            .wait_for_sync_completion(10)
            .await
            .expect("owner writes acknowledged"),
        "owner queue drains"
    );

    // An editor grant WITHOUT include_secrets: can_edit, not cleared.
    server.grant(&owner, &editor, GrantScope::Area(area), GrantFlags::edit());

    // The editor's projection: the whole group is gone — both exits AND the
    // Connection — with no to_unknown/token trace of either member.
    let editor_dir = TempCacheDir::new("closure-editor");
    let editor_mapper =
        new_synced_mapper(&server.base_url, &editor.api_key, editor_dir.path()).await;
    let editor_backend = CloudMapper::new(server.base_url.clone(), editor.api_key.clone());
    let projected = editor_backend.get_area(&area).await.expect("editor fetch");
    assert!(
        projected.connections.is_empty(),
        "the effectively-secret Connection is omitted"
    );
    let projected_exits: Vec<_> = projected
        .rooms
        .iter()
        .flat_map(|room| room.exits.iter())
        .collect();
    assert!(
        projected_exits.is_empty(),
        "BOTH members vanish — the public one included"
    );
    let raw = serde_json::to_string(&projected).expect("serialize");
    assert!(
        !raw.contains(&out_id.to_string()) && !raw.contains(&back_id.to_string()),
        "no exit id survives anywhere in the projection"
    );
    assert!(
        !raw.contains("to_area_token") && !raw.contains("\"to_unknown\":true"),
        "an omitted group leaves no unknown-target trace"
    );
    let editor_cached = editor_mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("editor caches the area");
    assert!(
        editor_cached.get_room_connections().is_empty(),
        "nothing renders for the uncleared editor"
    );

    // Clearing the secret reveals the whole group and moves the editor's
    // projected rev (§6.3: a reveal bumps the public projection).
    let editor_client = api_client(&server.base_url, &editor.api_key);
    let rev_hidden = sync_row_rev(&editor_client, area).await;
    owner_mapper.update_exit(
        RoomKey::new(area, RoomNumber(1)),
        out_id,
        ExitUpdates {
            is_secret: Some(false),
            ..ExitUpdates::default()
        },
    );
    assert!(
        owner_mapper
            .wait_for_sync_completion(10)
            .await
            .expect("owner unset acknowledged"),
        "owner queue drains"
    );
    let rev_revealed = sync_row_rev(&editor_client, area).await;
    assert_ne!(
        rev_revealed, rev_hidden,
        "revealing the group moves the editor's projected rev"
    );

    tick(&editor_mapper).await;
    let projected = editor_backend.get_area(&area).await.expect("editor refetch");
    assert_eq!(projected.connections.len(), 1, "the group reappears whole");
    let visible_exits: Vec<_> = projected
        .rooms
        .iter()
        .flat_map(|room| room.exits.iter())
        .collect();
    assert_eq!(visible_exits.len(), 2, "both members are back");
    assert!(
        visible_exits
            .iter()
            .all(|exit| exit.connection_id == projected.connections[0].id),
        "linkage is intact after the reveal"
    );
    let editor_cached = editor_mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("editor cache refreshed");
    assert_eq!(editor_cached.get_room_connections().len(), 1);
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

    // The secret room is inserted through the compound endpoint so the full
    // rev diverges from the public rev (secret-only writes bump only the
    // full rev).
    let owner_mapper = CloudMapper::new(server.base_url.clone(), owner.api_key.clone());
    execute_ops(
        &owner_mapper,
        area,
        vec![AreaMutation::UpsertRoom {
            room_number: RoomNumber(7),
            body: RoomUpdates {
                title: Some(SECRET_TITLE.to_string()),
                is_secret: Some(true),
                ..RoomUpdates::default()
            },
        }],
    )
    .await;

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

// ---------------------------------------------------------------------------
// 6. Two clients on one account edit the same area: the second client's
//    stale-rev envelope conflicts, refetches, replays, and both converge.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_edits_conflict_refetch_replay_and_converge() {
    let server = MockServer::spawn().await;
    let user = server.create_user("pair@example.com", "pair", true);
    let area = server.create_area(&user, "Contested Lands");
    server.add_room(area, 1, "Origin", false);

    let cache_a = TempCacheDir::new("converge-a");
    let cache_b = TempCacheDir::new("converge-b");
    let mapper_a = new_synced_mapper(&server.base_url, &user.api_key, cache_a.path()).await;
    let mapper_b = new_synced_mapper(&server.base_url, &user.api_key, cache_b.path()).await;

    // A's write lands, moving the server past B's confirmed revision.
    mapper_a.upsert_room(
        RoomKey::new(area, RoomNumber(2)),
        RoomUpdates {
            title: Some("From A".to_string()),
            ..RoomUpdates::default()
        },
    );
    mapper_a.sync_now();
    assert!(
        mapper_a
            .wait_for_sync_completion(10)
            .await
            .expect("A's write acknowledged"),
        "A's queue drains"
    );

    // B still holds the pre-write revision, so its envelope 409s; the client
    // refetches, replays the pending edit over the fresh projection, and
    // resends under the same operation id.
    mapper_b.upsert_room(
        RoomKey::new(area, RoomNumber(3)),
        RoomUpdates {
            title: Some("From B".to_string()),
            ..RoomUpdates::default()
        },
    );
    mapper_b.sync_now();
    assert!(
        mapper_b
            .wait_for_sync_completion(10)
            .await
            .expect("B's write acknowledged after the rebase"),
        "B's queue drains"
    );

    // B converged: the refetch delivered A's room, and B's own edit rode the
    // replay.
    let cached_b = mapper_b
        .get_current_atlas()
        .get_area(&area)
        .expect("B holds the area");
    assert_eq!(
        cached_b
            .get_room(&RoomNumber(2))
            .expect("A's room visible to B")
            .get_title(),
        "From A"
    );
    assert_eq!(
        cached_b
            .get_room(&RoomNumber(3))
            .expect("B's own room survives the rebase")
            .get_title(),
        "From B"
    );

    // A converges on its next tick (the server rev moved under it).
    tick(&mapper_a).await;
    let cached_a = mapper_a
        .get_current_atlas()
        .get_area(&area)
        .expect("A holds the area");
    for number in [1, 2, 3] {
        assert!(
            cached_a.get_room(&RoomNumber(number)).is_some(),
            "room {number} present in A after the refetch"
        );
    }

    // The mock applied exactly two envelopes (A's, and B's rebased resend);
    // the conflicted first attempt applied nothing and left no receipt.
    let applied = server.mutation_requests();
    assert_eq!(applied.len(), 2, "two envelopes were accepted");
    assert!(
        applied.iter().all(|(_, replayed)| !replayed),
        "neither acceptance came from a receipt replay"
    );
    assert_ne!(applied[0].0, applied[1].0, "distinct operations");
}

// ---------------------------------------------------------------------------
// 7. Receipt dedupe over the wire: the mock commits a mutation but drops the
//    response; the client's transport retry carries the same operation id and
//    replays the stored receipt, so the mutation applies exactly once.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receipt_dedupes_a_retry_after_a_lost_response() {
    let server = MockServer::spawn().await;
    let user = server.create_user("retry@example.com", "retry", true);
    let area = server.create_area(&user, "Flaky Wire");
    server.add_room(area, 1, "Origin", false);

    let cache_dir = TempCacheDir::new("receipt-dedupe");
    let mapper = new_synced_mapper(&server.base_url, &user.api_key, cache_dir.path()).await;
    let (rev_before, _) = server.area_revs(area);

    // The first response is lost AFTER the mock commits and stores the
    // receipt; the client sees a transport failure and retries.
    server.drop_next_mutation_responses(1);
    mapper.upsert_room(
        RoomKey::new(area, RoomNumber(2)),
        RoomUpdates {
            title: Some("Once Only".to_string()),
            ..RoomUpdates::default()
        },
    );
    assert!(
        mapper
            .wait_for_sync_completion(15)
            .await
            .expect("the retry lands cleanly"),
        "pending queue drains after the retry"
    );

    // Same operation id both times; the second acceptance came from the
    // receipt, so nothing re-applied and the rev moved exactly once.
    let requests = server.mutation_requests();
    assert_eq!(requests.len(), 2, "original send plus one retry");
    assert_eq!(requests[0].0, requests[1].0, "the retry reuses the operation id");
    assert!(!requests[0].1, "the first request applied fresh");
    assert!(requests[1].1, "the retry replayed the stored receipt");

    let (rev_after, _) = server.area_revs(area);
    assert_eq!(rev_after, rev_before + 1, "the mutation applied exactly once");

    // The client's confirmed state matches the single application.
    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("area cached");
    assert_eq!(
        cached
            .get_room(&RoomNumber(2))
            .expect("room landed")
            .get_title(),
        "Once Only"
    );
}

// ---------------------------------------------------------------------------
// 8. Receipt replay across a fingerprint change: a stored result embeds the
//    accept-time projection, so once the caller's capabilities move, the
//    identical retry is refused with `projection_changed` (carrying the
//    CURRENT fingerprint) instead of leaking the stale projection.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_after_fingerprint_change_yields_projection_changed() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("editor@example.com", "editor", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Receipt Realm");
    server.add_room(area, 1, "Foyer", false);
    let grant_id = server.grant(&owner, &grantee, GrantScope::Area(area), GrantFlags::edit());

    // The grantee applies an envelope through the real client stack.
    let grantee_backend = CloudMapper::new(server.base_url.clone(), grantee.api_key.clone());
    let before = grantee_backend.get_area(&area).await.expect("fetch for the precondition");
    let envelope = MutationEnvelope {
        operation_id: Uuid::new_v4(),
        preconditions: vec![Precondition {
            resource: ResourceKind::Area,
            id: area.0,
            expected_rev: before.area.rev,
            access_fingerprint: before.area.access.map(|access| access.fingerprint()),
        }],
        payload: vec![AreaMutation::UpsertRoom {
            room_number: RoomNumber(2),
            body: RoomUpdates {
                title: Some("Annex".to_string()),
                ..RoomUpdates::default()
            },
        }],
    };
    grantee_backend
        .execute_mutation(&area, &envelope)
        .await
        .expect("envelope accepted");

    // While the projection stands still, the identical retry replays the
    // stored receipt verbatim (nothing re-applies).
    grantee_backend
        .execute_mutation(&area, &envelope)
        .await
        .expect("identical retry replays under an unchanged fingerprint");
    let requests = server.mutation_requests();
    assert_eq!(requests.len(), 2, "fresh application plus one replay");
    assert!(requests[1].1, "the retry was served from the receipt");

    // The owner raises include_secrets: the grantee's projection class flips.
    let owner_client = api_client(&server.base_url, &owner.api_key);
    owner_client
        .update_share(
            grant_id,
            SharePatch {
                include_secrets: Some(true),
                ..SharePatch::default()
            },
        )
        .await
        .expect("owner raises include_secrets");

    // The same retry now crosses the projection boundary: the receipt's
    // stored result was built for the old capabilities, so the server
    // refuses with projection_changed carrying the CURRENT fingerprint.
    let err = grantee_backend
        .execute_mutation(&area, &envelope)
        .await
        .expect_err("replay must be refused after the fingerprint change");
    let current_fingerprint = grantee_backend
        .get_area(&area)
        .await
        .expect("refetch under the new capabilities")
        .area
        .access
        .expect("access block present")
        .fingerprint();
    match err {
        CloudError::ProjectionChanged { access_fingerprint } => assert_eq!(
            access_fingerprint, current_fingerprint,
            "the refusal carries the caller's current fingerprint"
        ),
        other => panic!("expected ProjectionChanged, got {other:?}"),
    }
    assert_eq!(
        server.mutation_requests().len(),
        2,
        "the refused retry neither re-applied nor replayed"
    );
}

// ---------------------------------------------------------------------------
// 9. An idempotent tag re-add through the Mapper is accepted but moves no
//    revision counter: the envelope's every op no-ops, so the /sync row rev
//    stands still and the response reports the standing revision.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotent_tag_readd_moves_no_rev() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("tagger@example.com", "tagger", true);
    let area = server.create_area(&owner, "Tag Stability");
    server.add_room(area, 1, "Shrine", false);

    let cache_dir = TempCacheDir::new("tag-noop");
    let mapper = new_synced_mapper(&server.base_url, &owner.api_key, cache_dir.path()).await;
    let client = api_client(&server.base_url, &owner.api_key);
    let rev_initial = sync_row_rev(&client, area).await;

    // First add: a real insert, the served rev moves.
    mapper.add_room_tag(RoomKey::new(area, RoomNumber(1)), "inn".to_string());
    assert!(
        mapper
            .wait_for_sync_completion(10)
            .await
            .expect("first add acknowledged"),
        "pending queue drains"
    );
    let rev_after_add = sync_row_rev(&client, area).await;
    assert_ne!(
        rev_after_add, rev_initial,
        "a real tag insert moves the served rev (guards the no-op assertion)"
    );

    // Re-add, differently cased (normalizes to the same tag): the envelope
    // is accepted and acknowledged, but every op no-ops, so neither rev nor
    // public_rev moves.
    let revs_before_readd = server.area_revs(area);
    mapper.add_room_tag(RoomKey::new(area, RoomNumber(1)), "Inn".to_string());
    assert!(
        mapper
            .wait_for_sync_completion(10)
            .await
            .expect("re-add acknowledged"),
        "pending queue drains after the all-no-op envelope"
    );
    assert_eq!(
        sync_row_rev(&client, area).await,
        rev_after_add,
        "an idempotent tag re-add moves no served revision"
    );
    assert_eq!(
        server.area_revs(area),
        revs_before_readd,
        "neither rev nor public_rev moved on the all-no-op envelope"
    );

    // Both envelopes were accepted fresh — the no-op acceptance is a normal
    // acknowledgment, not a conflict, retry, or receipt replay.
    let requests = server.mutation_requests();
    assert_eq!(requests.len(), 2, "two envelopes accepted");
    assert!(
        requests.iter().all(|(_, replayed)| !replayed),
        "neither acceptance came from a receipt replay"
    );

    // The tag itself stands, exactly once, normalized.
    let cached = mapper
        .get_current_atlas()
        .get_area(&area)
        .expect("area cached");
    let room = cached.get_room(&RoomNumber(1)).expect("room present");
    assert_eq!(room.tags().collect::<Vec<_>>(), vec!["INN"]);
}
