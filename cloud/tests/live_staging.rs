//! Live round trip of map wire format 3 against a REAL cloud service.
//! Ignored by default and additionally env-gated, so neither `cargo test`
//! nor `--ignored` sweeps can hit the network by accident.
//!
//! ```text
//! SMUDGY_LIVE_STAGING=1 \
//! SMUDGY_LIVE_TOKEN=smudgy_sess_…                         # session token
//! SMUDGY_LIVE_BASE_URL=https://api.staging.smudgy.org     # optional; the default
//! SMUDGY_LIVE_FRIEND_ID=<uuid>                            # optional; see below
//!     cargo test -p smudgy_cloud --test live_staging -- --ignored --nocapture
//! ```
//!
//! The production host is refused. Each test creates one map named
//! `live-format-3-<suffix>` with one Secret, and deletes the map whatever the
//! outcome; the Clan Secrets test also founds a clan for it and dissolves it,
//! the copy test deletes its copy too, and the clan transfer test founds a
//! clan `live-format-3-clan-<suffix>` of its own and dissolves it after
//! deleting the map it received. The Secret-sharing test grants, changes and
//! revokes a grant to `SMUDGY_LIVE_FRIEND_ID`, a friend of the token's user,
//! when it is set; without it, it checks only the refusals.

#![allow(clippy::too_many_lines)]

use smudgy_cloud::clan_maps::MapOwnership;
use smudgy_cloud::cloud_api::{SecretGrantChange, secret_action};
use smudgy_cloud::mapper::AreaMutationBatch;
use smudgy_cloud::mutation::{
    AreaMutation, MoveRequest, MovedRoom, MutationEnvelope, OpResult, Precondition,
};
use smudgy_cloud::{
    AreaId, AreaWithDetails, CachedCloudMapper, CloudApiClient, CloudError, CloudMapper,
    CreateAreaRequest, Credential, CredentialSource, ExitArgs, ExitDirection, LabelArgs, LabelId,
    Mapper, MapperBackend, MovedContent, PackageApiClient, PublishModule, RoomNumber, RoomUpdates,
    SourceBundle, SourceId, Uuid,
};

const DEFAULT_BASE_URL: &str = "https://api.staging.smudgy.org";

fn live_env() -> Option<(String, String)> {
    std::env::var("SMUDGY_LIVE_STAGING").ok()?;
    let token = std::env::var("SMUDGY_LIVE_TOKEN").ok()?;
    let base_url =
        std::env::var("SMUDGY_LIVE_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
    Some((base_url, token))
}

fn envelope(
    area: AreaId,
    source: SourceId,
    rev: i64,
    payload: Vec<AreaMutation>,
) -> MutationEnvelope {
    MutationEnvelope {
        operation_id: Uuid::new_v4(),
        preconditions: vec![Precondition::source(area.0, source.clone(), rev)],
        source,
        payload,
    }
}

fn exit(direction: ExitDirection, area: AreaId, to: i32, back: ExitDirection) -> ExitArgs {
    ExitArgs {
        to_source: None,
        id: None,
        connection_id: None,
        new_connection_id: None,
        from_direction: direction,
        to_area_id: Some(area),
        to_room_number: Some(RoomNumber(to)),
        to_direction: Some(back),
        path: None,
        is_hidden: false,
        door: None,
        weight: 1.0,
        command: None,
    }
}

async fn round_trip(mapper: &CloudMapper, area: AreaId) -> Result<(), String> {
    let fresh = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("first read: {e}"))?;
    if fresh.area.rev != 1 || fresh.area.projection_token.is_none() {
        return Err(format!(
            "new map: rev {} token {:?}",
            fresh.area.rev, fresh.area.projection_token
        ));
    }

    // One envelope: two rooms, a reciprocal pair of exits, a property, a tag.
    let first = envelope(
        area,
        SourceId::map(),
        1,
        vec![
            AreaMutation::UpsertRoom {
                room_source: None,
                room_number: RoomNumber(1),
                body: RoomUpdates {
                    title: Some("Gate".into()),
                    x: Some(0.1),
                    ..Default::default()
                },
            },
            AreaMutation::UpsertRoom {
                room_source: None,
                room_number: RoomNumber(2),
                body: RoomUpdates {
                    title: Some("Plaza".into()),
                    x: Some(1.0),
                    ..Default::default()
                },
            },
            AreaMutation::CreateExit {
                room_source: None,
                room_number: RoomNumber(1),
                body: exit(ExitDirection::East, area, 2, ExitDirection::West),
            },
            AreaMutation::CreateExit {
                room_source: None,
                room_number: RoomNumber(2),
                body: exit(ExitDirection::West, area, 1, ExitDirection::East),
            },
            AreaMutation::UpsertRoomProperty {
                room_source: None,
                room_number: RoomNumber(1),
                name: "notes".into(),
                value: "the map's".into(),
            },
            AreaMutation::AddRoomTag {
                room_source: None,
                room_number: RoomNumber(1),
                tag: " inn ".into(),
            },
        ],
    );
    let result = mapper
        .execute_mutation(&area, &first)
        .await
        .map_err(|e| format!("first write: {e}"))?;
    let own = result
        .versions
        .iter()
        .find(|v| v.is_map_of(area.0))
        .ok_or("no map version")?;
    if own.rev != 2 {
        return Err(format!("map rev after the write: {}", own.rev));
    }
    if !matches!(result.data.get(5), Some(OpResult::RoomTag { tag, .. }) if tag == "INN") {
        return Err(format!("tag echo: {:?}", result.data.get(5)));
    }

    // A replay of the same operation is the stored result.
    let replay = mapper
        .execute_mutation(&area, &first)
        .await
        .map_err(|e| format!("replay: {e}"))?;
    if serde_json::to_value(&replay).ok() != serde_json::to_value(&result).ok() {
        return Err("replay differs from the first result".into());
    }

    let after = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("second read: {e}"))?;
    if after.area.rev != 2 || after.rooms.len() != 2 {
        return Err(format!(
            "after write: rev {} rooms {}",
            after.area.rev,
            after.rooms.len()
        ));
    }
    if after.area.projection_token == fresh.area.projection_token {
        return Err("the token did not move with the content".into());
    }
    let gate = &after.rooms[0];
    if (gate.x - 0.1).abs() > f32::EPSILON || !gate.tags.contains("INN") {
        return Err(format!("room 1: x {} tags {:?}", gate.x, gate.tags));
    }
    let east = &gate.exits[0];
    let west = &after.rooms[1].exits[0];
    if east.connection_id != west.connection_id || after.connections.len() != 1 {
        return Err("reciprocal exits did not pair".into());
    }

    // /sync carries the same token and the map's revision.
    let rows = mapper
        .sync_state()
        .await
        .map_err(|e| format!("sync: {e}"))?
        .ok_or("no /sync")?;
    let row = rows
        .iter()
        .find(|row| row.area_id == area)
        .ok_or("no sync row")?;
    if row.map_rev() != Some(2)
        || Some(&row.projection_token) != after.area.projection_token.as_ref()
    {
        return Err(format!("sync row: {row:?}"));
    }

    // A stale revision is refused as a conflict on that source.
    let stale = envelope(
        area,
        SourceId::map(),
        1,
        vec![AreaMutation::DeleteRoom {
            room_source: None,
            room_number: RoomNumber(2),
        }],
    );
    match mapper.execute_mutation(&area, &stale).await {
        Err(CloudError::RevisionConflict {
            expected_rev: 1,
            current_rev: 2,
            ..
        }) => {}
        other => return Err(format!("stale write: {other:?}")),
    }

    // The caller's Private source keeps its own notes for the map room.
    let private = SourceId::Private;
    let mine = envelope(
        area,
        private.clone(),
        0,
        vec![AreaMutation::UpsertRoomProperty {
            room_source: None,
            room_number: RoomNumber(1),
            name: "notes".into(),
            value: "mine".into(),
        }],
    );
    mapper
        .execute_mutation(&area, &mine)
        .await
        .map_err(|e| format!("private write: {e}"))?;
    let both = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("third read: {e}"))?;
    if both.area.rev != 2 || both.rooms[0].properties[0].value != "the map's" {
        return Err("a private write changed the map".into());
    }
    let bundle = both
        .sources
        .iter()
        .find(|bundle| bundle.source == private)
        .ok_or("no private bundle")?;
    let data = bundle.room_data.first().ok_or("no private room data")?;
    if bundle.rev != 1 || data.room_number != RoomNumber(1) || data.properties[0].value != "mine" {
        return Err(format!("private bundle: {bundle:?}"));
    }
    secrets(mapper, area).await
}

