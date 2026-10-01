//! End-to-end coverage for the `nukefire-mapper` package's canonical areas:
//! one map per area name the game reports. Every room records its zone, and
//! the first time in a session the player stands in a zone, the mapper
//! gathers all of that zone's rooms into the map its area name belongs in.
//!
//! Each test seeds a local store the way older package versions left it, with
//! per-zone maps marked by `nukefire.mapper`, tied to their zone by
//! `nukefire.zone` and holding rooms that carry their zone and the game's
//! VNUM. It then sends the `Room.Info` and `NukeFire.Map.Local` messages the
//! game sends when the player stands in a room, and reads the result from
//! the maps and the session's transcript.

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use smudgy_cloud::ExitDirection::{self, Down, East, North, South, Up, West};
use smudgy_cloud::mapper::RoomKey;
use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{
    AreaId, AtlasId, ConnectionId, ConnectionKind, ConnectionRouting, ConnectionUpdates, ExitArgs,
    LabelArgs, MapDestination, MapStorage, Mapper, RoomNumber, RoomUpdates,
};
use smudgy_core::models::shared_packages;
use support::nukefire::{
    ChartLink, ChartRoom, MAPPER_SPEC, NOTICE_PREFIX, PROFILE, Session, find_file,
    install_mapper_packages, local_mapper, smudgy_home, start_session,
};

/// On a room: its zone. On a map an older version made: the zone it was made for.
const ZONE: &str = "nukefire.zone";
/// On a map: the folded area name it is the map for.
const AREA_KEY: &str = "nukefire.area";
/// On a map: present on every map the mapper made.
const MANAGED: &str = "nukefire.mapper";
const MANAGED_BY: &str = "NukeFire.Map.Local";
/// The terrain every chart in these tests reports. Seeded rooms carry none,
/// and mapping a room writes the terrain its chart reports, so a room that
/// shows it has been through a whole mapping pass, settling included.
const VISITED_TERRAIN: &str = "field";

/// The zone the player stands in folds its other section into the family's
/// largest map, and the link between the sections becomes a link inside it.
#[tokio::test]
async fn visiting_a_zone_folds_its_other_sections_into_one_map() {
    let server = "NukeFireFoldsSections";
    let (mapper, atlas) = nukefire_store(server).await;
    // The Deathlands as 0.17.x left them: zone 316's map, which also holds a
    // room of zone 315, and zone 315's own section, linked across the two.
    let deathlands = legacy_map(&mapper, atlas, "The Deathlands", 316).await;
    add_rooms(&mapper, deathlands, 316, &[31601, 31602, 31603]).await;
    add_rooms(&mapper, deathlands, 315, &[31599]).await;
    let section = legacy_map(&mapper, atlas, "The Deathlands", 315).await;
    add_rooms(&mapper, section, 315, &[31501, 31502]).await;
    link(&mapper, 31501, West, 31599, East).await;
    let rooms = room_count(&mapper);

    let mut session = start_session(server, 9401, &mapper).await;
    visit(&mut session, &mapper, "The Deathlands", 31501, 315).await;
    expect_notice(&mut session, "Combined 2 map sections into The Deathlands.").await;

    assert_eq!(
        maps_named(&mapper, "The Deathlands"),
        [deathlands],
        "one map for the name, and it is the family's largest"
    );
    assert_eq!(
        property(&mapper, deathlands, AREA_KEY).as_deref(),
        Some("the deathlands")
    );
    assert_eq!(maps_holding_zone(&mapper, 315), [deathlands]);
    assert!(
        !exists(&mapper, section),
        "the zone's other section is gone"
    );
    assert_eq!(
        exit_target(&mapper, 31501, West),
        Some(room_key(&mapper, 31599)),
        "the link between the sections now runs inside the map"
    );
    assert_eq!(
        exit_target(&mapper, 31599, East),
        Some(room_key(&mapper, 31501))
    );
    assert_eq!(room_count(&mapper), rooms, "no room is lost or added");
    expect_only_notices(&session, &["Combined 2 map sections into The Deathlands."]);
    session.shutdown();
}

/// Rooms of a zone that sit in another area's map move out into a map of
/// their own; the other map keeps its rooms and labels, and a link across
/// the zone border becomes a pair of links between the two maps.
#[tokio::test]
async fn a_zones_rooms_inside_another_familys_map_move_out_on_its_first_visit() {
    let server = "NukeFireMovesStrayRooms";
    let (mapper, atlas) = nukefire_store(server).await;
    // Caldera's map as an older version left it, holding two rooms of the
    // Night Lands next door. The game has never named the Night Lands zone
    // while the player stood in it, so it has no map of its own yet.
    let caldera = legacy_map(&mapper, atlas, "Caldera", 601).await;
    add_rooms(&mapper, caldera, 601, &[60101, 60102]).await;
    add_rooms(&mapper, caldera, 602, &[60201, 60202]).await;
    add_label(&mapper, caldera, "Caldera rim").await;
    link(&mapper, 60102, East, 60201, West).await;
    link(&mapper, 60201, East, 60202, West).await;

    let mut session = start_session(server, 9402, &mapper).await;
    visit(&mut session, &mapper, "Night Lands", 60201, 602).await;

    let night_lands = map_of(&mapper, 60201);
    assert_ne!(
        night_lands,
        caldera,
        "the Night Lands rooms are still in Caldera's map, whose key is now {:?}",
        property(&mapper, caldera, AREA_KEY)
    );
    assert_eq!(maps_holding_zone(&mapper, 602), [night_lands]);
    assert_eq!(name(&mapper, night_lands), "Night Lands");
    assert_eq!(
        property(&mapper, night_lands, AREA_KEY).as_deref(),
        Some("night lands")
    );
    assert_eq!(
        maps_holding_zone(&mapper, 601),
        [caldera],
        "Caldera keeps its own rooms"
    );
    assert_eq!(rooms_in(&mapper, caldera), 2);
    assert_eq!(labels(&mapper, caldera), ["Caldera rim"]);
    assert_eq!(
        exit_target(&mapper, 60102, East),
        Some(room_key(&mapper, 60201)),
        "the link across the zone border now leads into the Night Lands map"
    );
    assert_eq!(
        exit_target(&mapper, 60201, West),
        Some(room_key(&mapper, 60102))
    );
    expect_notice(&mut session, "Moved 2 rooms into Night Lands.").await;
    expect_only_notices(&session, &["Moved 2 rooms into Night Lands."]);
    session.shutdown();
}

