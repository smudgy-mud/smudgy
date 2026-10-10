//! Seeds a map with two Secrets into the cloud account a dev build is signed
//! in to, for trying how maps draw Secrets.
//!
//! Sign in once in a dev build (dev builds use the staging service), then:
//!
//! ```text
//! cargo run -p smudgy_core --example secret_demo
//! ```
//!
//! `SMUDGY_DEMO_TOKEN` seeds the account of that session token instead.
//!
//! It creates a map named `Secret Demo <time>`:
//! - a corridor of map rooms with two side rooms;
//! - "Behind The Bookcase": two rooms north of the corridor reached by a
//!   door from the map, with a label and a shape of its own;
//! - "Sewer Grate": a tunnel room between the two side rooms, and a passage
//!   between two map rooms that only it knows about.

use smudgy_cloud::mutation::{AreaMutation, MutationEnvelope, Precondition};
use smudgy_cloud::{
    AreaId, CloudMapper, CreateAreaRequest, Credential, CredentialSource, ExitArgs, ExitDirection,
    HorizontalAlignment, LabelArgs, MapperBackend, RoomNumber, RoomUpdates, ShapeArgs, ShapeType,
    SourceId, Uuid, VerticalAlignment,
};
use smudgy_core::models::{auth::load_session_token, settings::DEFAULT_API_BASE_URL};

/// A room at a map position.
fn room(number: i32, source: Option<SourceId>, title: &str, x: f32, y: f32) -> AreaMutation {
    AreaMutation::CreateRoom {
        room_number: RoomNumber(number),
        room_source: source,
        body: RoomUpdates {
            title: Some(title.to_string()),
            x: Some(x),
            y: Some(y),
            ..RoomUpdates::default()
        },
    }
}

/// One exit; the reciprocal exit pairs with it into one connection.
fn exit(
    area: AreaId,
    (from, from_source): (i32, Option<SourceId>),
    direction: ExitDirection,
    (to, to_source): (i32, Option<SourceId>),
    back: ExitDirection,
) -> AreaMutation {
    AreaMutation::CreateExit {
        room_number: RoomNumber(from),
        room_source: from_source,
        body: ExitArgs {
            from_direction: direction,
            to_area_id: Some(area),
            to_room_number: Some(RoomNumber(to)),
            to_source,
            to_direction: Some(back),
            weight: 1.0,
            ..ExitArgs::default()
        },
    }
}

/// Both directions of a two-way link.
fn link(
    area: AreaId,
    a: (i32, Option<SourceId>),
    out: ExitDirection,
    b: (i32, Option<SourceId>),
    back: ExitDirection,
) -> [AreaMutation; 2] {
    [exit(area, a, out, b, back), exit(area, b, back, a, out)]
}

async fn write(
    mapper: &CloudMapper,
    area: AreaId,
    source: SourceId,
    rev: i64,
    payload: Vec<AreaMutation>,
) -> Result<(), String> {
    let envelope = MutationEnvelope {
        operation_id: Uuid::new_v4(),
        source,
        preconditions: vec![Precondition::source(area.0, source, rev)],
        payload,
    };
    mapper
        .execute_mutation(&area, &envelope)
        .await
        .map(|_| ())
        .map_err(|e| format!("writing {source}: {e}"))
}