fn secret_bundle<'a>(
    area: &'a AreaWithDetails,
    secret: &SourceId,
) -> Result<&'a SourceBundle, String> {
    area.sources
        .iter()
        .find(|bundle| &bundle.source == secret)
        .ok_or_else(|| "no Secret bundle".to_string())
}

fn move_request(
    area: AreaId,
    (from, from_rev): (&SourceId, i64),
    (to, to_rev): (&SourceId, i64),
    rooms: Vec<RoomNumber>,
) -> MoveRequest {
    MoveRequest {
        operation_id: Uuid::new_v4(),
        from: from.clone(),
        to: to.clone(),
        preconditions: vec![
            Precondition::source(area.0, from.clone(), from_rev),
            Precondition::source(area.0, to.clone(), to_rev),
        ],
        rooms: rooms.into_iter().map(MovedRoom::plain).collect(),
        connections: Vec::new(),
        labels: Vec::new(),
        shapes: Vec::new(),
        access_review: None,
        properties: Vec::new(),
        property_resolutions: Vec::new(),
    }
}

/// An owner Secret: its own notes on a map room, a room moved into it with
/// its links and back out, a rename, and its deletion.
async fn secrets(mapper: &CloudMapper, area: AreaId) -> Result<(), String> {
    let generation = mapper.auth_generation();
    let map = SourceId::map();
    let secret = mapper
        .create_secret(&area, "Behind The Bookcase", None, generation)
        .await
        .map_err(|e| format!("create a Secret: {e}"))?
        .source;

    let notes = envelope(
        area,
        secret.clone(),
        1,
        vec![AreaMutation::UpsertRoomProperty {
            room_source: None,
            room_number: RoomNumber(1),
            name: "notes".into(),
            value: "the Secret's".into(),
        }],
    );
    mapper
        .execute_mutation(&area, &notes)
        .await
        .map_err(|e| format!("Secret notes: {e}"))?;

    // Room 2 leaves the map; both exits of the pair go with it.
    let moved = mapper
        .move_content(
            &area,
            &move_request(area, (&map, 2), (&secret, 2), vec![RoomNumber(2)]),
            generation,
        )
        .await
        .map_err(|e| format!("move into the Secret: {e}"))?;
    let revisions: Vec<i64> = moved.versions.iter().map(|version| version.rev).collect();
    if revisions[..2] != [3, 3] {
        return Err(format!("move versions: {:?}", moved.versions));
    }
    let hidden = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("read after the move: {e}"))?;
    let bundle = secret_bundle(&hidden, &secret)?;
    let hall = bundle
        .room_data
        .first()
        .ok_or("no Secret data for room 1")?;
    if hidden.rooms.len() != 1
        || !hidden.rooms[0].exits.is_empty()
        || hidden.rooms[0].properties[0].value != "the map's"
        || bundle.rooms.len() != 1
        || bundle.rooms[0].room_number != RoomNumber(2)
        || hall.properties[0].value != "the Secret's"
        || hall.exits.first().and_then(|exit| exit.to_source.as_ref()) != Some(&secret)
        || bundle.connections.len() != 1
    {
        return Err(format!(
            "after the move: map {:?} Secret {bundle:?}",
            hidden.rooms
        ));
    }

    let renamed = mapper
        .rename_secret(&area, &secret, "Under The Stairs", generation)
        .await
        .map_err(|e| format!("rename: {e}"))?;
    let listed = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("read after the rename: {e}"))?;
    let bundle = secret_bundle(&listed, &secret)?;
    if renamed.name != "Under The Stairs"
        || bundle.name.as_deref() != Some("Under The Stairs")
        || bundle.rev != 4
        || listed.area.projection_token == hidden.area.projection_token
    {
        return Err(format!("after the rename: {bundle:?}"));
    }

    // Splitting nothing, the room comes back whole.
    mapper
        .move_content(
            &area,
            &move_request(area, (&secret, 4), (&map, 3), vec![RoomNumber(2)]),
            generation,
        )
        .await
        .map_err(|e| format!("move back: {e}"))?;
    let back = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("read after moving back: {e}"))?;
    if back.rooms.len() != 2 || back.connections.len() != 1 || back.rooms[0].exits.len() != 1 {
        return Err(format!("after moving back: {:?}", back.rooms));
    }

    // A chosen color comes back lowercase and moves the revision; clearing
    // it gives the Secret back to the palette.
    let before = secret_bundle(&back, &secret)?.rev;
    let colored = mapper
        .recolor_secret(&area, &secret, Some("#3A7BD5"), generation)
        .await
        .map_err(|e| format!("recolor: {e}"))?;
    let read = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("read after the recolor: {e}"))?;
    let bundle = secret_bundle(&read, &secret)?;
    if colored.color.as_deref() != Some("#3a7bd5")
        || bundle.color.as_deref() != Some("#3a7bd5")
        || bundle.rev != before + 1
    {
        return Err(format!("after the recolor: {bundle:?}"));
    }
    mapper
        .recolor_secret(&area, &secret, None, generation)
        .await
        .map_err(|e| format!("clear the color: {e}"))?;
    let read = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("read after clearing the color: {e}"))?;
    if secret_bundle(&read, &secret)?.color.is_some() {
        return Err("the color stayed after clearing it".to_string());
    }

    mapper
        .delete_secret(&area, &secret, generation)
        .await
        .map_err(|e| format!("delete the Secret: {e}"))?;
    let gone = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("read after the delete: {e}"))?;
    if gone.sources.iter().any(|bundle| bundle.source == secret) {
        return Err("the deleted Secret is still served".into());
    }
    Ok(())
}