/// The default rules send numbered areas to their family's map, and tell
/// "DARK PLEASURES", in exactly those capitals, from "Dark Pleasures".
#[tokio::test]
async fn aliases_and_exact_case_rules_decide_which_map_a_zone_joins() {
    let server = "NukeFireAreaNameRules";
    let (mapper, atlas) = nukefire_store(server).await;
    let vega_jane = legacy_map(&mapper, atlas, "Vega Jane", 700).await;
    add_rooms(&mapper, vega_jane, 700, &[70001, 70002, 70003]).await;
    let vega_jane_ii = legacy_map(&mapper, atlas, "Vega Jane II", 701).await;
    add_rooms(&mapper, vega_jane_ii, 701, &[70101, 70102]).await;
    let darker = legacy_map(&mapper, atlas, "Darker Pleasures", 801).await;
    add_rooms(&mapper, darker, 801, &[80101, 80102, 80103]).await;
    let shouted = legacy_map(&mapper, atlas, "DARK PLEASURES", 800).await;
    add_rooms(&mapper, shouted, 800, &[80001, 80002]).await;
    let dark = legacy_map(&mapper, atlas, "Dark Pleasures", 802).await;
    add_rooms(&mapper, dark, 802, &[80201]).await;

    let mut session = start_session(server, 9403, &mapper).await;
    visit(&mut session, &mapper, "Vega Jane II", 70101, 701).await;
    expect_notice(&mut session, "Combined 2 map sections into Vega Jane.").await;
    assert_eq!(maps_holding_zone(&mapper, 701), [vega_jane]);
    assert!(!exists(&mapper, vega_jane_ii));
    assert_eq!(
        property(&mapper, vega_jane, AREA_KEY).as_deref(),
        Some("vega jane")
    );

    visit(&mut session, &mapper, "DARK PLEASURES", 80001, 800).await;
    expect_notice(
        &mut session,
        "Combined 2 map sections into Darker Pleasures.",
    )
    .await;
    assert_eq!(maps_holding_zone(&mapper, 800), [darker]);
    assert!(!exists(&mapper, shouted));
    assert_eq!(
        property(&mapper, darker, AREA_KEY).as_deref(),
        Some("darker pleasures")
    );

    visit(&mut session, &mapper, "Dark Pleasures", 80201, 802).await;
    assert_eq!(maps_holding_zone(&mapper, 802), [dark]);
    assert_eq!(
        property(&mapper, dark, AREA_KEY).as_deref(),
        Some("dark pleasures"),
        "a different capitalization is an area of its own"
    );
    assert_eq!(rooms_in(&mapper, darker), 5);
    expect_only_notices(
        &session,
        &[
            "Combined 2 map sections into Vega Jane.",
            "Combined 2 map sections into Darker Pleasures.",
        ],
    );
    session.shutdown();
}

/// A rule the player adds sends a zone that already has a map of its own
/// into the map the rule names, the next time the player stands in it.
#[tokio::test]
async fn a_player_rule_moves_a_zone_on_its_next_visit() {
    let server = "NukeFirePlayerRule";
    let (mapper, atlas) = nukefire_store(server).await;
    // Both zones were settled into maps of their own on earlier visits.
    let forest = settled_map(&mapper, atlas, "Eastern Forest", "eastern forest").await;
    add_rooms(&mapper, forest, 900, &[90001, 90002, 90003]).await;
    let knob = settled_map(
        &mapper,
        atlas,
        "Forest around Hermit's Knob",
        "forest around hermit's knob",
    )
    .await;
    add_rooms(&mapper, knob, 901, &[90101, 90102]).await;
    shared_packages::save_param_value(
        server,
        MAPPER_SPEC,
        "areaNameRules",
        json!([{ "name": "Forest around Hermit's Knob", "map": "Eastern Forest", "exactCase": false }]),
    )
    .expect("save the player's rule");

    let mut session = start_session(server, 9404, &mapper).await;
    visit(
        &mut session,
        &mapper,
        "Forest around Hermit's Knob",
        90101,
        901,
    )
    .await;
    expect_notice(&mut session, "Combined 2 map sections into Eastern Forest.").await;

    assert_eq!(maps_holding_zone(&mapper, 901), [forest]);
    assert_eq!(maps_holding_zone(&mapper, 900), [forest]);
    assert!(!exists(&mapper, knob), "the zone's own map folded in");
    assert_eq!(
        property(&mapper, forest, AREA_KEY).as_deref(),
        Some("eastern forest")
    );
    expect_only_notices(&session, &["Combined 2 map sections into Eastern Forest."]);
    session.shutdown();
}

/// Empty maps older versions left for a zone, tied to it by their
/// `nukefire.zone` or by a `NukeFire Zone N` name, go when the zone is
/// visited, without a word. Empty maps the player made stay.
#[tokio::test]
async fn empty_sections_left_by_older_versions_are_removed_when_their_zone_is_visited() {
    let server = "NukeFireEmptySections";
    let (mapper, atlas) = nukefire_store(server).await;
    let kreel = legacy_map(&mapper, atlas, "Mines of Kreel", 1000).await;
    add_rooms(&mapper, kreel, 1000, &[100_001, 100_002]).await;
    let shell = legacy_map(&mapper, atlas, "Mines of Kreel", 1000).await;
    let placeholder = seed_map(
        &mapper,
        Some(atlas),
        "NukeFire Zone 1000",
        &[(MANAGED, MANAGED_BY)],
    )
    .await;
    let players_notes = seed_map(&mapper, None, "Mines of Kreel", &[]).await;
    let players_placeholder = seed_map(&mapper, Some(atlas), "NukeFire Zone 1000", &[]).await;

    let mut session = start_session(server, 9405, &mapper).await;
    visit(&mut session, &mapper, "Mines of Kreel", 100_001, 1000).await;

    assert!(
        !exists(&mapper, shell),
        "the empty section tied by its zone is gone"
    );
    assert!(
        !exists(&mapper, placeholder),
        "the empty placeholder tied by its name is gone"
    );
    assert!(
        exists(&mapper, players_notes),
        "the player's empty map stays"
    );
    assert!(
        exists(&mapper, players_placeholder),
        "an empty map the player made stays whatever its name"
    );
    assert_eq!(maps_holding_zone(&mapper, 1000), [kreel]);
    assert_eq!(
        property(&mapper, kreel, AREA_KEY).as_deref(),
        Some("mines of kreel")
    );
    session.run_for(Duration::from_secs(1)).await;
    expect_only_notices(&session, &[]);
    session.shutdown();
}