async fn seed(mapper: &CloudMapper) -> Result<String, String> {
    let name = format!("Secret Demo {}", chrono::Local::now().format("%H:%M:%S"));
    let area = mapper
        .create_area(CreateAreaRequest {
            name: name.clone(),
            atlas_id: None,
            clan_id: None,
            ownership: None,
            ephemeral: false,
            properties: std::collections::BTreeMap::default(),
        })
        .await
        .map_err(|e| format!("creating the map: {e}"))?
        .id;
    let map = None;
    let mut payload = vec![
        room(1, map, "West Gate", 0.0, 0.0),
        room(2, map, "Lamp Street", 1.0, 0.0),
        room(3, map, "Library Steps", 2.0, 0.0),
        room(4, map, "Market Row", 3.0, 0.0),
        room(5, map, "East Gate", 4.0, 0.0),
        room(6, map, "Cooper's Yard", 1.0, 1.0),
        room(7, map, "Dye Works", 3.0, 1.0),
    ];
    for (a, b) in [(1, 2), (2, 3), (3, 4), (4, 5)] {
        payload.extend(link(
            area,
            (a, map),
            ExitDirection::East,
            (b, map),
            ExitDirection::West,
        ));
    }
    payload.extend(link(
        area,
        (2, map),
        ExitDirection::South,
        (6, map),
        ExitDirection::North,
    ));
    payload.extend(link(
        area,
        (4, map),
        ExitDirection::South,
        (7, map),
        ExitDirection::North,
    ));
    write(mapper, area, SourceId::Map, 1, payload).await?;

    let generation = mapper.auth_generation();
    let study = mapper
        .create_secret(&area, "Behind The Bookcase", None, generation)
        .await
        .map_err(|e| format!("creating a Secret: {e}"))?
        .source;
    let own = Some(study);
    let mut payload = vec![
        room(1, own, "Hidden Study", 2.0, -1.0),
        room(2, own, "Reading Nook", 3.0, -1.0),
        AreaMutation::CreateLabel {
            body: LabelArgs {
                x: 1.6,
                y: -2.0,
                width: 1.8,
                height: 0.35,
                horizontal_alignment: HorizontalAlignment::Left,
                vertical_alignment: VerticalAlignment::Center,
                text: "Pull the red book".to_string(),
                color: "#e8e8e8".to_string(),
                font_size: 12,
                font_weight: 400,
                ..LabelArgs::default()
            },
        },
        AreaMutation::CreateShape {
            body: ShapeArgs {
                x: 1.55,
                y: -1.45,
                width: 1.9,
                height: 0.9,
                shape_type: ShapeType::RoundedRectangle,
                border_radius: 0.15,
                background_color: Some("#2a2a33".to_string()),
                ..ShapeArgs::default()
            },
        },
    ];
    payload.extend(link(
        area,
        (3, map),
        ExitDirection::North,
        (1, own),
        ExitDirection::South,
    ));
    payload.extend(link(
        area,
        (1, own),
        ExitDirection::East,
        (2, own),
        ExitDirection::West,
    ));
    write(mapper, area, study, 1, payload).await?;

    let sewer = mapper
        .create_secret(&area, "Sewer Grate", None, generation)
        .await
        .map_err(|e| format!("creating a Secret: {e}"))?
        .source;
    let own = Some(sewer);
    let mut payload = vec![room(1, own, "Sewer Tunnel", 2.0, 1.0)];
    payload.extend(link(
        area,
        (6, map),
        ExitDirection::East,
        (1, own),
        ExitDirection::West,
    ));
    payload.extend(link(
        area,
        (1, own),
        ExitDirection::East,
        (7, map),
        ExitDirection::West,
    ));
    payload.extend(link(
        area,
        (1, map),
        ExitDirection::Southeast,
        (6, map),
        ExitDirection::Northwest,
    ));
    write(mapper, area, sewer, 1, payload).await?;
    Ok(name)
}

#[tokio::main]
async fn main() {
    // SMUDGY_DEMO_TOKEN seeds another account (a test fixture) instead.
    let token = std::env::var("SMUDGY_DEMO_TOKEN")
        .ok()
        .or_else(load_session_token);
    let Some(token) = token else {
        eprintln!(
            "No session found: sign in to the cloud once in a dev build, then run this again."
        );
        std::process::exit(1);
    };
    if !DEFAULT_API_BASE_URL.contains("staging") {
        eprintln!("This example seeds the staging service only; build it as a dev build.");
        std::process::exit(1);
    }
    let mapper = CloudMapper::with_credentials(
        DEFAULT_API_BASE_URL.to_string(),
        CredentialSource::new(Some(Credential::Session(token))),
    );
    match seed(&mapper).await {
        Ok(name) => {
            println!("Created \"{name}\" on {DEFAULT_API_BASE_URL}. Open it in the map editor.")
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