#[tokio::test]
#[ignore = "talks to a live cloud service; see the module docs"]
async fn format_3_round_trip() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("SMUDGY_LIVE_STAGING / SMUDGY_LIVE_TOKEN unset; skipping");
        return;
    };
    assert!(
        !base_url.contains("api.smudgy.org"),
        "refusing to run against production"
    );
    let mapper = CloudMapper::with_credentials(
        base_url,
        CredentialSource::new(Some(Credential::Session(token))),
    );
    let area = mapper
        .create_area(CreateAreaRequest {
            name: format!("live-format-3-{}", Uuid::new_v4().simple()),
            atlas_id: None,
            clan_id: None,
            ownership: None,
            ephemeral: false,
            properties: std::collections::BTreeMap::default(),
        })
        .await
        .expect("create a map")
        .id;
    let outcome = round_trip(&mapper, area).await;
    let cleanup = mapper.delete_area(&area).await;
    outcome.expect("format-3 round trip");
    cleanup.expect("delete the map");
}

/// What the client predicted for a source and what the server serves must
/// agree on everything but identities the server keeps to itself.
fn shape_of(bundle: &SourceBundle) -> serde_json::Value {
    let rooms: Vec<_> = bundle
        .rooms
        .iter()
        .map(|room| {
            let exits: Vec<_> = room
                .exits
                .iter()
                .map(|exit| (exit.from_direction, exit.to_room_number, exit.to_source))
                .collect();
            serde_json::json!([room.room_number, room.title, room.properties, exits])
        })
        .collect();
    let data: Vec<_> = bundle
        .room_data
        .iter()
        .map(|data| {
            let exits: Vec<_> = data
                .exits
                .iter()
                .map(|exit| (exit.from_direction, exit.to_room_number, exit.to_source))
                .collect();
            serde_json::json!([data.room_number, data.properties, data.tags, exits])
        })
        .collect();
    let connections: Vec<_> = bundle
        .connections
        .iter()
        .map(|connection| {
            let mut ends: Vec<_> = std::iter::once(connection.endpoint_a)
                .chain(connection.endpoint_b)
                .map(|end| (end.room_number, end.source))
                .collect();
            ends.sort();
            ends
        })
        .collect();
    serde_json::json!({ "rooms": rooms, "room_data": data, "connections": connections })
}

/// Edits a Secret through the full client stack: the batch is compiled over
/// the Secret's document, shown at once, accepted by the server, and what the
/// server then serves matches what the client showed.
#[tokio::test]
#[ignore = "talks to a live cloud service; see the module docs"]
async fn secret_edits_through_the_mapper() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("SMUDGY_LIVE_STAGING / SMUDGY_LIVE_TOKEN unset; skipping");
        return;
    };
    assert!(
        !base_url.contains("api.smudgy.org"),
        "refusing to run against production"
    );
    let credentials = CredentialSource::new(Some(Credential::Session(token)));
    let direct = CloudMapper::with_credentials(base_url.clone(), credentials.clone());
    let area = direct
        .create_area(CreateAreaRequest {
            name: format!("live-format-3-mapper-{}", Uuid::new_v4().simple()),
            atlas_id: None,
            clan_id: None,
            ownership: None,
            ephemeral: false,
            properties: std::collections::BTreeMap::default(),
        })
        .await
        .expect("create a map")
        .id;
    let cache = std::env::temp_dir().join(format!("smudgy-live-{}", Uuid::new_v4()));
    let outcome = mapper_round_trip(&direct, &base_url, credentials, &cache, area).await;
    let cleanup = direct.delete_area(&area).await;
    let _ = std::fs::remove_dir_all(&cache);
    outcome.expect("Secret edits through the mapper");
    cleanup.expect("delete the map");
}