/// A zone seen only at the edge of the chart waits in a `NukeFire Zone N`
/// map; standing in it later adopts that map under the zone's area name.
#[tokio::test]
async fn glimpsed_zones_stay_in_placeholder_maps_until_visited() {
    let server = "NukeFireGlimpsedZones";
    let (mapper, _) = nukefire_store(server).await;
    let gate = chart_room(120_001, "Gulch Gate", 1200, 0);
    let edge = chart_room(120_101, "Flats Edge", 1201, 1);
    let road = chart_room(120_102, "Flats Road", 1201, 2);
    let links = [
        ChartLink {
            from: 120_001,
            to: 120_101,
            direction: "east",
        },
        ChartLink {
            from: 120_101,
            to: 120_102,
            direction: "east",
        },
    ];

    let mut session = start_session(server, 9406, &mapper).await;
    // The player stands at the edge of Rusty Gulch; the next zone's rooms
    // show on the chart, but the game has not named its area.
    session.visit("Rusty Gulch", &gate, &[edge, road], &links);
    let charted = session
        .wait_until(|_| {
            [120_001, 120_101, 120_102]
                .iter()
                .all(|&vnum| find(&mapper, vnum).is_some())
        })
        .await;
    assert!(
        charted,
        "the chart was never mapped:\n{}",
        session.transcript()
    );

    let placeholder = map_of(&mapper, 120_101);
    assert_eq!(name(&mapper, placeholder), "NukeFire Zone 1201");
    assert_eq!(
        property(&mapper, placeholder, ZONE).as_deref(),
        Some("1201")
    );
    assert_eq!(
        property(&mapper, placeholder, MANAGED).as_deref(),
        Some(MANAGED_BY)
    );
    assert_eq!(maps_holding_zone(&mapper, 1201), [placeholder]);
    let gulch = map_of(&mapper, 120_001);
    assert_eq!(name(&mapper, gulch), "Rusty Gulch");
    assert_eq!(
        property(&mapper, gulch, AREA_KEY).as_deref(),
        Some("rusty gulch")
    );

    session.visit("Dusty Flats", &edge, &[road], &links[1..]);
    // Adopting binds the map to the area's name, then renames it.
    let adopted = session
        .wait_until(|_| {
            property(&mapper, placeholder, AREA_KEY).is_some()
                && name(&mapper, placeholder) == "Dusty Flats"
        })
        .await;
    assert!(
        adopted,
        "the placeholder was never adopted:\n{}",
        session.transcript()
    );
    assert_eq!(
        maps_named(&mapper, "Dusty Flats"),
        [placeholder],
        "the placeholder itself is adopted and takes the area's name"
    );
    assert_eq!(
        property(&mapper, placeholder, AREA_KEY).as_deref(),
        Some("dusty flats")
    );
    assert_eq!(maps_holding_zone(&mapper, 1201), [placeholder]);
    session.run_for(Duration::from_secs(1)).await;
    expect_only_notices(&session, &[]);
    session.shutdown();
}

/// A zone the player keeps in a map of their own stays there, even when its
/// rooms were stored without their zone, as in a map they imported; the
/// zone's new rooms join that map, and no map is made for the zone.
#[tokio::test]
async fn a_zone_in_a_map_the_player_made_stays_there_and_its_new_rooms_join_it() {
    let server = "NukeFirePlayersMap";
    let (mapper, _) = nukefire_store(server).await;
    let theirs = seed_map(&mapper, None, "Crater Notes", &[]).await;
    add_stored_rooms(&mapper, theirs, None, &[130_001, 130_002]).await;
    let maps = map_count(&mapper);

    let mut session = start_session(server, 9408, &mapper).await;
    let rim = chart_room(130_001, "Room 130001", 1300, 0);
    let ledge = chart_room(130_002, "Room 130002", 1300, 1);
    let floor = chart_room(130_003, "Crater Floor", 1300, 2);
    let links = [
        ChartLink {
            from: 130_001,
            to: 130_002,
            direction: "east",
        },
        ChartLink {
            from: 130_002,
            to: 130_003,
            direction: "east",
        },
    ];
    session.visit("The Crater", &rim, &[ledge, floor], &links);
    let charted = session
        .wait_until(|_| find(&mapper, 130_003).is_some())
        .await;
    assert!(
        charted,
        "the crater floor was never mapped:\n{}",
        session.transcript()
    );

    assert_eq!(
        map_of(&mapper, 130_003),
        theirs,
        "the zone's new room joins the player's map"
    );
    assert_eq!(maps_holding_zone(&mapper, 1300), [theirs]);
    assert_eq!(map_count(&mapper), maps, "no map is made for the zone");
    assert_eq!(property(&mapper, theirs, AREA_KEY), None);
    assert_eq!(property(&mapper, theirs, MANAGED), None);
    session.run_for(Duration::from_secs(1)).await;
    expect_only_notices(&session, &[]);
    session.shutdown();
}

/// Every change settling makes is in the mapper's decision log, like every
/// other mapper change: the map it binds, the move and the polish request,
/// then the settled zone itself.
#[tokio::test]
async fn settling_records_each_change_in_the_decision_log() {
    let server = "NukeFireSettleLog";
    let (mapper, atlas) = nukefire_store(server).await;
    let vega_jane = legacy_map(&mapper, atlas, "Vega Jane", 700).await;
    add_rooms(&mapper, vega_jane, 700, &[70001, 70002, 70003]).await;
    let vega_jane_ii = legacy_map(&mapper, atlas, "Vega Jane II", 701).await;
    add_rooms(&mapper, vega_jane_ii, 701, &[70101, 70102]).await;
    shared_packages::save_param_value(server, MAPPER_SPEC, "debugMappingDecisions", json!(true))
        .expect("turn the decision log on");

    let mut session = start_session(server, 9409, &mapper).await;
    visit(&mut session, &mapper, "Vega Jane II", 70101, 701).await;

    let into = vega_jane.to_string();
    let completed = |api: &'static str| {
        let into = into.clone();
        move |record: &Value| {
            record["kind"] == "mutation-complete"
                && record["api"] == api
                && record["areaId"] == into.as_str()
        }
    };
    expect_logged(
        &mut session,
        server,
        "binding the map",
        completed("setAreaProperty"),
    )
    .await;
    expect_logged(&mut session, server, "the move", |record| {
        completed("mergeAreas")(record) && record["result"]["moved"] == 2
    })
    .await;
    expect_logged(&mut session, server, "the polish request", |record| {
        record["kind"] == "layout-polish-state"
            && record["area"]["id"] == into.as_str()
            && record["event"] == "topology-deferred"
            && record["pending"] == true
    })
    .await;
    expect_logged(&mut session, server, "the settled zone", |record| {
        record["kind"] == "zone-settled"
            && record["zone"] == 701
            && record["into"] == into.as_str()
            && record["destination"] == "adopt"
            && record["moved"] == 2
    })
    .await;
    session.shutdown();
}

/// Combining sections leaves their seams to the polish that follows: it first
/// moves only the rooms around the seams, then polishes the whole map from
/// the layout the combining left, not from that preview, and when it is done
/// the map is polished and no seam is left to preview.
#[tokio::test]
async fn combined_sections_polish_their_seams_first_and_then_the_whole_map() {
    let server = "NukeFireSeamPreview";
    let (mapper, atlas) = nukefire_store(server).await;
    // A row of six rooms, and a section of two whose rooms lie south of the
    // row's first and third rooms. The section moves as one block, which
    // cannot meet both of those links, so the combined map starts with a
    // seam the polish must straighten.
    let row = legacy_map(&mapper, atlas, "Seam Flats", 1400).await;
    let row_rooms = [140_001, 140_002, 140_003, 140_004, 140_005, 140_006];
    add_rooms(&mapper, row, 1400, &row_rooms).await;
    for pair in row_rooms.windows(2) {
        link(&mapper, pair[0], East, pair[1], West).await;
    }
    let section = legacy_map(&mapper, atlas, "Seam Flats", 1401).await;
    add_rooms(&mapper, section, 1401, &[140_101, 140_102]).await;
    link(&mapper, 140_001, South, 140_101, North).await;
    link(&mapper, 140_003, South, 140_102, North).await;
    shared_packages::save_param_value(server, MAPPER_SPEC, "debugMappingDecisions", json!(true))
        .expect("turn the decision log on");

    let mut session = start_session(server, 9410, &mapper).await;
    visit(&mut session, &mapper, "Seam Flats", 140_101, 1401).await;
    expect_notice(&mut session, "Combined 2 map sections into Seam Flats.").await;
    assert!(!exists(&mapper, section));
    let into = row.to_string();
    let in_row = |record: &Value, kind: &str| {
        record["kind"] == kind && record["area"]["id"] == into.as_str()
    };
    // The seams are the rooms at either end of the two links between sections.
    expect_logged(&mut session, server, "the seams", |record| {
        in_row(record, "layout-polish-state")
            && record["event"] == "topology-deferred"
            && record["seams"] == 4
    })
    .await;
    // The completed polish of the whole map leaves no seam to preview.
    expect_logged(&mut session, server, "the completed polish", |record| {
        in_row(record, "layout-polish-state")
            && record["event"] == "polish-completed"
            && record["seams"] == 0
    })
    .await;

    let records = decision_records(server);
    let round = records
        .iter()
        .position(|record| in_row(record, "layout-seam-round"))
        .expect("the seam round");
    expect_seam_round(&records[round]);
    assert!(
        records[..round]
            .iter()
            .any(|record| in_row(record, "layout-progress-applied") && record["seamRound"] == true),
        "the round's layout went on the map"
    );

    // The whole-map polish planned from the layout the combining left: the
    // row where it was and the section's rooms side by side, as they were
    // drawn, although the preview had already moved them apart.
    let decision = records[round..]
        .iter()
        .find(|record| {
            in_row(record, "layout-decision") && record["trigger"]["moveExisting"] == true
        })
        .expect("the whole-map polish's decision, after the round");
    for (column, &vnum) in (1..).zip(&row_rooms) {
        assert_eq!(
            planned_cell(decision, &mapper, vnum),
            (column, 0),
            "row room {vnum} as seeded"
        );
    }
    let (first, second) = (
        planned_cell(decision, &mapper, 140_101),
        planned_cell(decision, &mapper, 140_102),
    );
    assert_eq!(
        (second.0 - first.0, second.1 - first.1),
        (1, 0),
        "the section's rooms as the combining left them"
    );

    // The map ends polished: each section room south of its row room.
    assert_eq!(cell(&mapper, 140_101), below(cell(&mapper, 140_001)));
    assert_eq!(cell(&mapper, 140_102), below(cell(&mapper, 140_003)));
    assert_eq!(
        property(&mapper, row, "nukefire.layout.polish-seams").unwrap_or_default(),
        "",
        "the completed polish clears the seams"
    );
    expect_only_notices(&session, &["Combined 2 map sections into Seam Flats."]);
    session.shutdown();
}

/// Polish may move rooms between levels. A Connection whose rooms it parts
/// runs straight from then on, and one whose rooms it brings onto one level
/// is routed once the move has committed: only rooms on one level may be
/// joined by a drawn route, so either would otherwise fail the edit.
#[tokio::test]
async fn polish_moves_rooms_between_levels_together_with_their_connections() {
    let server = "NukeFireLevelRoutes";
    let (mapper, atlas) = nukefire_store(server).await;
    let map = settled_map(&mapper, atlas, "Level Steps", "level steps").await;
    // Up and Down join two rooms drawn side by side, under the route the
    // mapper draws between rooms on one level; East and West join two rooms
    // a level apart.
    add_rooms(&mapper, map, 1500, &[150_001, 150_002, 150_003, 150_004]).await;
    link(&mapper, 150_001, Up, 150_002, Down).await;
    link(&mapper, 150_003, East, 150_004, West).await;
    let mut edits = vec![
        AreaMutation::UpsertRoom {
            room_number: room_key(&mapper, 150_004).room_number,
            body: RoomUpdates {
                level: Some(1),
                ..RoomUpdates::default()
            },
        },
        AreaMutation::UpsertAreaProperty {
            name: "nukefire.layout.polish-pending".to_string(),
            value: "true".to_string(),
            is_secret: None,
        },
    ];
    let stairs = connections_between(&mapper, 150_001, 150_002);
    assert!(!stairs.is_empty(), "the stairs are joined");
    edits.extend(
        stairs
            .iter()
            .map(|&connection_id| AreaMutation::UpdateConnection {
                connection_id,
                body: ConnectionUpdates {
                    routing: Some(ConnectionRouting::Automatic),
                    ..ConnectionUpdates::default()
                },
            }),
    );
    apply(&mapper, map, edits).await;
    let slope = connections_between(&mapper, 150_003, 150_004);
    assert!(
        slope
            .iter()
            .all(|&id| connection(&mapper, map, id).0 == ConnectionKind::CrossLevel),
        "the slope joins two levels"
    );
    shared_packages::save_param_value(server, MAPPER_SPEC, "debugMappingDecisions", json!(true))
        .expect("turn the decision log on");

    let mut session = start_session(server, 9420, &mapper).await;
    visit(&mut session, &mapper, "Level Steps", 150_001, 1500).await;
    let in_map = map.to_string();
    expect_logged(&mut session, server, "the completed polish", |record| {
        record["kind"] == "layout-polish-state"
            && record["event"] == "polish-completed"
            && record["area"]["id"] == in_map.as_str()
    })
    .await;
    expect_no_failed_edit(&session, server);

    assert_ne!(
        level(&mapper, 150_001),
        level(&mapper, 150_002),
        "the polish stacked the stairs"
    );
    for &id in &stairs {
        assert_eq!(
            connection(&mapper, map, id),
            (ConnectionKind::CrossLevel, ConnectionRouting::Simple),
            "between levels the stairs run straight"
        );
    }
    assert_eq!(
        level(&mapper, 150_003),
        level(&mapper, 150_004),
        "the polish flattened the slope"
    );
    for &id in &slope {
        assert_eq!(
            connection(&mapper, map, id),
            (ConnectionKind::Internal, ConnectionRouting::Automatic),
            "on one level the mapper routes the slope's Connection"
        );
    }
    session.shutdown();
}