async fn mapper_round_trip(
    direct: &CloudMapper,
    base_url: &str,
    credentials: CredentialSource,
    cache: &std::path::Path,
    area: AreaId,
) -> Result<(), String> {
    let backend = CachedCloudMapper::new(
        CloudMapper::with_credentials(base_url.to_string(), credentials),
        cache,
    );
    let mapper = Mapper::new(std::sync::Arc::new(backend), cache);
    let loaded = |mapper: &Mapper| mapper.get_current_atlas().get_area(&area).is_some();
    for _ in 0..200 {
        if loaded(&mapper) {
            break;
        }
        mapper.sync_now();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    if !loaded(&mapper) {
        return Err("the new map never loaded".into());
    }
    mapper
        .mutate_area(
            area,
            vec![AreaMutation::UpsertRoom {
                room_number: RoomNumber(1),
                room_source: None,
                body: RoomUpdates {
                    title: Some("Reading Room".into()),
                    ..Default::default()
                },
            }],
            "Map room",
        )
        .map_err(|e| format!("map room: {e}"))?;
    let secret = mapper
        .create_secret(area, "Behind The Shelf", None)
        .await
        .map_err(|e| format!("create the Secret: {e}"))?
        .source;

    let mut door = exit(ExitDirection::East, area, 2, ExitDirection::West);
    door.to_source = Some(secret);
    let back = exit(ExitDirection::West, area, 1, ExitDirection::East);
    let submissions = mapper
        .mutate_batches(vec![
            AreaMutationBatch::strict(
                area,
                vec![
                    AreaMutation::CreateRoom {
                        room_number: RoomNumber(2),
                        room_source: Some(secret),
                        body: RoomUpdates {
                            title: Some("Hidden Study".into()),
                            x: Some(1.0),
                            ..Default::default()
                        },
                    },
                    AreaMutation::UpsertRoomProperty {
                        room_number: RoomNumber(1),
                        room_source: None,
                        name: "notes".into(),
                        value: "pull the red book".into(),
                    },
                    AreaMutation::CreateExit {
                        room_number: RoomNumber(1),
                        room_source: None,
                        body: door,
                    },
                    AreaMutation::CreateExit {
                        room_number: RoomNumber(2),
                        room_source: Some(secret),
                        body: back,
                    },
                ],
                "Hidden study behind the shelf",
            )
            .in_source(secret),
        ])
        .map_err(|e| format!("stage the Secret edit: {e}"))?;
    let shown = mapper
        .get_current_atlas()
        .get_area(&area)
        .and_then(|cache| {
            cache
                .meta()
                .sources
                .iter()
                .find(|bundle| bundle.source == secret)
                .cloned()
        })
        .ok_or("the Secret is not shown")?;
    if shown.rooms.len() != 1 || shown.room_data.len() != 1 || shown.connections.len() != 1 {
        return Err(format!("optimistic Secret: {}", shape_of(&shown)));
    }
    for submission in submissions {
        if let Some(operation) = submission.operation_id() {
            mapper
                .wait_for_mutation(operation)
                .await
                .map_err(|e| format!("the server refused the Secret edit: {e:?}"))?;
        }
    }

    let fetched = direct
        .get_area(&area)
        .await
        .map_err(|e| format!("read the map: {e}"))?;
    let truth = fetched
        .sources
        .iter()
        .find(|bundle| bundle.source == secret)
        .ok_or("the server serves no Secret")?;
    if shape_of(truth) != shape_of(&shown) {
        return Err(format!(
            "the server's Secret {} differs from the client's {}",
            shape_of(truth),
            shape_of(&shown)
        ));
    }
    if fetched.rooms[0]
        .properties
        .iter()
        .any(|p| p.name == "notes")
    {
        return Err("the Secret's note landed on the map".into());
    }
    label_moves(direct, &mapper, area, secret).await
}

/// A label drawn in the Secret moves to the map and back at once: each move
/// stands on the revisions the edits and moves before it produced, which
/// the published map does not carry until it is read again.
async fn label_moves(
    direct: &CloudMapper,
    mapper: &Mapper,
    area: AreaId,
    secret: SourceId,
) -> Result<(), String> {
    let label = LabelId(Uuid::new_v4());
    let drawn = mapper
        .mutate_batches(vec![
            AreaMutationBatch::strict(
                area,
                vec![AreaMutation::CreateLabel {
                    body: LabelArgs {
                        id: Some(label),
                        text: "Shelf mark".into(),
                        width: 40.0,
                        height: 10.0,
                        color: "#ffffff".into(),
                        font_size: 12,
                        font_weight: 400,
                        ..LabelArgs::default()
                    },
                }],
                "Label",
            )
            .in_source(secret),
        ])
        .map_err(|e| format!("stage the label: {e}"))?;
    for submission in drawn {
        if let Some(operation) = submission.operation_id() {
            mapper
                .wait_for_mutation(operation)
                .await
                .map_err(|e| format!("the server refused the label: {e:?}"))?;
        }
    }
    let only = || MovedContent {
        labels: vec![label],
        ..MovedContent::default()
    };
    for (from, to) in [
        (secret, SourceId::map()),
        (SourceId::map(), secret),
        (secret, SourceId::map()),
        (SourceId::map(), secret),
    ] {
        mapper
            .move_content(area, from, to, only())
            .await
            .map_err(|e| format!("move the label from {from:?} to {to:?}: {e:?}"))?;
    }
    let fetched = direct
        .get_area(&area)
        .await
        .map_err(|e| format!("read the map after the moves: {e}"))?;
    if fetched.labels.iter().any(|shown| shown.id == label)
        || !secret_bundle(&fetched, &secret)?
            .labels
            .iter()
            .any(|shown| shown.id == label)
    {
        return Err("the label is not back in the Secret".into());
    }
    Ok(())
}

/// Room numbers through the full client stack: undo of a renumbering move
/// asks for the room's old number and gets it, and a deleted map room takes
/// a Secret's door into it along, its number free for the next room.
#[tokio::test]
#[ignore = "talks to a live cloud service; see the module docs"]
async fn room_numbers_through_the_mapper() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("SMUDGY_LIVE_STAGING / SMUDGY_LIVE_TOKEN unset; skipping");
        return;
    };
    assert!(
        !base_url.contains("api.smudgy.org"),
        "refusing to run against production"
    );
    let credentials = CredentialSource::new(Some(Credential::Session(token)));
    let direct = CloudMapper::with_credentials(base_url.clone(), credentials.clone());
    let area = direct
        .create_area(CreateAreaRequest {
            name: format!("live-format-3-numbers-{}", Uuid::new_v4().simple()),
            atlas_id: None,
            clan_id: None,
            ownership: None,
            ephemeral: false,
            properties: std::collections::BTreeMap::default(),
        })
        .await
        .expect("create a map")
        .id;
    let cache = std::env::temp_dir().join(format!("smudgy-live-{}", Uuid::new_v4()));
    let outcome = room_numbers(&direct, &base_url, credentials, &cache, area).await;
    let cleanup = direct.delete_area(&area).await;
    let _ = std::fs::remove_dir_all(&cache);
    outcome.expect("room numbers through the mapper");
    cleanup.expect("delete the map");
}