/// An exit the game charts between rooms on different levels joins them with
/// a Connection that runs straight.
#[tokio::test]
async fn a_charted_exit_between_levels_joins_its_rooms_with_a_straight_connection() {
    let server = "NukeFireLevelLink";
    let (mapper, atlas) = nukefire_store(server).await;
    let map = settled_map(&mapper, atlas, "Level Ledge", "level ledge").await;
    add_rooms(&mapper, map, 1510, &[151_001, 151_002]).await;
    apply(
        &mapper,
        map,
        vec![AreaMutation::UpsertRoom {
            room_number: room_key(&mapper, 151_002).room_number,
            body: RoomUpdates {
                level: Some(1),
                ..RoomUpdates::default()
            },
        }],
    )
    .await;
    shared_packages::save_param_value(server, MAPPER_SPEC, "debugMappingDecisions", json!(true))
        .expect("turn the decision log on");

    let mut session = start_session(server, 9421, &mapper).await;
    // The player stands in the lower room, and the game charts the upper one
    // east of it.
    let lower = chart_room(151_001, "Room 151001", 1510, 0);
    let upper = ChartRoom {
        z: 1,
        ..chart_room(151_002, "Room 151002", 1510, 1)
    };
    let east = [ChartLink {
        from: 151_001,
        to: 151_002,
        direction: "east",
    }];
    session.visit("Level Ledge", &lower, &[upper], &east);
    let joined = session
        .wait_until(|_| !connections_between(&mapper, 151_001, 151_002).is_empty())
        .await;
    assert!(
        joined,
        "the mapper never joined the rooms:\n{}",
        session.transcript()
    );
    expect_no_failed_edit(&session, server);
    for id in connections_between(&mapper, 151_001, 151_002) {
        let (kind, routing) = connection(&mapper, map, id);
        assert!(
            kind.allows_routing(routing),
            "a {kind} Connection may not be {routing}"
        );
    }
    session.shutdown();
}

/// `nfmap tidy` settles every zone the mapper's maps name, as a visit would,
/// and polishes every map, saying what it checks and combines as it goes.
#[tokio::test]
async fn nfmap_tidy_combines_every_named_zone_and_polishes_every_map() {
    let server = "NukeFireTidy";
    let (mapper, atlas) = nukefire_store(server).await;
    // Seam Flats in two sections; a zone only a placeholder holds; and a
    // glade the player stands in first, which settles it this session.
    let row = legacy_map(&mapper, atlas, "Seam Flats", 1600).await;
    add_rooms(&mapper, row, 1600, &[160_001, 160_002, 160_003]).await;
    link(&mapper, 160_001, East, 160_002, West).await;
    link(&mapper, 160_002, East, 160_003, West).await;
    let section = legacy_map(&mapper, atlas, "Seam Flats", 1600).await;
    add_rooms(&mapper, section, 1600, &[160_101]).await;
    link(&mapper, 160_003, East, 160_101, West).await;
    let placeholder = seed_map(
        &mapper,
        Some(atlas),
        "NukeFire Zone 1601",
        &[(ZONE, "1601"), (MANAGED, MANAGED_BY)],
    )
    .await;
    add_rooms(&mapper, placeholder, 1601, &[160_201, 160_202]).await;
    let glade = legacy_map(&mapper, atlas, "Quiet Glade", 1602).await;
    add_rooms(&mapper, glade, 1602, &[160_301]).await;
    shared_packages::save_param_value(server, MAPPER_SPEC, "debugMappingDecisions", json!(true))
        .expect("turn the decision log on");

    let mut session = start_session(server, 9422, &mapper).await;
    visit(&mut session, &mapper, "Quiet Glade", 160_301, 1602).await;
    session.send("nfmap tidy");
    let done = session
        .wait_until(|lines| {
            lines
                .iter()
                .any(|line| line.starts_with("[nfmap] Done in "))
        })
        .await;
    assert!(done, "the tidy never finished:\n{}", session.transcript());

    let said = |text: &str| session.lines.iter().any(|line| line == text);
    let transcript = session.transcript();
    for line in [
        "[nfmap] Checking 3 zones in 4 maps.",
        "[nfmap] Zone 1600 (Seam Flats): checking.",
        "[nukefire-mapper] Combined 2 map sections into Seam Flats.",
        "[nfmap] Zone 1601: only placeholder maps hold it, so it settles when you visit it.",
        "[nfmap] Zone 1602 (Quiet Glade): settled already this session.",
        "[nfmap] Polishing 3 maps.",
        "[nfmap] NukeFire Zone 1601: nothing to polish.",
        "[nfmap] Quiet Glade: nothing to polish.",
    ] {
        assert!(said(line), "the tidy never said {line:?}:\n{transcript}");
    }
    assert!(
        session
            .lines
            .iter()
            .any(|line| line.starts_with("[nfmap] Polishing Seam Flats: 4 rooms, 6 exits, now ")),
        "the tidy never polished the combined map:\n{transcript}"
    );
    assert!(
        session.lines.iter().any(|line| {
            line.starts_with("[nfmap] Polished Seam Flats in ")
                || line.starts_with("[nfmap] Seam Flats needed nothing")
        }),
        "the polish of the combined map never finished:\n{transcript}"
    );
    assert!(
        session.lines.iter().any(|line| {
            line.starts_with("[nfmap] Done in ")
                && line.contains(": 1 zone combined, ")
                && line.ends_with(" of 1 map improved.")
        }),
        "the tidy's summary differs:\n{transcript}"
    );
    assert!(!exists(&mapper, section), "the section joined Seam Flats");
    assert_eq!(map_of(&mapper, 160_101), row);
    assert!(
        exists(&mapper, placeholder),
        "the placeholder waits for a visit"
    );
    expect_no_failed_edit(&session, server);
    session.shutdown();
}

/// While `nfmap tidy` polishes a map it says every second how the polish is
/// going, the player maps on, and `nfmap stop` ends it, leaving the map with
/// the best layout already on it.
#[tokio::test]
async fn nfmap_stop_ends_a_tidy_while_it_polishes() {
    let server = "NukeFireTidyStop";
    let (mapper, atlas) = nukefire_store(server).await;
    // A grid of rooms whose positions are shuffled, which takes the polish
    // well over a second.
    let grid = legacy_map(&mapper, atlas, "Shuffled Grid", 1700).await;
    seed_shuffled_grid(&mapper, grid, 1700, 170_000, 20).await;
    shared_packages::save_param_value(server, MAPPER_SPEC, "debugMappingDecisions", json!(true))
        .expect("turn the decision log on");

    let mut session = start_session(server, 9423, &mapper).await;
    visit(&mut session, &mapper, "Shuffled Grid", 170_000, 1700).await;
    session.send("nfmap tidy");
    let reporting = session
        .wait_until(|lines| {
            lines
                .iter()
                .any(|line| line.starts_with("[nfmap] Shuffled Grid 0:01 | "))
        })
        .await;
    assert!(
        reporting,
        "the polish never reported its progress:\n{}",
        session.transcript()
    );
    // The search leaves the maps to mapping, which goes on meanwhile.
    visit(&mut session, &mapper, "Far Meadow", 171_001, 1710).await;
    assert_ne!(
        map_of(&mapper, 171_001),
        grid,
        "the meadow has a map of its own"
    );
    session.send("nfmap stop");
    let stopped = session
        .wait_until(|lines| {
            lines
                .iter()
                .any(|line| line.starts_with("[nfmap] Stopped after "))
        })
        .await;
    let transcript = session.transcript();
    assert!(stopped, "the tidy never stopped:\n{transcript}");
    assert!(
        session.lines.iter().any(|line| {
            line == "[nfmap] Stopping; a map being polished keeps the best layout already on it."
        }),
        "nfmap stop never answered:\n{transcript}"
    );
    assert!(
        session
            .lines
            .iter()
            .any(|line| line.starts_with("[nfmap] Stopped polishing Shuffled Grid after ")),
        "the polish of the grid never stopped:\n{transcript}"
    );
    assert!(
        !session
            .lines
            .iter()
            .any(|line| line.starts_with("[nfmap] Done in ")),
        "a stopped tidy does not finish:\n{transcript}"
    );
    session.send("nfmap stop");
    let idle = session
        .wait_until(|lines| {
            lines
                .iter()
                .any(|line| line == "[nfmap] No tidy is running.")
        })
        .await;
    assert!(
        idle,
        "a second stop finds nothing to stop:\n{}",
        session.transcript()
    );
    expect_no_failed_edit(&session, server);
    session.shutdown();
}