async fn room_numbers(
    direct: &CloudMapper,
    base_url: &str,
    credentials: CredentialSource,
    cache: &std::path::Path,
    area: AreaId,
) -> Result<(), String> {
    let backend = CachedCloudMapper::new(
        CloudMapper::with_credentials(base_url.to_string(), credentials),
        cache,
    );
    let mapper = Mapper::new(std::sync::Arc::new(backend), cache);
    let loaded = |mapper: &Mapper| mapper.get_current_atlas().get_area(&area).is_some();
    for _ in 0..200 {
        if loaded(&mapper) {
            break;
        }
        mapper.sync_now();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    if !loaded(&mapper) {
        return Err("the new map never loaded".into());
    }
    let create = |number: i32, title: &str| {
        let submission = mapper
            .create_room(
                smudgy_cloud::mapper::RoomKey::new(area, RoomNumber(number)),
                RoomUpdates {
                    title: Some(title.to_string()),
                    ..RoomUpdates::default()
                },
            )
            .map_err(|e| format!("create room {number}: {e}"));
        let mapper = &mapper;
        async move {
            let operation = submission?.operation_id().ok_or("not a cloud write")?;
            mapper
                .wait_for_mutation(operation)
                .await
                .map_err(|e| format!("room {number} refused: {e}"))
        }
    };
    let one = |room: i32, asks: Option<i32>| MovedContent {
        rooms: vec![RoomNumber(room)],
        asked: asks
            .map(|asks| (RoomNumber(room), RoomNumber(asks)))
            .into_iter()
            .collect(),
        ..MovedContent::default()
    };
    let changed = |result: &smudgy_cloud::mutation::MoveResult| -> Vec<(i32, i32)> {
        result
            .renumbered
            .iter()
            .map(|renumbering| (renumbering.from.0, renumbering.to.0))
            .collect()
    };

    for (number, title) in [(1, "Gate"), (3, "Pantry"), (4, "Vault")] {
        create(number, title).await?;
    }
    let secret = mapper
        .create_secret(area, "Bookcase", None)
        .await
        .map_err(|e| format!("create the Secret: {e}"))?
        .source;

    // The Secret gets a room 3 of its own; the map's new 3 then lands as 4.
    mapper
        .move_content(area, SourceId::map(), secret, one(3, None))
        .await
        .map_err(|e| format!("move the Pantry: {e:?}"))?;
    create(3, "Larder").await?;
    let moved = mapper
        .move_content(area, SourceId::map(), secret, one(3, None))
        .await
        .map_err(|e| format!("move the Larder: {e:?}"))?;
    if changed(&moved) != [(3, 4)] {
        return Err(format!("the Larder's move: {moved:?}"));
    }
    // Undo asks for 3 back, and gets it.
    let undone = mapper
        .move_content(area, secret, SourceId::map(), one(4, Some(3)))
        .await
        .map_err(|e| format!("undo: {e:?}"))?;
    if changed(&undone) != [(4, 3)] {
        return Err(format!("the undo: {undone:?}"));
    }
    let read = direct
        .get_area(&area)
        .await
        .map_err(|e| format!("read after the undo: {e}"))?;
    let larder = read
        .rooms
        .iter()
        .find(|room| room.room_number == RoomNumber(3));
    if larder.is_none_or(|room| room.title != "Larder") {
        return Err(format!("map room 3 after the undo: {:?}", read.rooms));
    }

    // A hidden door from the Gate into the Vault, kept by the Secret.
    let mut door = exit(ExitDirection::Down, area, 4, ExitDirection::Up);
    door.is_hidden = true;
    let submissions = mapper
        .mutate_batches(vec![
            AreaMutationBatch::strict(
                area,
                vec![AreaMutation::CreateExit {
                    room_number: RoomNumber(1),
                    room_source: None,
                    body: door,
                }],
                "Door",
            )
            .in_source(secret),
        ])
        .map_err(|e| format!("stage the door: {e}"))?;
    for submission in submissions {
        if let Some(operation) = submission.operation_id() {
            mapper
                .wait_for_mutation(operation)
                .await
                .map_err(|e| format!("the door refused: {e:?}"))?;
        }
    }
    let deleted = mapper
        .delete_room(smudgy_cloud::mapper::RoomKey::new(area, RoomNumber(4)))
        .map_err(|e| format!("delete the Vault: {e}"))?;
    mapper
        .wait_for_mutation(deleted.operation_id().ok_or("not a cloud write")?)
        .await
        .map_err(|e| format!("the Vault's deletion refused: {e:?}"))?;
    let read = direct
        .get_area(&area)
        .await
        .map_err(|e| format!("read after the deletion: {e}"))?;
    let doors: Vec<_> = secret_bundle(&read, &secret)?
        .room_data
        .iter()
        .flat_map(|data| data.exits.iter())
        .map(|exit| (exit.to_area_id, exit.to_room_number))
        .collect();
    if doors != [(None, None)] {
        return Err(format!("the door after the deletion: {doors:?}"));
    }
    let next = mapper
        .try_next_room_number(&area)
        .map_err(|e| format!("the next number: {e}"))?;
    if next != RoomNumber(4) {
        return Err(format!("the next map room is {next}, not 4"));
    }
    Ok(())
}
/// Shares a Secret one grant at a time: the refusals always, and with
/// `SMUDGY_LIVE_FRIEND_ID` a grant that is listed, changed and revoked.
#[tokio::test]
#[ignore = "talks to a live cloud service; see the module docs"]
async fn secret_grants_round_trip() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("SMUDGY_LIVE_STAGING / SMUDGY_LIVE_TOKEN unset; skipping");
        return;
    };
    assert!(
        !base_url.contains("api.smudgy.org"),
        "refusing to run against production"
    );
    let friend = std::env::var("SMUDGY_LIVE_FRIEND_ID")
        .ok()
        .map(|raw| Uuid::parse_str(&raw).expect("SMUDGY_LIVE_FRIEND_ID is a UUID"));
    let credentials = CredentialSource::new(Some(Credential::Session(token)));
    let mapper = CloudMapper::with_credentials(base_url.clone(), credentials.clone());
    let client = CloudApiClient::new(base_url, credentials);
    let area = mapper
        .create_area(CreateAreaRequest {
            name: format!("live-format-3-grants-{}", Uuid::new_v4().simple()),
            atlas_id: None,
            clan_id: None,
            ownership: None,
            ephemeral: false,
            properties: std::collections::BTreeMap::default(),
        })
        .await
        .expect("create a map")
        .id;
    let outcome = grants_round_trip(&mapper, &client, area, friend).await;
    let cleanup = mapper.delete_area(&area).await;
    outcome.expect("Secret grants round trip");
    cleanup.expect("delete the map");
}

async fn grants_round_trip(
    mapper: &CloudMapper,
    client: &CloudApiClient,
    area: AreaId,
    friend: Option<Uuid>,
) -> Result<(), String> {
    let generation = mapper.auth_generation();
    let secret = mapper
        .create_secret(&area, "Behind The Bookcase", None, generation)
        .await
        .map_err(|e| format!("create a Secret: {e}"))?
        .source;
    let listed = client
        .secret_grants(&secret)
        .await
        .map_err(|e| format!("list the grants: {e}"))?;
    if !listed.is_empty() {
        return Err(format!("a new Secret has grants: {listed:?}"));
    }
    match client
        .grant_secret(&secret, Uuid::new_v4(), &[secret_action::EDIT])
        .await
    {
        Err(CloudError::NotFoundOrNoAccess) => {}
        other => return Err(format!("a stranger's grant: {other:?}")),
    }
    let anyone = friend.unwrap_or_else(Uuid::new_v4);
    match client.grant_secret(&secret, anyone, &["fly"]).await {
        Err(CloudError::InvalidInput(_)) => {}
        other => return Err(format!("an unknown action: {other:?}")),
    }

    let Some(friend) = friend else {
        return Ok(());
    };
    let grant = client
        .grant_secret(&secret, friend, &[secret_action::EDIT])
        .await
        .map_err(|e| format!("grant to the friend: {e}"))?;
    if grant.grantee_id != friend
        || grant.secret_id != secret
        || grant.area_id != area
        || !grant.can(secret_action::READ)
        || !grant.can(secret_action::EDIT)
    {
        return Err(format!("the grant: {grant:?}"));
    }
    let again = client
        .grant_secret(&secret, friend, &[secret_action::ADD])
        .await
        .map_err(|e| format!("grant again: {e}"))?;
    if again.id != grant.id || !again.can(secret_action::EDIT) || !again.can(secret_action::ADD) {
        return Err(format!(
            "a repeat grant did not add to the first: {again:?}"
        ));
    }
    let changed = mapper
        .update_secret_grant(
            &area,
            &secret,
            grant.id,
            &SecretGrantChange {
                add: [secret_action::REMOVE.to_string()].into(),
                remove: [secret_action::EDIT.to_string()].into(),
            },
            generation,
        )
        .await
        .map_err(|e| format!("change the grant: {e}"))?;
    if !changed.can(secret_action::REMOVE) {
        return Err(format!("the changed grant: {changed:?}"));
    }
    let listed = mapper
        .secret_grants(&area, &secret, generation)
        .await
        .map_err(|e| format!("list again: {e}"))?;
    if listed.iter().map(|g| g.id).collect::<Vec<_>>() != [grant.id] {
        return Err(format!("the listing: {listed:?}"));
    }
    client
        .revoke_secret_grant(&secret, grant.id)
        .await
        .map_err(|e| format!("revoke: {e}"))?;
    let left = client
        .secret_grants(&secret)
        .await
        .map_err(|e| format!("list after revoking: {e}"))?;
    if !left.is_empty() {
        return Err(format!("revoked grants remain: {left:?}"));
    }
    Ok(())
}