/// The package fills the unset "Area name rules" setting with the rules it
/// knows, so the player sees and edits every rule the mapper applies.
#[tokio::test]
async fn the_area_name_rules_setting_starts_with_the_default_rules() {
    let server = "NukeFireDefaultRules";
    install_mapper_packages(server);
    let mapper = local_mapper(&smudgy_home().join("maps").join(server));
    let saved = || {
        shared_packages::get_param_value_for_profile(server, PROFILE, MAPPER_SPEC, "areaNameRules")
    };
    assert_eq!(saved(), None, "the setting starts unset");

    let mut session = start_session(server, 9407, &mapper).await;
    let seeded = session.wait_until(|_| saved().is_some()).await;
    assert!(
        seeded,
        "the rules were never saved:\n{}",
        session.transcript()
    );
    let rule = |name: &str, map: &str| json!({ "name": name, "map": map, "exactCase": false });
    assert_eq!(
        saved(),
        Some(json!([
            rule("Vega Jane II", "Vega Jane"),
            rule("Vega Jane III", "Vega Jane"),
            rule("Vega Jane IV", "Vega Jane"),
            rule("Dread Dungeon 253", "Dread Dungeon"),
            rule("Dread Dungeon 254", "Dread Dungeon"),
            rule("Dread Dungeon 258", "Dread Dungeon"),
            rule("Monster Island II", "Monster Island"),
            rule("Shadowspire II", "Shadowspire"),
            rule("SST - Federation Station", "SST"),
            rule("Jurassic Park II", "Jurassic Park"),
            rule("Jurassic World", "Jurassic Park"),
            { "name": "DARK PLEASURES", "map": "Darker Pleasures", "exactCase": true },
        ]))
    );
    session.shutdown();
}

// === Seeding maps as older versions left them ===

/// A mapper over an empty local store of its own, with the packages installed
/// for `server`, and the local "Nukefire" atlas the mapper files maps in.
async fn nukefire_store(server: &str) -> (Mapper, AtlasId) {
    install_mapper_packages(server);
    let mapper = local_mapper(&smudgy_home().join("maps").join(server));
    let atlas = mapper
        .create_atlas_at("Nukefire".to_string(), MapStorage::Local)
        .await
        .expect("create the Nukefire atlas");
    (mapper, atlas.id)
}

/// An empty map as 0.17.x made it: named after its area, marked as the
/// mapper's and tied to the zone it was made for.
async fn legacy_map(mapper: &Mapper, atlas: AtlasId, name: &str, zone: u32) -> AreaId {
    let zone = zone.to_string();
    seed_map(
        mapper,
        Some(atlas),
        name,
        &[(ZONE, zone.as_str()), (MANAGED, MANAGED_BY)],
    )
    .await
}

/// An empty map as this version makes it: marked as the mapper's and keyed
/// by its folded area name.
async fn settled_map(mapper: &Mapper, atlas: AtlasId, name: &str, key: &str) -> AreaId {
    seed_map(
        mapper,
        Some(atlas),
        name,
        &[(AREA_KEY, key), (MANAGED, MANAGED_BY)],
    )
    .await
}

/// An empty local map with `properties`, in `atlas` or loose.
async fn seed_map(
    mapper: &Mapper,
    atlas: Option<AtlasId>,
    name: &str,
    properties: &[(&str, &str)],
) -> AreaId {
    let destination = atlas.map_or(MapDestination::loose(MapStorage::Local), |atlas| {
        MapDestination::in_atlas(MapStorage::Local, atlas)
    });
    let map = mapper
        .create_area_at(name.to_string(), destination)
        .await
        .expect("create a map");
    let edits: Vec<_> = properties
        .iter()
        .map(|&(name, value)| AreaMutation::UpsertAreaProperty {
            name: name.to_string(),
            value: value.to_string(),
            is_secret: None,
        })
        .collect();
    if !edits.is_empty() {
        apply(mapper, map, edits).await;
    }
    map
}

/// Adds rooms of `zone` to `map` in a row, as the mapper stores them: the
/// game's VNUM as the external id and the zone as a property.
async fn add_rooms(mapper: &Mapper, map: AreaId, zone: u32, vnums: &[u64]) {
    add_stored_rooms(mapper, map, Some(zone), vnums).await;
}

/// Adds rooms to `map` in a row with the game's VNUM as the external id, and
/// with `zone` as a property when given: maps made elsewhere store no zone.
async fn add_stored_rooms(mapper: &Mapper, map: AreaId, zone: Option<u32>, vnums: &[u64]) {
    let first = mapper
        .get_current_atlas()
        .get_area(&map)
        .expect("the map exists")
        .get_rooms()
        .len();
    let mut edits = Vec::new();
    for (index, vnum) in vnums.iter().enumerate() {
        let number = i32::try_from(first + index + 1).expect("a small map");
        let column = i16::try_from(number).expect("a small map");
        edits.push(AreaMutation::UpsertRoom {
            room_number: RoomNumber(number),
            body: RoomUpdates {
                title: Some(format!("Room {vnum}")),
                external_id: Some(Some(vnum.to_string())),
                x: Some(f32::from(column)),
                y: Some(0.0),
                level: Some(0),
                ..RoomUpdates::default()
            },
        });
        if let Some(zone) = zone {
            edits.push(AreaMutation::UpsertRoomProperty {
                room_number: RoomNumber(number),
                name: ZONE.to_string(),
                value: zone.to_string(),
                is_secret: None,
            });
        }
    }
    apply(mapper, map, edits).await;
}

/// Fills `map` with a `side` by `side` grid of rooms of `zone`, VNUMs from
/// `first_vnum` in reading order, every neighbour joined both ways, and each
/// room stored at the cell of a fixed shuffle of the grid instead of its own.
/// Every room is stored before any exit, in mutations the mapper accepts.
async fn seed_shuffled_grid(mapper: &Mapper, map: AreaId, zone: u32, first_vnum: u64, side: u32) {
    let count = side * side;
    let number = |index: u32| RoomNumber(i32::try_from(index + 1).expect("a small map"));
    let coordinate = |value: u32| f32::from(u16::try_from(value).expect("a small map"));
    let mut rooms = Vec::new();
    for index in 0..count {
        // 7 is coprime with the room count, so this moves every room to a
        // cell of its own.
        let cell = (index * 7 + 3) % count;
        let vnum = first_vnum + u64::from(index);
        rooms.push(AreaMutation::UpsertRoom {
            room_number: number(index),
            body: RoomUpdates {
                title: Some(format!("Room {vnum}")),
                external_id: Some(Some(vnum.to_string())),
                x: Some(coordinate(cell % side)),
                y: Some(coordinate(cell / side)),
                level: Some(0),
                ..RoomUpdates::default()
            },
        });
        rooms.push(AreaMutation::UpsertRoomProperty {
            room_number: number(index),
            name: ZONE.to_string(),
            value: zone.to_string(),
            is_secret: None,
        });
    }
    let mut exits = Vec::new();
    for index in 0..count {
        let (x, y) = (index % side, index / side);
        for (dx, dy, direction, back) in [(1, 0, East, West), (0, 1, South, North)] {
            if x + dx >= side || y + dy >= side {
                continue;
            }
            let neighbour = (y + dy) * side + x + dx;
            for (from, to, there, here) in [
                (index, neighbour, direction, back),
                (neighbour, index, back, direction),
            ] {
                exits.push(AreaMutation::CreateExit {
                    room_number: number(from),
                    body: ExitArgs {
                        from_direction: there,
                        to_area_id: Some(map),
                        to_room_number: Some(number(to)),
                        to_direction: Some(here),
                        weight: 1.0,
                        ..ExitArgs::default()
                    },
                });
            }
        }
    }
    for edits in [rooms, exits] {
        for chunk in edits.chunks(256) {
            apply(mapper, map, chunk.to_vec()).await;
        }
    }
}

/// Joins two stored rooms, in one map or two, with a pair of exits: `from`
/// leads `direction` to `to`, which leads `back`.
async fn link(mapper: &Mapper, from: u64, direction: ExitDirection, to: u64, back: ExitDirection) {
    let (from, to) = (room_key(mapper, from), room_key(mapper, to));
    for (at, direction, target, back) in
        [(&from, direction, &to, back), (&to, back, &from, direction)]
    {
        let exit = AreaMutation::CreateExit {
            room_number: at.room_number,
            body: ExitArgs {
                from_direction: direction,
                to_area_id: Some(target.area_id),
                to_room_number: Some(target.room_number),
                to_direction: Some(back),
                weight: 1.0,
                ..ExitArgs::default()
            },
        };
        apply(mapper, at.area_id, vec![exit]).await;
    }
}

async fn add_label(mapper: &Mapper, map: AreaId, text: &str) {
    let label = AreaMutation::CreateLabel {
        body: LabelArgs {
            text: text.to_string(),
            y: -1.0,
            width: 40.0,
            height: 10.0,
            color: "#ffffff".to_string(),
            font_size: 12,
            font_weight: 400,
            ..LabelArgs::default()
        },
    };
    apply(mapper, map, vec![label]).await;
}

async fn apply(mapper: &Mapper, map: AreaId, edits: Vec<AreaMutation>) {
    let submitted = mapper
        .mutate_area(map, edits, "Seed a map as an older version left it")
        .expect("seed a map");
    if let Some(operation_id) = submitted.operation_id() {
        mapper
            .wait_for_mutation(operation_id)
            .await
            .expect("seeded map acknowledged");
    }
}

// === Standing in rooms ===