/// Packages through the real client against the live service: a version
/// published as one zstd bundle, installed whole, and a second version whose
/// update fetches only the body that changed. The package is deleted after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "talks to a live cloud service; see the module docs"]
async fn package_bundles_round_trip() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("SMUDGY_LIVE_STAGING / SMUDGY_LIVE_TOKEN unset; skipping");
        return;
    };
    assert!(
        !base_url.contains("api.smudgy.org"),
        "refusing to run against production"
    );
    let credentials = CredentialSource::new(Some(Credential::Session(token)));
    let owner = CloudApiClient::new(base_url.clone(), credentials.clone())
        .me()
        .await
        .expect("profile")
        .nickname
        .expect("the live account has a nickname");
    let api = PackageApiClient::new(base_url, credentials);
    let name = format!("live-bundles-{}", Uuid::new_v4().simple());
    let package = api
        .create_package(&name, "live staging bundle test")
        .await
        .expect("create package");
    let outcome = package_bundles(&api, package.id, &owner, &name).await;
    api.delete_package(package.id)
        .await
        .expect("delete the test package");
    if let Err(failure) = outcome {
        panic!("{failure}");
    }
}

async fn package_bundles(
    api: &PackageApiClient,
    package_id: Uuid,
    owner: &str,
    name: &str,
) -> Result<(), String> {
    let module = |subpath: &str, content: Vec<u8>, is_entry: bool| PublishModule {
        subpath: subpath.to_string(),
        content,
        media_type: "application/typescript".to_string(),
        is_entry,
    };
    let shared = b"export const shared = 1;\n".repeat(100);
    let first = vec![
        module(
            "index.ts",
            b"export const hello = 'world';\n".repeat(200),
            true,
        ),
        module("shared.ts", shared.clone(), false),
        module("copy.ts", shared.clone(), false),
    ];
    let manifest = |version: &str| serde_json::json!({ "name": name, "version": version });
    api.publish_version(package_id, "1.0.0", &manifest("1.0.0"), &first, &[], None)
        .await
        .map_err(|e| format!("publish 1.0.0: {e}"))?;

    let resolved = api
        .resolve_package(Some(owner), name, Some("1.0.0"))
        .await
        .map_err(|e| format!("resolve 1.0.0: {e}"))?;
    if resolved.bodies.len() != 2 {
        return Err(format!("1.0.0 bodies: {:?}", resolved.bodies));
    }
    let hashes: Vec<&str> = resolved
        .modules
        .iter()
        .map(|m| m.content_hash.as_str())
        .collect();
    let installed = api
        .fetch_bodies(&resolved.bundle_url, &resolved.bodies, &hashes)
        .await
        .map_err(|e| format!("fetch 1.0.0: {e}"))?;
    for published in &first {
        let wire = resolved
            .modules
            .iter()
            .find(|m| m.subpath == published.subpath)
            .ok_or_else(|| format!("{} is missing from 1.0.0", published.subpath))?;
        if installed.get(&wire.content_hash) != Some(&published.content) {
            return Err(format!("{} came back different", published.subpath));
        }
    }

    // An update asks only for the body it doesn't hold.
    let changed = b"export const hello = 'there';\n".repeat(200);
    let mut second = first.clone();
    second[0].content.clone_from(&changed);
    api.publish_version(package_id, "1.0.1", &manifest("1.0.1"), &second, &[], None)
        .await
        .map_err(|e| format!("publish 1.0.1: {e}"))?;
    let update = api
        .resolve_package(Some(owner), name, Some("1.0.1"))
        .await
        .map_err(|e| format!("resolve 1.0.1: {e}"))?;
    let missing: Vec<&str> = update
        .bodies
        .iter()
        .map(|body| body.content_hash.as_str())
        .filter(|hash| !installed.contains_key(*hash))
        .collect();
    if missing.len() != 1 {
        return Err(format!("expected one new body, found {missing:?}"));
    }
    let fetched = api
        .fetch_bodies(&update.bundle_url, &update.bodies, &missing)
        .await
        .map_err(|e| format!("fetch the update: {e}"))?;
    if fetched.get(missing[0]) != Some(&changed) || fetched.len() != 1 {
        return Err("the update fetched the wrong bodies".to_string());
    }
    Ok(())
}

/// Clan Secrets with one account, the clan's founder and only member: a
/// Member-owned and a Clan-owned Secret on a clan map, who has access, and an
/// offer that makes the Clan-owned one the founder's own. The clan, its
/// folder and map are made for the test and dissolved afterwards whatever
/// the outcome.
#[tokio::test]
#[ignore = "talks to a live cloud service; see the module docs"]
async fn clan_secrets_round_trip() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("SMUDGY_LIVE_STAGING / SMUDGY_LIVE_TOKEN unset; skipping");
        return;
    };
    assert!(
        !base_url.contains("api.smudgy.org"),
        "refusing to run against production"
    );
    let credentials = CredentialSource::new(Some(Credential::Session(token)));
    let mapper = CloudMapper::with_credentials(base_url.clone(), credentials.clone());
    let client = CloudApiClient::new(base_url, credentials);
    let suffix = Uuid::new_v4().simple().to_string();
    let clan = client
        .create_clan(&format!("live-clan-{}", &suffix[..12]))
        .await
        .expect("create a clan")
        .id;
    let mut area = None;
    let outcome = clan_secrets(&mapper, &client, clan, &suffix, &mut area).await;
    let map_cleanup = match area {
        Some(area) => mapper.delete_area(&area).await,
        None => Ok(()),
    };
    let clan_cleanup = client.delete_clan(clan).await;
    outcome.expect("Clan Secrets round trip");
    map_cleanup.expect("delete the clan map");
    clan_cleanup.expect("dissolve the clan");
}

async fn clan_secrets(
    mapper: &CloudMapper,
    client: &CloudApiClient,
    clan: Uuid,
    suffix: &str,
    area_out: &mut Option<AreaId>,
) -> Result<(), String> {
    use smudgy_cloud::clan_secrets::{AccessReason, NewSecret, NewSecretOwner, OfferRequest};

    let me = client.me().await.map_err(|e| format!("me: {e}"))?.id;
    let atlas = client
        .create_clan_atlas(clan, "Roads")
        .await
        .map_err(|e| format!("clan folder: {e}"))?
        .id;
    let area = client
        .create_clan_area(
            clan,
            atlas,
            &format!("live-clan-secrets-{suffix}"),
            MapOwnership::Clan,
        )
        .await
        .map_err(|e| format!("clan map: {e}"))?
        .id;
    *area_out = Some(area);
    let resources = client
        .clan_map_resources(clan)
        .await
        .map_err(|e| format!("resources: {e}"))?;
    if !resources.iter().any(|row| {
        row.id == area
            && row.atlas_id == Some(atlas)
            && row.actions.contains("secret.create_member_owned")
            && row.actions.contains("secret.create_clan_owned")
    }) {
        return Err(format!("resources: {resources:?}"));
    }

    let generation = mapper.auth_generation();
    let members = mapper
        .create_secret_as(
            &area,
            &NewSecret {
                name: "Quest".to_string(),
                color: Some("#8A5CF6".to_string()),
                owner: NewSecretOwner::Members { clan_id: clan },
            },
            generation,
        )
        .await
        .map_err(|e| format!("Member-owned Secret: {e}"))?;
    if members.ownership != "members"
        || members.clan_id != Some(clan)
        || !members.actions.contains("manage_ownership")
        || members.color.as_deref() != Some("#8a5cf6")
    {
        return Err(format!("Member-owned summary: {members:?}"));
    }
    let access = client
        .secret_access(&members.source)
        .await
        .map_err(|e| format!("access: {e}"))?;
    if access.members.len() != 1
        || access.members[0].user_id != me
        || access.members[0].reasons != [AccessReason::Owner]
    {
        return Err(format!("Member-owned access: {access:?}"));
    }
    let owners = client
        .secret_owners(&members.source)
        .await
        .map_err(|e| format!("owners: {e}"))?;
    if owners.len() != 1 || owners[0].user_id != me || !owners[0].active {
        return Err(format!("owners: {owners:?}"));
    }
    // An owner is not offered what they own.
    match client
        .offer_secret_ownership(&members.source, &OfferRequest::to_members(vec![me], false))
        .await
    {
        Err(CloudError::NotFoundOrNoAccess) => {}
        other => return Err(format!("offer to an owner: {other:?}")),
    }

    let owned = mapper
        .create_secret_as(
            &area,
            &NewSecret {
                name: "Survey".to_string(),
                color: None,
                owner: NewSecretOwner::Clan { clan_id: clan },
            },
            generation,
        )
        .await
        .map_err(|e| format!("Clan-owned Secret: {e}"))?;
    if owned.ownership != "clan" || !owned.actions.contains("manage_ownership") {
        return Err(format!("Clan-owned summary: {owned:?}"));
    }
    let access = client
        .secret_access(&owned.source)
        .await
        .map_err(|e| format!("Clan-owned access: {e}"))?;
    if access.members.len() != 1
        || access.members[0].reasons.first() != Some(&AccessReason::ClanOwner)
    {
        return Err(format!("Clan-owned access: {access:?}"));
    }

    // The founder offers the Clan-owned Secret to themselves as a member,
    // and accepts: it becomes Member-owned, theirs alone.
    let offer = client
        .offer_secret_ownership(&owned.source, &OfferRequest::to_members(vec![me], false))
        .await
        .map_err(|e| format!("offer: {e}"))?;
    let mine = client
        .my_secret_offers()
        .await
        .map_err(|e| format!("my offers: {e}"))?;
    if !mine
        .iter()
        .any(|row| row.id == offer.id && row.secret_name == "Survey")
    {
        return Err(format!("my offers: {mine:?}"));
    }
    let pending = client
        .secret_offers(&owned.source)
        .await
        .map_err(|e| format!("offers: {e}"))?;
    if pending.len() != 1 || pending[0].recipients.len() != 1 {
        return Err(format!("offers: {pending:?}"));
    }
    let accepted = client
        .accept_secret_offer(&owned.source, offer.id)
        .await
        .map_err(|e| format!("accept: {e}"))?;
    if accepted.ownership != "members" {
        return Err(format!("accepted: {accepted:?}"));
    }
    let owners = client
        .secret_owners(&owned.source)
        .await
        .map_err(|e| format!("owners after: {e}"))?;
    if owners.iter().map(|owner| owner.user_id).collect::<Vec<_>>() != [me] {
        return Err(format!("owners after: {owners:?}"));
    }

    // A withdrawn offer is gone.
    let again = client
        .offer_secret_ownership(&members.source, &OfferRequest::to_clan(me))
        .await
        .map_err(|e| format!("offer to the clan: {e}"))?;
    client
        .withdraw_secret_offer(&members.source, again.id)
        .await
        .map_err(|e| format!("withdraw: {e}"))?;
    let pending = client
        .secret_offers(&members.source)
        .await
        .map_err(|e| format!("offers after withdrawing: {e}"))?;
    if !pending.is_empty() {
        return Err(format!("offers after withdrawing: {pending:?}"));
    }

    // The projection carries both, with their badges.
    let read = mapper
        .get_area(&area)
        .await
        .map_err(|e| format!("read: {e}"))?;
    let badges: Vec<(Option<String>, Option<Uuid>)> = read
        .sources
        .iter()
        .filter(|bundle| bundle.source.is_secret())
        .map(|bundle| (bundle.ownership.clone(), bundle.clan_id))
        .collect();
    let expected = vec![(Some("members".to_string()), Some(clan)); 2];
    if badges != expected {
        return Err(format!("badges: {badges:?}"));
    }
    Ok(())
}

/// A map with room 1 and a Secret keeping notes on it, for the copy and
/// transfer tests. Answers the Secret.
async fn map_with_noted_secret(mapper: &CloudMapper, area: AreaId) -> Result<SourceId, String> {
    let rooms = envelope(
        area,
        SourceId::map(),
        1,
        vec![AreaMutation::UpsertRoom {
            room_source: None,
            room_number: RoomNumber(1),
            body: RoomUpdates {
                title: Some("Gate".into()),
                ..Default::default()
            },
        }],
    );
    mapper
        .execute_mutation(&area, &rooms)
        .await
        .map_err(|e| format!("a map room: {e}"))?;
    let secret = mapper
        .create_secret(&area, "Behind The Bookcase", None, mapper.auth_generation())
        .await
        .map_err(|e| format!("create a Secret: {e}"))?
        .source;
    let notes = envelope(
        area,
        secret,
        1,
        vec![AreaMutation::UpsertRoomProperty {
            room_source: None,
            room_number: RoomNumber(1),
            name: "notes".into(),
            value: "the Secret's".into(),
        }],
    );
    mapper
        .execute_mutation(&area, &notes)
        .await
        .map_err(|e| format!("Secret notes: {e}"))?;
    Ok(secret)
}