fn chart_room(vnum: u64, name: &'static str, zone: i64, x: i64) -> ChartRoom<'static> {
    ChartRoom {
        vnum,
        name,
        zone,
        terrain: VISITED_TERRAIN,
        x,
        y: 0,
        z: 0,
    }
}

/// Stands the player in stored room `vnum` of `zone`, in the area the game
/// calls `area`, and waits until the mapper has mapped the room.
async fn visit(session: &mut Session, mapper: &Mapper, area: &str, vnum: u64, zone: i64) {
    let title = format!("Room {vnum}");
    let room = ChartRoom {
        name: &title,
        ..chart_room(vnum, "", zone, 0)
    };
    session.visit(area, &room, &[], &[]);
    let visited = session
        .wait_until(|_| {
            find(mapper, vnum)
                .is_some_and(|(_, terrain)| terrain.as_deref() == Some(VISITED_TERRAIN))
        })
        .await;
    assert!(
        visited,
        "the mapper never mapped room {vnum} of {area}:\n{}",
        session.transcript()
    );
}

/// Waits for the mapper to print `notice`.
async fn expect_notice(session: &mut Session, notice: &str) {
    let said = |lines: &[String]| {
        lines
            .iter()
            .any(|line| line.strip_prefix(NOTICE_PREFIX) == Some(notice))
    };
    let printed = session.wait_until(said).await;
    assert!(
        printed,
        "the mapper never said {notice:?}:\n{}",
        session.transcript()
    );
}

/// Waits for the mapper's decision log to hold a record `matches` accepts.
async fn expect_logged(
    session: &mut Session,
    server: &str,
    what: &str,
    matches: impl Fn(&Value) -> bool,
) {
    let logged = session
        .wait_until(|_| decision_records(server).iter().any(&matches))
        .await;
    assert!(
        logged,
        "the decision log never recorded {what}:\n{}",
        session.transcript()
    );
}

/// The seam round straightened the seams, moving the rooms around them and
/// not the whole map.
fn expect_seam_round(round: &Value) {
    assert_eq!(round["seams"], 4);
    assert_eq!(round["resumed"], false);
    assert!(
        round["passes"].as_u64().is_some_and(|passes| passes >= 1),
        "the round ran: {round}"
    );
    assert!(
        round["region"]
            .as_u64()
            .is_some_and(|region| (4..8).contains(&region)),
        "the region holds the seams and their neighbours, not the whole map: {round}"
    );
    let violations = |quality: &Value| quality["cardinalRayViolations"].as_u64().unwrap();
    assert!(
        violations(&round["after"]) < violations(&round["before"]),
        "the round straightened the seam: {round}"
    );
}

/// Where a `layout-decision` record's request placed stored room `vnum`.
fn planned_cell(decision: &Value, mapper: &Mapper, vnum: u64) -> (i64, i64) {
    let id = format!("room:{}", room_key(mapper, vnum).room_number.0);
    let resident = decision["request"]["residents"]
        .as_array()
        .expect("the request's residents")
        .iter()
        .find(|resident| resident["id"] == id.as_str())
        .unwrap_or_else(|| panic!("room {vnum} is in the request"));
    let at = &resident["position"];
    (at["x"].as_i64().unwrap(), at["y"].as_i64().unwrap())
}

/// The complete records in the mapper's decision log so far.
fn decision_records(server: &str) -> Vec<Value> {
    let Some(path) = find_file(&smudgy_home().join(server), "mapping-decisions.jsonl") else {
        return Vec::new();
    };
    std::fs::read(path)
        .unwrap_or_default()
        .split_inclusive(|byte| *byte == b'\n')
        // The asynchronous writer may still be appending the last record.
        .filter(|line| line.ends_with(b"\n"))
        .map(|line| serde_json::from_slice(line).expect("a valid decision record"))
        .collect()
}

/// The mapper printed `expected` and nothing else: no other notice, and no
/// error. Where its decision log is written, when it is on, does not count.
fn expect_only_notices(session: &Session, expected: &[&str]) {
    let notices: Vec<_> = session
        .notices()
        .into_iter()
        .filter(|notice| !notice.starts_with("mapping decisions: "))
        .collect();
    assert_eq!(
        notices,
        expected,
        "the mapper's lines differ:\n{}",
        session.transcript()
    );
}

// === Reading the maps ===

/// Where stored room `vnum` is, and the terrain recorded on it.
fn find(mapper: &Mapper, vnum: u64) -> Option<(RoomKey, Option<String>)> {
    let (key, room) = mapper
        .get_current_atlas()
        .find_room_by_external_id(&vnum.to_string())?;
    let terrain = room.get_property("terrain").map(str::to_string);
    Some((key, terrain))
}

fn room_key(mapper: &Mapper, vnum: u64) -> RoomKey {
    find(mapper, vnum)
        .unwrap_or_else(|| panic!("room {vnum} is mapped"))
        .0
}

fn map_of(mapper: &Mapper, vnum: u64) -> AreaId {
    room_key(mapper, vnum).area_id
}

fn exists(mapper: &Mapper, map: AreaId) -> bool {
    mapper.get_current_atlas().get_area(&map).is_some()
}

fn name(mapper: &Mapper, map: AreaId) -> String {
    let area = mapper
        .get_current_atlas()
        .get_area(&map)
        .expect("the map exists");
    area.get_name().to_string()
}

fn property(mapper: &Mapper, map: AreaId, name: &str) -> Option<String> {
    let area = mapper.get_current_atlas().get_area(&map)?;
    area.get_property(name).map(str::to_string)
}

/// The level stored room `vnum` is on.
fn level(mapper: &Mapper, vnum: u64) -> i32 {
    let key = room_key(mapper, vnum);
    mapper
        .get_current_atlas()
        .get_room(&key)
        .unwrap_or_else(|| panic!("room {vnum} is mapped"))
        .get_level()
}

/// The Connections between stored rooms `a` and `b`, which share a map.
fn connections_between(mapper: &Mapper, a: u64, b: u64) -> Vec<ConnectionId> {
    let (Some((a, _)), Some((b, _))) = (find(mapper, a), find(mapper, b)) else {
        return Vec::new();
    };
    let Some(area) = mapper.get_current_atlas().get_area(&a.area_id) else {
        return Vec::new();
    };
    area.get_connections()
        .iter()
        .filter(|connection| {
            connection.endpoint_b.is_some_and(|endpoint_b| {
                let ends = [connection.endpoint_a.room_number, endpoint_b.room_number];
                ends.contains(&a.room_number) && ends.contains(&b.room_number)
            })
        })
        .map(|connection| connection.id)
        .collect()
}

/// A Connection's kind and routing.
fn connection(
    mapper: &Mapper,
    map: AreaId,
    id: ConnectionId,
) -> (ConnectionKind, ConnectionRouting) {
    let area = mapper
        .get_current_atlas()
        .get_area(&map)
        .expect("the map exists");
    let connection = area.get_connection(id).expect("the Connection exists");
    (connection.kind, connection.routing)
}

/// No edit the mapper submitted failed.
fn expect_no_failed_edit(session: &Session, server: &str) {
    let failed: Vec<_> = decision_records(server)
        .into_iter()
        .filter(|record| record["kind"] == "mutation-error")
        .collect();
    assert!(
        failed.is_empty(),
        "edits failed: {failed:#?}\n{}",
        session.transcript()
    );
}

fn rooms_in(mapper: &Mapper, map: AreaId) -> usize {
    let area = mapper
        .get_current_atlas()
        .get_area(&map)
        .expect("the map exists");
    area.get_rooms().len()
}

fn labels(mapper: &Mapper, map: AreaId) -> Vec<String> {
    let area = mapper
        .get_current_atlas()
        .get_area(&map)
        .expect("the map exists");
    area.get_labels()
        .iter()
        .map(|label| label.text.clone())
        .collect()
}

fn map_count(mapper: &Mapper) -> usize {
    mapper.get_current_atlas().areas().count()
}

fn room_count(mapper: &Mapper) -> usize {
    let atlas = mapper.get_current_atlas();
    atlas.areas().map(|area| area.get_rooms().len()).sum()
}

fn maps_named(mapper: &Mapper, name: &str) -> Vec<AreaId> {
    let atlas = mapper.get_current_atlas();
    atlas
        .areas()
        .filter(|area| area.get_name() == name)
        .map(|area| *area.get_id())
        .collect()
}

/// The maps holding rooms of `zone`, each once.
fn maps_holding_zone(mapper: &Mapper, zone: u32) -> Vec<AreaId> {
    let mut maps = Vec::new();
    for (map, _) in mapper
        .get_current_atlas()
        .get_rooms_by_property(ZONE, &zone.to_string())
    {
        if !maps.contains(&map) {
            maps.push(map);
        }
    }
    maps
}

/// The cell stored room `vnum` lies in: its column, row and level.
fn cell(mapper: &Mapper, vnum: u64) -> (i64, i64, i32) {
    let atlas = mapper.get_current_atlas();
    let (_, room) = atlas
        .find_room_by_external_id(&vnum.to_string())
        .unwrap_or_else(|| panic!("room {vnum} is mapped"));
    #[allow(clippy::cast_possible_truncation)]
    let (x, y) = (room.get_x().round() as i64, room.get_y().round() as i64);
    (x, y, room.get_level())
}

/// The cell south of `cell`, on its level.
fn below((x, y, level): (i64, i64, i32)) -> (i64, i64, i32) {
    (x, y + 1, level)
}

/// The room the exit of stored room `vnum` in `direction` leads to.
fn exit_target(mapper: &Mapper, vnum: u64, direction: ExitDirection) -> Option<RoomKey> {
    let atlas = mapper.get_current_atlas();
    let (_, room) = atlas.find_room_by_external_id(&vnum.to_string())?;
    let exit = room
        .get_exits()
        .iter()
        .find(|exit| exit.from_direction == direction)?;
    Some(RoomKey::new(exit.to_area_id?, exit.to_room_number?))
}