fn new_map(name: &str) -> CreateAreaRequest {
    CreateAreaRequest {
        name: format!("{name}-{}", Uuid::new_v4().simple()),
        atlas_id: None,
        clan_id: None,
        ownership: None,
        ephemeral: false,
        properties: std::collections::BTreeMap::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "talks to a live cloud service; see the module docs"]
async fn copies_carry_the_secrets_their_copier_reads() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("SMUDGY_LIVE_STAGING / SMUDGY_LIVE_TOKEN unset; skipping");
        return;
    };
    assert!(
        !base_url.contains("api.smudgy.org"),
        "refusing to run against production"
    );
    let credentials = CredentialSource::new(Some(Credential::Session(token)));
    let mapper = CloudMapper::with_credentials(base_url.clone(), credentials.clone());
    let client = CloudApiClient::new(base_url, credentials);
    let area = mapper
        .create_area(new_map("live-format-3-copy"))
        .await
        .expect("create a map")
        .id;
    let mut copy = None;
    let outcome = copy_round_trip(&mapper, &client, area, &mut copy).await;
    let cleanup = mapper.delete_area(&area).await;
    let copy_cleanup = match copy {
        Some(copy) => mapper.delete_area(&copy).await,
        None => Ok(()),
    };
    outcome.expect("a copy carries the Secret");
    cleanup.expect("delete the map");
    copy_cleanup.expect("delete the copy");
}

async fn copy_round_trip(
    mapper: &CloudMapper,
    client: &CloudApiClient,
    area: AreaId,
    copy_id: &mut Option<AreaId>,
) -> Result<(), String> {
    let secret = map_with_noted_secret(mapper, area).await?;
    let copy = client
        .copy_area(area, &smudgy_cloud::cloud_api::CopyAreaRequest::default())
        .await
        .map_err(|e| format!("copy: {e}"))?
        .id;
    *copy_id = Some(copy);
    let carried = client
        .area_secrets(copy)
        .await
        .map_err(|e| format!("the copy's Secrets: {e}"))?;
    let [only] = carried.as_slice() else {
        return Err(format!("the copy's Secrets: {carried:?}"));
    };
    if only.name != "Behind The Bookcase" || only.ownership != "owner" || only.source == secret {
        return Err(format!("the copied Secret: {only:?}"));
    }
    let grants = client
        .secret_grants(&only.source)
        .await
        .map_err(|e| format!("the copied Secret's grants: {e}"))?;
    if !grants.is_empty() {
        return Err(format!("a copied Secret has grants: {grants:?}"));
    }
    let read = mapper
        .get_area(&copy)
        .await
        .map_err(|e| format!("read the copy: {e}"))?;
    let bundle = secret_bundle(&read, &only.source)?;
    let noted = bundle.room_data.iter().any(|data| {
        data.room_number == RoomNumber(1)
            && data
                .properties
                .iter()
                .any(|p| p.name == "notes" && p.value == "the Secret's")
    });
    if bundle.rev != 1 || !noted {
        return Err(format!("the copied Secret's content: {bundle:?}"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "talks to a live cloud service; see the module docs"]
async fn a_map_transferred_into_a_clan() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("SMUDGY_LIVE_STAGING / SMUDGY_LIVE_TOKEN unset; skipping");
        return;
    };
    assert!(
        !base_url.contains("api.smudgy.org"),
        "refusing to run against production"
    );
    let credentials = CredentialSource::new(Some(Credential::Session(token)));
    let mapper = CloudMapper::with_credentials(base_url.clone(), credentials.clone());
    let client = CloudApiClient::new(base_url, credentials);
    let suffix = Uuid::new_v4().simple().to_string();
    let clan = client
        .create_clan(&format!("live-format-3-clan-{}", &suffix[..12]))
        .await
        .expect("create a clan")
        .id;
    let area = mapper
        .create_area(new_map("live-format-3-transfer"))
        .await
        .expect("create a map")
        .id;
    let outcome = clan_transfer(&mapper, &client, clan, area).await;
    // Whoever owns it now, the map goes, then the clan.
    let cleanup = mapper.delete_area(&area).await;
    let dissolve = client.delete_clan(clan).await;
    outcome.expect("transfer into a clan");
    cleanup.expect("delete the map");
    dissolve.expect("dissolve the clan");
}

async fn clan_transfer(
    mapper: &CloudMapper,
    client: &CloudApiClient,
    clan: Uuid,
    area: AreaId,
) -> Result<(), String> {
    use smudgy_cloud::cloud_api::TransferDirection;
    let secret = map_with_noted_secret(mapper, area).await?;
    let folder = client
        .create_clan_atlas(clan, "Live Roads")
        .await
        .map_err(|e| format!("a clan folder: {e}"))?
        .id;
    let summary = client
        .clan(clan)
        .await
        .map_err(|e| format!("the clan: {e}"))?;
    if !summary.can(smudgy_cloud::clans::action::ACCEPT_TRANSFER) {
        return Err(format!(
            "an owner without atlas.accept_transfer: {summary:?}"
        ));
    }

    let operation = Uuid::new_v4();
    let moved = client
        .transfer_area_to_clan(area, clan, MapOwnership::Clan, folder, operation)
        .await
        .map_err(|e| format!("move into the clan: {e}"))?;
    if moved.status != "Accepted" || moved.to_clan_id != Some(clan) {
        return Err(format!("the completed transfer: {moved:?}"));
    }
    let repeated = client
        .transfer_area_to_clan(area, clan, MapOwnership::Clan, folder, operation)
        .await
        .map_err(|e| format!("retry the completed transfer: {e}"))?;
    if repeated.id != moved.id {
        return Err("retry changed the operation ID".into());
    }
    if client
        .transfers(TransferDirection::Offered)
        .await
        .map_err(|e| e.to_string())?
        .iter()
        .any(|offer| offer.id == moved.id)
    {
        return Err("a completed transfer became an offer".into());
    }

    let row = mapper
        .list_areas()
        .await
        .map_err(|e| format!("list maps: {e}"))?
        .into_iter()
        .find(|row| row.id == area)
        .ok_or("the clan's map is listed")?;
    let access = row.access.ok_or("an access block")?;
    if row.user_id.is_some()
        || row.clan_id != Some(clan)
        || row.atlas_id != Some(folder)
        || access.is_owner
        || access.can_admin
    {
        return Err(format!("the clan's map: {row:?}"));
    }
    let carried = client
        .area_secrets(area)
        .await
        .map_err(|e| format!("its Secrets: {e}"))?;
    let [only] = carried.as_slice() else {
        return Err(format!("its Secrets: {carried:?}"));
    };
    if only.source != secret
        || only.ownership != "members"
        || only.clan_id != Some(clan)
        || !only.actions.contains("manage_ownership")
    {
        return Err(format!("the Member-owned Secret: {only:?}"));
    }
    // A clan's map is never offered again, by anyone.
    match client
        .transfer_area_to_clan(area, clan, MapOwnership::Clan, folder, Uuid::new_v4())
        .await
    {
        Err(CloudError::NotFoundOrNoAccess) => Ok(()),
        other => Err(format!("a clan map offered: {other:?}")),
    }
}
