//! End-to-end coverage for a map's places from a script, through V8, against a
//! cloud map with two Secrets. "Bookcase" (writable) keeps notes, a tag and a
//! hidden door on map room 1, owns room 2 behind that door (which leads on to
//! map room 3, otherwise unreachable), tags map room 3 `LOOT` and keeps an
//! area property; "Ledger" (read only) keeps its own notes for room 1.
//!
//! A trusted script lists the places, reads and writes them through views,
//! reads them side by side and together, searches across them (the nearest
//! search walks through the hidden door), follows exits into a Secret's room,
//! and creates, updates and deletes a Secret, each in one request. Installed
//! packages see the map alone without the `secrets` capability, and the write
//! and manage gates hold with read alone.

use std::{
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use smudgy_cloud::{
    AREA_FORMAT_VERSION, Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, CloudError,
    CloudResult, CreateAreaRequest, Mapper, MapperBackend, Property, RoomData, RoomNumber,
    RoomWithDetails, SourceBundle, SourceId, Uuid,
    cloud_api::{SecretChange, SecretColorChange, SecretSummary},
    mutation::{AreaMutation, MutationEnvelope, MutationResult, ResourceKind, VersionInfo},
};
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams, spawn,
    spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage, SmudgyCapabilities,
};

const MAP: Uuid = Uuid::from_u128(0x5ec1_0000_0000_4000_8000_0000_0000_0001);
const BOOKCASE: Uuid = Uuid::from_u128(0x5ec1_0000_0000_4000_8000_0000_0000_0002);
const LEDGER: Uuid = Uuid::from_u128(0x5ec1_0000_0000_4000_8000_0000_0000_0003);
const QUEST: Uuid = Uuid::from_u128(0x5ec1_0000_0000_4000_8000_0000_0000_0004);
const ROAD: Uuid = Uuid::from_u128(0x5ec1_0000_0000_4000_8000_0000_0000_0005);
const CLAN: Uuid = Uuid::from_u128(77);

/// The checking helpers every script below shares.
fn prelude() -> String {
    format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";

const failures = [];
const check = (label, actual, expected) => {{
    if (actual !== expected) failures.push(`${{label}}: got [${{actual}}] want [${{expected}}]`);
}};
const failure = async (run) => {{
    try {{ await run(); return "no error"; }} catch (error) {{ return `${{error?.constructor?.name}}: ${{error?.message ?? error}}`; }}
}};
const name = (place) => (typeof place === "string" ? place : place.name);
const where = (room) => `${{room.areaId === "{MAP}" ? "map" : name(room.place)}}:${{room.roomNumber}}`;
const found = (rooms) => rooms.filter(Boolean).map(where).sort().join(",");
const NOT_CAPABLE = (capability) =>
    `Error: smudgy: this package did not request the '${{capability}}' capability`;

await mapper.ready();
const map = mapper.getAreaById("{MAP}");
const room = map.room(1);
"#
    )
}

#[allow(clippy::too_many_lines)] // one script, read top to bottom
fn trusted_module() -> String {
    format!(
        r##"{prelude}
try {{
// Places: where things live.
check("map place", map.place, "map");
check("room place", room.place, "map");
const places = map.places;
check("places", places.map(name).join(","), "map,Bookcase,Ledger,private");
const [, bookcase, ledger] = places;
check("one handle per Secret", map.secrets.get(bookcase.id) === bookcase, true);
check("secret mapId", bookcase.mapId, map.id);
check("secret ownership", bookcase.ownership, "owner");
check("secret color", bookcase.color, "#8a5cf6");
check("secret actions", bookcase.actions.join(","), "add,edit,read,remove");
check("ledger clan", ledger.clanId, "{CLAN}");
check("ledger actions", ledger.actions.join(","), "manageAccess,read");
const vault = bookcase.area;
check("secret area", vault.id, bookcase.id);
check("secret area mapId", vault.mapId, map.id);
check("secret area place", vault.place === bookcase, true);
check("secret area places", vault.places.length === 1 && vault.places[0] === bookcase, true);
check("secret room place", vault.room(2).place === bookcase, true);
check("secret room area", vault.room(2).areaId, bookcase.id);

// Views: one place at a time, never merged.
check("own notes", room.data("notes"), "Ordinary");
check("map view", room.in("map").data("notes"), "Ordinary");
check("by handle", room.in(bookcase).data("notes"), "Behind the bookcase");
check("by id", room.in(bookcase.id).data("notes"), "Behind the bookcase");
check("view place", room.in(bookcase.id).place === bookcase, true);
check("ledger view", room.in(ledger).data("notes"), "Owed 3 gold");
check("absent in place", room.in(bookcase).data("title"), undefined);
check("private empty", room.in("private").data("notes"), undefined);
check("view tags", room.in(bookcase).tags.join(","), "DOOR");
check("view hasTag", room.in(bookcase).hasTag("door"), true);
check("map view tags", room.in("map").tags.join(","), "START");
check("view exits", room.in(bookcase).exits.length, 1);
check("map view exits", room.in("map").exits.map((exit) => exit.toRoomNumber).join(","), "4");
check("area view", map.in(bookcase).data("summary"), "Hidden room");
check("map area data", map.data("summary"), undefined);
check("own room, own place", vault.room(2).in(bookcase).data("notes"), "Vault notes");
check("own room, empty other place", vault.room(2).in("map").data("notes"), undefined);
check("unknown secret", await failure(() => room.in("00000000-0000-4000-8000-00000000dead")), "Error: Secret not found");
check("not a place", (await failure(() => room.in("effective"))).includes("is not a place"), true);

// Side by side, and together.
check(
    "combinedData",
    JSON.stringify(room.combinedData("notes").map((entry) => [name(entry.source), entry.data])),
    JSON.stringify([["map", "Ordinary"], ["Bookcase", "Behind the bookcase"], ["Ledger", "Owed 3 gold"]]),
);
check("combinedData handle", room.combinedData("notes")[1].source === bookcase, true);
check(
    "combinedData all",
    room.combinedData().map((entry) => `${{name(entry.source)}}:${{entry.key}}`).join(","),
    "map:notes,Bookcase:notes,Ledger:notes",
);
check(
    "combinedTags",
    room.combinedTags().map((entry) => `${{name(entry.source)}}:${{entry.tag}}`).join(","),
    "map:START,Bookcase:DOOR",
);
check("tags union", room.tags.join(","), "DOOR,START");
check("hasTag any place", room.hasTag("door"), true);
check("data stays own", room.data("notes"), "Ordinary");

// Exits across places.
const door = room.exits.find((exit) => exit.place !== "map");
check("door place", door?.place === bookcase, true);
check("door toRoom", door?.toRoom === undefined ? "none" : where(door.toRoom), "Bookcase:2");
check("vault exit toRoom", where(vault.room(2).exits[0].toRoom), "map:3");
check("vault exit place", vault.room(2).exits[0].place === bookcase, true);
const denDoor = map.room(3).exits.find((exit) => exit.place === "private");
check("den door", denDoor?.toAreaId, "{private}");
check("den", denDoor?.toRoom?.title, "Den");
check("den place", denDoor?.toRoom?.place, "private");

// Searches cover every place, or one.
check("search door", found(mapper.findRoomsWithTag("door")), "map:1");
check("search vault", found(mapper.findRoomsWithTag("vault")), "Bookcase:2");
check("search loot", found(mapper.findRoomsWithTag("loot")), "map:3");
check("search notes", found(mapper.findRoomsWithProperty("notes")), "Bookcase:2,map:1");
check("search value", found(mapper.findRoomsByProperty("notes", "Owed 3 gold")), "map:1");
check("narrowed to map", found(mapper.findRoomsWithTag("door", {{ in: "map" }})), "");
check("narrowed to bookcase", found(mapper.findRoomsWithTag("door", {{ in: bookcase }})), "map:1");
check("narrowed by id", found(mapper.findRoomsWithTag("vault", {{ in: bookcase.id }})), "Bookcase:2");
check("area search", found(map.findRoomsWithTag("vault")), "Bookcase:2");
check("area view search", found(map.in(ledger).findRoomsWithProperty("notes")), "map:1");
check("nearest through the door", found([mapper.findNearestRoomWithTag(room, "vault")]), "Bookcase:2");
check("nearest secret tag on a map room", found([mapper.findNearestRoomWithTag(room, "loot")]), "map:3");
check("nearest in the map alone", mapper.findNearestRoomWithTag(room, "loot", {{ in: "map" }}), undefined);
check("nearest across places", found([mapper.findNearestRoomWithTags(room, {{ all: ["start", "door"] }})]), "map:1");

// Writing through views lands in exactly that place.
await room.in(bookcase).setData("notes", "Quest note");
check("written", room.in(bookcase).data("notes"), "Quest note");
check("map untouched", map.room(1).data("notes"), "Ordinary");
check("existing tag", await room.in(bookcase).addTag("door"), null);
await room.in(bookcase.id).addTag("quest");
check("tag written", room.in(bookcase).tags.join(","), "DOOR,QUEST");
await room.in("private").setData("notes", "Mine");
check("private written", room.in("private").data("notes"), "Mine");
await mapper.mutateArea(map, async (m) => {{
    await m.setRoomProperty(1, "kws", "lever");
    await m.setAreaProperty("summary", "Two rooms");
}}, {{ in: bookcase, description: "Bookcase notes" }});
check("mutate property", room.in(bookcase).data("kws"), "lever");
check("mutate area property", map.in(bookcase).data("summary"), "Two rooms");
const trapdoor = await room.in(bookcase).createExit({{
    fromDirection: "Down", toAreaId: map.id, toRoomNumber: 3, isHidden: true,
}});
check("hidden door listed", map.room(1).exits.some((exit) => exit.id === trapdoor), true);
await room.in(bookcase).deleteData("kws");
check("deleted in place", room.in(bookcase).data("kws"), undefined);
await room.in(bookcase).removeTag("quest");
check("tag removed", room.in(bookcase).tags.join(","), "DOOR");
await map.in(bookcase).deleteData("summary");
check("area property deleted", map.in(bookcase).data("summary"), undefined);
check("map kept its notes", map.room(1).data("notes"), "Ordinary");
check(
    "room fields refused",
    (await failure(() => mapper.mutateArea(map, (m) => m.setRoomTitle(1, "Nope"), {{ in: bookcase }}))).startsWith("TypeError"),
    true,
);
check(
    "view only refused",
    (await failure(() => room.in(ledger).setData("notes", "Nope"))).includes("secretViewOnly; formerly secret_view_only"),
    true,
);
check(
    "unknown secret write",
    await failure(() => mapper.mutateArea(map, (m) => m.setAreaProperty("a", "b"), {{ in: "00000000-0000-4000-8000-00000000dead" }})),
    "Error: Secret not found",
);

// The registry, and one request per create and per update.
check("list", map.secrets.list().map(name).join(","), "Bookcase,Ledger");
check("exists", map.secrets.exists(bookcase.id), true);
check("exists unknown", map.secrets.exists("00000000-0000-4000-8000-00000000dead"), false);
check("exists junk", map.secrets.exists("junk"), false);
check("get unknown", await failure(() => map.secrets.get("00000000-0000-4000-8000-00000000dead")), "Error: Secret not found");
check("by stored id", mapper.getSecretById(ledger.id) === ledger, true);
check("none on a secret's area", vault.secrets.list().length, 0);
const quest = await map.secrets.create({{ name: "Quest", color: "#ABCDEF" }});
check("created", `${{quest.name}} ${{quest.color}}`, "Quest #abcdef");
check("created on map", quest.mapId, map.id);
check("created listed", map.secrets.list().map(name).join(","), "Bookcase,Ledger,Quest");

// Each place numbers its own rooms, from 1.
check("map next", map.nextRoomNumber, 6);
check("secret next", vault.nextRoomNumber, 3);
check("ledger next", ledger.area.nextRoomNumber, 1);
check("private next", mapper.getAreaById("{private}").nextRoomNumber, 8);
check("new secret next", quest.area.nextRoomNumber, 1);
check("new secret's first room", await mapper.createRoom(quest.id, {{ title: "Crypt" }}), 1);
check("new secret's first room is its own", where(quest.area.room(1)), "Quest:1");
check("new secret then", quest.area.nextRoomNumber, 2);
check("secret's next room", await mapper.createRoom(vault.id, {{ title: "Alcove" }}), 3);
check("map next unmoved", mapper.getAreaById(map.id).nextRoomNumber, 6);
await quest.update({{ name: "Phylactery", color: "#123456" }});
check("updated", `${{quest.name}} ${{quest.color}}`, "Phylactery #123456");
await quest.update({{ color: null }});
check("uncolored", `${{quest.name}} ${{quest.color}}`, "Phylactery null");
check("empty update refused", (await failure(() => quest.update({{}}))).startsWith("TypeError"), true);
await quest.delete();
check("deleted", await failure(() => mapper.getSecretById(quest.id)), "Error: Secret not found");
check("deleted unlisted", map.secrets.list().length, 2);

// A Clan Secret names its owner; the shape is checked before any request.
const CLAN = "11111111-1111-4111-8111-111111111111";
check("unknown ownership", (await failure(() => map.secrets.create({{ name: "X", ownership: "guild" }}))).startsWith("TypeError"), true);
check("owner takes no clan", (await failure(() => map.secrets.create({{ name: "X", clanId: CLAN }}))).startsWith("TypeError"), true);
check("clan id is a string", (await failure(() => map.secrets.create({{ name: "X", ownership: "clan", clanId: 7 }}))).startsWith("TypeError"), true);
const survey = await map.secrets.create({{ name: "Survey", ownership: "clan", clanId: CLAN }});
check("clan created", `${{survey.ownership}} ${{survey.clanId}}`, `clan ${{CLAN}}`);
await survey.delete();
}} catch (error) {{
    failures.push(`threw ${{error?.stack ?? error}}`);
}}

echo(failures.length === 0 ? "SCRIPT_PLACES_OK" : `SCRIPT_PLACES_FAIL ${{failures.join(" | ")}}`);
"##,
        prelude = prelude(),
        private = private_area()
    )
}

/// What an installed package without the `secrets` capability sees: the map's
/// place alone, and nothing of a Secret anywhere.
fn uncapable_module() -> String {
    format!(
        r#"{prelude}
try {{
check("places", map.places.join(","), "map");
check("place", room.place, "map");
check("secrets", map.secrets.list().length, 0);
check("exists", map.secrets.exists("{BOOKCASE}"), false);
check("tags", room.tags.join(","), "START");
check("hasTag", room.hasTag("door"), false);
check("combinedData", room.combinedData("notes").map((entry) => name(entry.source)).join(","), "map");
check("combinedTags", room.combinedTags().map((entry) => entry.tag).join(","), "START");
check("search", found(mapper.findRoomsWithTag("door")), "");
check("search own room", found(mapper.findRoomsWithTag("vault")), "");
check("area search", found(map.findRoomsWithTag("vault")), "");
check("nearest", mapper.findNearestRoomWithTag(room, "loot"), undefined);
check("map view", room.in("map").data("notes"), "Ordinary");
check("exits", room.exits.map((exit) => `${{name(exit.place)}}>${{exit.toRoomNumber}}`).join(","), "map>4");
check("route", mapper.getPathBetweenRooms(map.id, 1, map.id, 3).length, 4);
for (const place of ["private", "{BOOKCASE}", "00000000-0000-4000-8000-00000000beef", "junk"]) {{
    check(`view ${{place}}`, await failure(() => room.in(place)), NOT_CAPABLE("secrets-read"));
    check(`area view ${{place}}`, await failure(() => map.in(place)), NOT_CAPABLE("secrets-read"));
    check(`narrowed ${{place}}`, await failure(() => mapper.findRoomsWithTag("door", {{ in: place }})), NOT_CAPABLE("secrets-read"));
}}
check("by stored id", await failure(() => mapper.getSecretById("{BOOKCASE}")), NOT_CAPABLE("secrets-read"));
check("get", await failure(() => map.secrets.get("{BOOKCASE}")), NOT_CAPABLE("secrets-read"));
check(
    "batch",
    await failure(() => mapper.mutateArea(map, (m) => m.setAreaProperty("a", "b"), {{ in: "private" }})),
    NOT_CAPABLE("secrets-write"),
);
check("secret's area", await failure(() => mapper.getAreaById("{BOOKCASE}")), "Error: Area not found");
}} catch (error) {{
    failures.push(`threw ${{error?.stack ?? error}}`);
}}

echo(failures.length === 0 ? "PLACES_UNCAPABLE_OK" : `PLACES_UNCAPABLE_FAIL ${{failures.join(" | ")}}`);
"#,
        prelude = prelude()
    )
}

/// Everything a package without the `secrets` capability can ask about a map
/// and a Secret's id, one `PROBE` line each, writes naming the Secret's own
/// area last. Run against the map with its Secrets and without them, the lines
/// must match, room numbers too: each place numbers its own rooms, so the
/// numbers the map hands out never pass over its Secrets' and Private
/// additions'.
#[allow(clippy::too_many_lines)] // one probe, read top to bottom
fn probe_module() -> String {
    format!(
        r##"
import {{ echo, mapper }} from "smudgy:core";

const S = "{BOOKCASE}";
const P = "{private}";
const U = "00000000-0000-4000-8000-00000000beef";
const where = (room) => (room ? `${{room.areaId}}:${{room.roomNumber}}` : "none");
const probe = async (label, run) => {{
    let seen;
    try {{
        seen = JSON.stringify(await run()) ?? "undefined";
    }} catch (error) {{
        seen = `!! ${{error?.constructor?.name}}: ${{error?.message ?? error}}`;
    }}
    echo(`PROBE ${{label}}: ${{seen}}`);
}};

await mapper.ready();
const map = mapper.getAreaById("{MAP}");
const room = map.room(1);
const exitSummary = (exit) => ({{
    id: exit.id, to: [exit.toAreaId, exit.toRoomNumber], hidden: exit.isHidden,
    place: exit.place, toRoom: where(exit.toRoom),
}});

await probe("areas", () => mapper.areas.map((area) => area.id));
await probe("getAreaById secret", () => mapper.getAreaById(S).id);
await probe("getAreaById unknown", () => mapper.getAreaById(U).id);
await probe("places", () => map.places);
await probe("room place", () => room.place);
await probe("secrets", () => map.secrets.list().map((secret) => secret.id));
await probe("exists secret", () => map.secrets.exists(S));
await probe("exists unknown", () => map.secrets.exists(U));
await probe("exists junk", () => map.secrets.exists("junk"));
await probe("get secret", () => map.secrets.get(S).id);
await probe("getSecretById secret", () => mapper.getSecretById(S).id);
await probe("getSecretById unknown", () => mapper.getSecretById(U).id);
await probe("in secret", () => room.in(S));
await probe("in unknown", () => room.in(U));
await probe("in private", () => room.in("private"));
await probe("area in secret", () => map.in(S));
for (const number of [1, 3, 4, 5]) {{
    const here = map.room(number);
    await probe(`room ${{number}} exits`, () => here.exits.map(exitSummary));
    await probe(`room ${{number}} tags`, () => here.tags);
    await probe(`room ${{number}} hasTag`, () => ["door", "loot", "start"].map((tag) => here.hasTag(tag)));
    await probe(`room ${{number}} combinedData`, () => here.combinedData());
    await probe(`room ${{number}} combinedTags`, () => here.combinedTags());
}}
for (const tag of ["vault", "loot", "door", "start", "den"]) {{
    await probe(`findRoomsWithTag ${{tag}}`, () => mapper.findRoomsWithTag(tag).map(where));
    await probe(`area findRoomsWithTag ${{tag}}`, () => map.findRoomsWithTag(tag).map(where));
    await probe(`nearest ${{tag}}`, () => where(mapper.findNearestRoomWithTag(room, tag)));
}}
await probe("nearest none start", () => where(mapper.findNearestRoomWithTags(room, {{ none: ["start"] }})));
await probe("findRoomsWithProperty notes", () => mapper.findRoomsWithProperty("notes").map(where));
await probe("findRoomsByProperty vault notes", () => mapper.findRoomsByProperty("notes", "Vault notes").map(where));
await probe("title Vault", () => mapper.listRoomsByTitleAndDescription("Vault", "").map(where));
await probe("title Library", () => mapper.listRoomsByTitleAndDescription("Library", "").map(where));
await probe("visible exits Vault", () => mapper.listRoomsByTitleDescriptionAndVisibleExits("Vault", "", []).map(where));
await probe("external v-2", () => where(mapper.findRoomByExternalId("v-2")));
await probe("external m-4", () => where(mapper.findRoomByExternalId("m-4")));
await probe("rescue v-2", () => mapper.rescueRoomByExternalId("v-2"));
await probe("export map", async () => {{
    const exported = await mapper.exportArea(map);
    return {{
        token: exported.projection_token ?? null,
        sources: exported.sources ?? [],
        rooms: exported.rooms.map((room) => room.room_number),
    }};
}});
await probe("route map 1 to map 3", () => mapper.getPathBetweenRooms(map.id, 1, map.id, 3));
await probe("route map 1 to secret 2", () => mapper.getPathBetweenRooms(map.id, 1, S, 2));
await probe("route secret 2 to map 3", () => mapper.getPathBetweenRooms(S, 2, map.id, 3));
await probe("route secret 2 to itself", () => mapper.getPathBetweenRooms(S, 2, S, 2));
await probe("route unknown 2 to itself", () => mapper.getPathBetweenRooms(U, 2, U, 2));
await probe("route map 3 to the den", () => mapper.getPathBetweenRooms(map.id, 3, P, 7));
await probe("route the den to itself", () => mapper.getPathBetweenRooms(P, 7, P, 7));
await probe("getAreaById private", () => mapper.getAreaById(P).id);
await probe("external p-7", () => where(mapper.findRoomByExternalId("p-7")));
await probe("title Den", () => mapper.listRoomsByTitleAndDescription("Den", "").map(where));
await probe("setRoomTitle private", () => mapper.setRoomTitle(P, 7, "x"));
await probe("nearest in secret", () => where(mapper.findNearestRoomInArea(room, S)));
await probe("nearest in map", () => where(mapper.findNearestRoomInArea(room, map.id)));
await probe("nearest from secret", () => where(mapper.findNearestRoomInArea({{ areaId: S, roomNumber: 2 }}, map.id)));
await probe("nearest from secret into secret", () => where(mapper.findNearestRoomInArea({{ areaId: S, roomNumber: 2 }}, S)));
await probe("set location", () => mapper.setCurrentLocation(S, 2));
await probe("read location", () => mapper.getCurrentLocation());

// Writes naming the Secret's own area, as writes naming an unknown one.
const EXIT = "00000000-0000-4000-8000-000000000121";
const CONN = "00000000-0000-4000-8000-000000000221";
const LABEL = "00000000-0000-4000-8000-0000000001ab";
const endpoint = {{ roomNumber: 2, side: "North", portOffset: 0.5, portMode: "AutoPinned" }};
await probe("setRoomTitle", () => mapper.setRoomTitle(S, 2, "x"));
await probe("setRoomDescription", () => mapper.setRoomDescription(S, 2, "x"));
await probe("setRoomColor", () => mapper.setRoomColor(S, 2, "#ffffff"));
await probe("setRoomLevel", () => mapper.setRoomLevel(S, 2, 1));
await probe("setRoomX", () => mapper.setRoomX(S, 2, 1));
await probe("setRoomY", () => mapper.setRoomY(S, 2, 1));
await probe("setRoomExternalId", () => mapper.setRoomExternalId(S, 2, "x"));
await probe("setRoomProperty", () => mapper.setRoomProperty(S, 2, "k", "v"));
await probe("setAreaProperty", () => mapper.setAreaProperty(S, "k", "v"));
await probe("deleteRoomProperty", () => mapper.deleteRoomProperty(S, 2, "notes"));
await probe("deleteAreaProperty", () => mapper.deleteAreaProperty(S, "summary"));
await probe("addRoomTag", () => mapper.addRoomTag(S, 2, "t"));
await probe("removeRoomTag", () => mapper.removeRoomTag(S, 2, "vault"));
await probe("createRoom", () => mapper.createRoom(S, {{ title: "x" }}));
await probe("updateRoom", () => mapper.updateRoom(S, 2, {{ title: "x" }}));
await probe("updateRooms", () => mapper.updateRooms(S, [[2, {{ title: "x" }}]]));
await probe("createRoomExit", () => mapper.createRoomExit(S, 2, {{ fromDirection: "North" }}));
await probe("setRoomExit", () => mapper.setRoomExit(S, 2, EXIT, {{ weight: 2 }}));
await probe("mergeRooms", () => mapper.mergeRooms(S, 2, 3));
await probe("createLink", () => mapper.createLink(S, {{ endpointA: endpoint, traversals: [] }}));
await probe("setConnection", () => mapper.setConnection(S, CONN, {{ dash: "Dashed" }}));
await probe("pairConnections", () => mapper.pairConnections(S, CONN, CONN));
await probe("unlinkRoomExit", () => mapper.unlinkRoomExit(S, EXIT));
await probe("createLabel", () => mapper.createLabel(S, {{ x: 0, y: 0, width: 1, height: 1, text: "t" }}));
await probe("createShape", () => mapper.createShape(S, {{ x: 0, y: 0, width: 1, height: 1 }}));
await probe("setLabel", () => mapper.setLabel(S, LABEL, {{ text: "x" }}));
await probe("setShape", () => mapper.setShape(S, LABEL, {{ x: 1 }}));
await probe("deleteLabel", () => mapper.deleteLabel(S, LABEL));
await probe("deleteShape", () => mapper.deleteShape(S, LABEL));
await probe("exportArea", () => mapper.exportArea(S));
await probe("mutateArea", () => mapper.mutateArea(S, (m) => m.setAreaProperty("a", "b")));
await probe("mergeAreas", () => mapper.mergeAreas(map, [S]));
await probe("copyArea", () => mapper.copyArea(S, {{ storage: "local" }}));
await probe("renameArea", () => mapper.renameArea(S, "x"));
await probe("deleteRoomExit", () => mapper.deleteRoomExit(S, 2, EXIT));
await probe("deleteLink", () => mapper.deleteLink(S, CONN));
await probe("deleteRoom", () => mapper.deleteRoom(S, 2));
await probe("deleteArea", () => mapper.deleteArea(S));

// The numbers new map rooms take, one at a time and in a batch.
const live = () => mapper.getAreaById(map.id);
await probe("nextRoomNumber", () => map.nextRoomNumber);
await probe("createRoom on the map", () => mapper.createRoom(map.id, {{ title: "Annex" }}));
await probe("nextRoomNumber after createRoom", () => live().nextRoomNumber);
await probe("mutateArea createRoom on the map", async () => {{
    const created = [];
    await mapper.mutateArea(map, async (m) => {{
        created.push(await m.createRoom({{ title: "Wing" }}));
        created.push(await m.createRoom({{ title: "Tower" }}));
    }});
    return created;
}});
await probe("nextRoomNumber after mutateArea", () => live().nextRoomNumber);
await probe("map rooms after creating", () => live().roomNumbers);
await probe("after", () => mapper.areas.map((area) => area.id));

echo("PROBE_DONE");
"##,
        private = private_area()
    )
}

/// The area the caller's Private additions on the fixture map read under.
fn private_area() -> AreaId {
    smudgy_cloud::mapper::area_cache::source_area_id(AreaId(MAP), SourceId::Private, None)
        .expect("Private additions have an area")
}

/// What an installed package with `secrets: ["read"]` alone sees: every place,
/// but no writes to them and no Secret lifecycle.
fn reader_module() -> String {
    format!(
        r#"{prelude}
try {{
check("places", map.places.map(name).join(","), "map,Bookcase,Ledger,private");
check("tags", room.tags.join(","), "DOOR,START");
check("view", room.in("{BOOKCASE}").data("notes"), "Behind the bookcase");
check(
    "write",
    await failure(() => room.in("{BOOKCASE}").setData("notes", "Nope")),
    NOT_CAPABLE("secrets-write"),
);
check("create", await failure(() => map.secrets.create({{ name: "Nope" }})), NOT_CAPABLE("secrets-manage"));
check("update", await failure(() => map.secrets.get("{BOOKCASE}").update({{ name: "Nope" }})), NOT_CAPABLE("secrets-manage"));
check("delete", await failure(() => map.secrets.get("{BOOKCASE}").delete()), NOT_CAPABLE("secrets-manage"));
}} catch (error) {{
    failures.push(`threw ${{error?.stack ?? error}}`);
}}

echo(failures.length === 0 ? "PLACES_READER_OK" : `PLACES_READER_FAIL ${{failures.join(" | ")}}`);
"#,
        prelude = prelude()
    )
}

/// Every write through a Secret's own area, as an installed package with mapper write makes
/// them. With `secrets` read alone each is refused for want of `secrets-write`;
/// a Map-owned exit into the Secret requires Read only and stays Map-owned.
#[allow(clippy::too_many_lines)] // one script, read top to bottom
fn write_tier_module(refused: bool) -> String {
    format!(
        r##"{prelude}
const S = "{BOOKCASE}";
const EXIT = "00000000-0000-4000-8000-000000000121";
const CONN = "00000000-0000-4000-8000-000000000221";
const LABEL = "00000000-0000-4000-8000-0000000001ab";
const endpoint = {{ roomNumber: 2, side: "North", portOffset: 0.5, portMode: "AutoPinned" }};
const REFUSED = {refused};
const writes = [
    ["setRoomTitle", () => mapper.setRoomTitle(S, 2, "x"), true],
    ["setRoomDescription", () => mapper.setRoomDescription(S, 2, "x"), true],
    ["setRoomColor", () => mapper.setRoomColor(S, 2, "#ffffff"), true],
    ["setRoomLevel", () => mapper.setRoomLevel(S, 2, 1), true],
    ["setRoomX", () => mapper.setRoomX(S, 2, 1), true],
    ["setRoomY", () => mapper.setRoomY(S, 2, 1), true],
    ["setRoomExternalId", () => mapper.setRoomExternalId(S, 2, "x"), true],
    ["setRoomProperty", () => mapper.setRoomProperty(S, 2, "k", "v"), true],
    ["setAreaProperty", () => mapper.setAreaProperty(S, "k", "v"), true],
    ["deleteRoomProperty", () => mapper.deleteRoomProperty(S, 2, "k"), false],
    ["deleteAreaProperty", () => mapper.deleteAreaProperty(S, "k"), false],
    ["addRoomTag", () => mapper.addRoomTag(S, 2, "t"), true],
    ["removeRoomTag", () => mapper.removeRoomTag(S, 2, "t"), false],
    ["createRoom", () => mapper.createRoom(S, {{ title: "x" }}), true],
    ["updateRoom", () => mapper.updateRoom(S, 2, {{ title: "y" }}), true],
    ["updateRooms", () => mapper.updateRooms(S, [[2, {{ title: "z" }}]]), true],
    ["mutateArea createRoom", () => mapper.mutateArea(S, async (m) => {{ await m.createRoom({{ title: "w" }}); }}), false],
    ["createRoomExit", () => mapper.createRoomExit(S, 2, {{ fromDirection: "North" }}), false],
    ["exit into the Secret", () => mapper.createRoomExit(map.id, 1, {{ fromDirection: "Up", toAreaId: S, toRoomNumber: 2 }}), false],
    ["setRoomExit", () => mapper.setRoomExit(S, 2, EXIT, {{ weight: 2 }}), false],
    ["mergeRooms", () => mapper.mergeRooms(S, 2, 3), false],
    ["createLink", () => mapper.createLink(S, {{ endpointA: endpoint, traversals: [] }}), false],
    ["setConnection", () => mapper.setConnection(S, CONN, {{ dash: "Dashed" }}), false],
    ["pairConnections", () => mapper.pairConnections(S, CONN, CONN), false],
    ["unlinkRoomExit", () => mapper.unlinkRoomExit(S, EXIT), false],
    ["createLabel", () => mapper.createLabel(S, {{ x: 0, y: 0, width: 1, height: 1, text: "t" }}), false],
    ["createShape", () => mapper.createShape(S, {{ x: 0, y: 0, width: 1, height: 1 }}), false],
    ["setLabel", () => mapper.setLabel(S, LABEL, {{ text: "x" }}), false],
    ["setShape", () => mapper.setShape(S, LABEL, {{ x: 1 }}), false],
    ["deleteLabel", () => mapper.deleteLabel(S, LABEL), false],
    ["deleteShape", () => mapper.deleteShape(S, LABEL), false],
    ["mutateArea", () => mapper.mutateArea(S, (m) => m.setAreaProperty("a", "b")), true],
    ["mergeAreas", () => mapper.mergeAreas(map, [S]), false],
    ["renameArea", () => mapper.renameArea(S, "x"), false],
    ["deleteRoomExit", () => mapper.deleteRoomExit(S, 2, EXIT), false],
    ["deleteLink", () => mapper.deleteLink(S, CONN), false],
];
try {{
for (const [label, run, lands] of writes) {{
    const outcome = await failure(run);
    if (label === "exit into the Secret") {{
        check(label, outcome, "no error");
        const exit = mapper.getAreaById(map.id).room(1).exits.find((e) => e.fromDirection === "Up");
        check("link owner", exit?.place, "map");
        check("link destination", exit?.toRoom?.areaId, S);
    }} else if (REFUSED) {{
        check(label, outcome, NOT_CAPABLE("secrets-write"));
    }} else if (lands) {{
        check(label, outcome, "no error");
    }} else if (outcome === NOT_CAPABLE("secrets-write")) {{
        failures.push(`${{label}}: refused for secrets-write`);
    }}
}}
}} catch (error) {{
    failures.push(`threw ${{error?.stack ?? error}}`);
}}

echo(failures.length === 0 ? "PLACES_WRITE_TIER_OK" : `PLACES_WRITE_TIER_FAIL ${{failures.join(" | ")}}`);
"##,
        prelude = prelude()
    )
}

fn property(name: &str, value: &str) -> Property {
    Property {
        name: name.to_string(),
        value: value.to_string(),
    }
}

/// One cloud map with two Secrets, served as the cloud serves it; place
/// writes apply to the served bundles, so a refetch shows them.
struct PlacesCloud {
    details: Mutex<AreaWithDetails>,
    envelopes: Mutex<Vec<MutationEnvelope>>,
    /// Each create and update request, as the server received it.
    lifecycle: Mutex<Vec<String>>,
    /// A second map, "Road", when the run has one (see [`Self::with_road`]).
    road: Mutex<Option<AreaWithDetails>>,
}

impl PlacesCloud {
    #[allow(clippy::too_many_lines)] // the whole fixture in one place
    fn new() -> Self {
        let map = AreaId(MAP);
        let room = |number: i32, title: &str, notes: Option<&str>, tags: &[&str]| {
            serde_json::json!({
                "room_number": number, "title": title, "description": "", "color": "",
                "level": 0, "x": f64::from(number), "y": 0.0, "exits": [], "tags": tags,
                "properties": notes.map_or_else(Vec::new, |notes| vec![serde_json::json!({"name": "notes", "value": notes})]),
            })
        };
        let exit = |id: u128, to_room: i32, to_source: Option<Uuid>| {
            serde_json::json!({
                "id": Uuid::from_u128(id), "from_direction": "East",
                "to_area_id": map, "to_room_number": to_room, "to_source": to_source,
                "to_direction": null, "to_unknown": false, "path": "", "command": "",
                "weight": 1.0, "connection_id": Uuid::from_u128(id + 0x100),
                "is_hidden": true, "door": null
            })
        };
        let mut vault = room(2, "Vault", Some("Vault notes"), &["VAULT"]);
        vault["exits"] = serde_json::json!([exit(0x22, 3, None)]);
        vault["external_id"] = serde_json::json!("v-2");
        // Each exit's Connection; the end on the Bookcase's own room names its source.
        let connection = |id: u128, (from, from_own): (i32, bool), (to, to_own): (i32, bool)| {
            let end = |room: i32, own: bool, side: &str| {
                let mut end = serde_json::json!({
                    "room_number": room, "side": side, "port_offset": 0.5, "port_mode": "AutoPinned"
                });
                if own {
                    end["source"] = serde_json::json!(BOOKCASE);
                }
                end
            };
            serde_json::json!({
                "id": Uuid::from_u128(id + 0x100),
                "endpoint_a": end(from, from_own, "East"),
                "endpoint_b": end(to, to_own, "West"),
                "kind": "Internal", "routing": "Simple", "segment_shape": "Direct",
                "corner": "Sharp", "route_points": [], "dash": "Solid",
                "color": "#A4A4A4", "thickness": 1.0
            })
        };
        let bookcase: SourceBundle = serde_json::from_value(serde_json::json!({
            "source": BOOKCASE, "name": "Bookcase", "ownership": "owner", "color": "#8a5cf6",
            "rev": 1, "actions": ["read", "add", "edit", "remove"],
            "properties": [{ "name": "summary", "value": "Hidden room" }],
            "rooms": [vault],
            "room_data": [
                {
                    "room_number": 1,
                    "properties": [{ "name": "notes", "value": "Behind the bookcase" }],
                    "tags": ["DOOR"],
                    "exits": [exit(0x21, 2, Some(BOOKCASE))],
                },
                { "room_number": 3, "tags": ["LOOT"] },
            ],
            "connections": [
                connection(0x21, (1, false), (2, true)),
                connection(0x22, (2, true), (3, false)),
            ],
        }))
        .expect("the Bookcase parses");
        // The caller's Private additions keep a den of their own behind a hidden door on
        // map room 3.
        let mut den = room(7, "Den", None, &["DEN"]);
        den["external_id"] = serde_json::json!("p-7");
        let private: SourceBundle = serde_json::from_value(serde_json::json!({
            "source": "private", "rev": 1, "actions": ["read", "add", "edit", "remove"],
            "rooms": [den],
            "room_data": [{
                "room_number": 3,
                "exits": [{
                    "id": Uuid::from_u128(0x41), "from_direction": "Down",
                    "to_area_id": map, "to_room_number": 7, "to_source": "private",
                    "to_direction": null, "to_unknown": false, "path": "", "command": "",
                    "weight": 1.0, "connection_id": Uuid::from_u128(0x141),
                    "is_hidden": true, "door": null
                }],
            }],
            "connections": [{
                "id": Uuid::from_u128(0x141),
                "endpoint_a": { "room_number": 3, "side": "South", "port_offset": 0.5, "port_mode": "AutoPinned" },
                "endpoint_b": {
                    "room_number": 7, "source": "private", "side": "North",
                    "port_offset": 0.5, "port_mode": "AutoPinned"
                },
                "kind": "Internal", "routing": "Simple", "segment_shape": "Direct",
                "corner": "Sharp", "route_points": [], "dash": "Solid",
                "color": "#A4A4A4", "thickness": 1.0
            }],
        }))
        .expect("the Private additions parse");
        let ledger: SourceBundle = serde_json::from_value(serde_json::json!({
            "source": LEDGER, "name": "Ledger", "ownership": "members",
            "clan_id": CLAN, "rev": 4, "actions": ["read", "manage_access"],
            "room_data": [{
                "room_number": 1,
                "properties": [{ "name": "notes", "value": "Owed 3 gold" }],
            }],
        }))
        .expect("the Ledger parses");
        // The map's own long way round from room 1 to room 3, through rooms 4 and 5: the
        // Bookcase's door is a shortcut beside it.
        let mut library = room(1, "Library", Some("Ordinary"), &["START"]);
        library["exits"] = serde_json::json!([exit(0x31, 4, None)]);
        let mut hall = room(4, "Hall", None, &[]);
        hall["exits"] = serde_json::json!([exit(0x32, 5, None)]);
        hall["external_id"] = serde_json::json!("m-4");
        let mut stair = room(5, "Stair", None, &[]);
        stair["exits"] = serde_json::json!([exit(0x33, 3, None)]);
        let rooms = serde_json::from_value(serde_json::json!([
            library,
            room(3, "Garden", None, &[]),
            hall,
            stair
        ]))
        .expect("the map's rooms parse");
        let connections = serde_json::from_value(serde_json::json!([
            connection(0x31, (1, false), (4, false)),
            connection(0x32, (4, false), (5, false)),
            connection(0x33, (5, false), (3, false)),
        ]))
        .expect("the map's connections parse");
        Self {
            details: Mutex::new(AreaWithDetails {
                room_data: Vec::new(),
                area: Area {
                    id: map,
                    user_id: None,
                    atlas_id: None,
                    name: "Library".to_string(),
                    created_at: Utc::now(),
                    rev: 1,
                    access: Some(AreaAccess::OWNER),
                    owner_nickname: None,
                    copied_from_area_id: None,
                    copied_from_rev: None,
                    copied_at: None,
                    family_token: None,
                    clan_id: None,
                    clan_name: None,
                    actions: None,
                    clan_ownership: smudgy_cloud::clan_maps::ClanOwnership::default(),
                    atlas_name: None,
                    projection_token: Some("p_places".to_string()),
                },
                format_version: AREA_FORMAT_VERSION,
                properties: vec![],
                rooms,
                labels: vec![],
                shapes: vec![],
                connections,
                linked_areas: vec![],
                sources: vec![bookcase, ledger, private],
            }),
            envelopes: Mutex::new(Vec::new()),
            lifecycle: Mutex::new(Vec::new()),
            road: Mutex::new(None),
        }
    }

    /// Beside the map, "Road": room 1 "Milestone" (tagged `MILE`) leads East to room 2
    /// "Tollgate". With `into_secret`, the milestone also has a door leading North into
    /// the Bookcase's own room 2 (the Vault), as its reader is served it: an exit into
    /// another map's Secret room, with its connection and the `linked_areas` entry only
    /// it makes.
    fn with_road(mut self, into_secret: bool) -> Self {
        let exit = |id: u128, direction: &str, to: (Uuid, i32), to_source: Option<Uuid>| {
            serde_json::json!({
                "id": Uuid::from_u128(id), "from_direction": direction,
                "to_area_id": to.0, "to_room_number": to.1, "to_source": to_source,
                "to_direction": null, "to_unknown": false, "path": "", "command": "",
                "weight": 1.0, "connection_id": Uuid::from_u128(id + 0x100),
                "is_hidden": false,
                "door": { "state": "locked", "name": "grate", "opens_with": "lift grate" }
            })
        };
        let mut exits = vec![exit(0x51, "East", (ROAD, 2), None)];
        let end = |room: i32, side: &str| serde_json::json!({ "room_number": room, "side": side, "port_offset": 0.5, "port_mode": "AutoPinned" });
        let mut connections = vec![serde_json::json!({
            "id": Uuid::from_u128(0x151), "endpoint_a": end(1, "East"), "endpoint_b": end(2, "West"),
            "kind": "Internal", "routing": "Simple", "segment_shape": "Direct", "corner": "Sharp",
            "route_points": [], "dash": "Solid", "color": "#A4A4A4", "thickness": 1.0
        })];
        let mut linked_areas = Vec::new();
        if into_secret {
            exits.push(exit(0x52, "North", (MAP, 2), Some(BOOKCASE)));
            connections.push(serde_json::json!({
                "id": Uuid::from_u128(0x152), "endpoint_a": end(1, "North"),
                "kind": "External", "routing": "Simple", "segment_shape": "Direct",
                "corner": "Sharp", "route_points": [], "dash": "Solid", "color": "#A4A4A4",
                "thickness": 1.0
            }));
            linked_areas
                .push(serde_json::json!({ "to_area_id": MAP, "name": "Library", "visible": true }));
        }
        let road = serde_json::json!({
            "id": ROAD, "user_id": null, "atlas_id": null, "name": "Road",
            "created_at": Utc::now(), "rev": 1, "access": AreaAccess::OWNER,
            "projection_token": "p_road", "format_version": AREA_FORMAT_VERSION,
            "properties": [],
            "rooms": [
                {
                    "room_number": 1, "title": "Milestone", "description": "", "color": "",
                    "level": 0, "x": 0.0, "y": 0.0, "exits": exits, "tags": ["MILE"],
                    "properties": []
                },
                {
                    "room_number": 2, "title": "Tollgate", "description": "", "color": "",
                    "level": 0, "x": 2.0, "y": 0.0, "exits": [], "tags": [], "properties": []
                }
            ],
            "labels": [], "shapes": [], "connections": connections,
            "linked_areas": linked_areas,
        });
        self.road = Mutex::new(Some(serde_json::from_value(road).expect("the Road parses")));
        self
    }

    /// The same map with no Secrets and no Private additions at all. Its
    /// projection token differs, as the server's token covers every source
    /// the projection shows.
    fn without_sources() -> Self {
        let cloud = Self::new();
        {
            let mut details = cloud.details.lock().unwrap();
            details.sources.clear();
            details.area.projection_token = Some("p_plain".to_string());
        }
        cloud
    }

    /// The map takes new rooms and nothing else, all or none, refusing a number
    /// the map already uses as the server does. Its Secrets' and Private
    /// additions' numbers are theirs: the server numbers each place on its own.
    #[allow(clippy::too_many_lines)] // Keeps the deliberately limited fixture's accepted wire operations together.
    fn create_map_rooms(
        &self,
        area_id: AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        let mut details = self.details.lock().unwrap();
        // The read-only Secret consent probe creates a Map-owned exit into
        // the readable Bookcase. Persist its exit and connection together.
        if let [
            AreaMutation::CreateExit {
                room_number,
                room_source: None,
                body,
            },
        ] = envelope.payload.as_slice()
            && body.to_area_id == Some(area_id)
            && body.to_source == Some(SourceId::Secret(BOOKCASE))
            && body.to_room_number == Some(RoomNumber(2))
        {
            let connection_id = body.new_connection_id.expect("a compiled new connection");
            let exit: smudgy_cloud::Exit = serde_json::from_value(serde_json::json!({
                "id": body.id.unwrap(), "from_direction": body.from_direction,
                "to_area_id": area_id, "to_room_number": 2, "to_source": BOOKCASE,
                "to_direction": body.to_direction, "path": body.path.clone().unwrap_or_default(),
                "is_hidden": body.is_hidden, "door": body.door, "weight": body.weight,
                "command": body.command.clone().unwrap_or_default(), "connection_id": connection_id
            }))
            .unwrap();
            let connection = serde_json::from_value(serde_json::json!({
                "id": connection_id,
                "endpoint_a": { "room_number": room_number, "side": "North", "port_offset": 0.5, "port_mode": "AutoPinned" },
                "endpoint_b": { "room_number": 2, "source": BOOKCASE, "side": "South", "port_offset": 0.5, "port_mode": "AutoPinned" },
                "kind": "Internal", "routing": "Simple", "segment_shape": "Direct", "corner": "Sharp",
                "route_points": [], "dash": "Solid", "color": "#A4A4A4", "thickness": 1.0
            })).unwrap();
            details
                .rooms
                .iter_mut()
                .find(|room| room.room_number == *room_number)
                .unwrap()
                .exits
                .push(exit);
            details.connections.push(connection);
            details.area.rev += 1;
            return Ok(MutationResult {
                operation_id: envelope.operation_id,
                versions: vec![VersionInfo::map_source(area_id.0, details.area.rev)],
                data: vec![],
            });
        }
        // This fixture also accepts Map-owned properties and tags on rooms
        // in other readable sources. Keep ordinary map-write probes unchanged.
        if envelope.payload.iter().all(|operation| {
            matches!(operation,
            AreaMutation::UpsertRoomProperty { room_source: Some(source), .. }
            | AreaMutation::DeleteRoomProperty { room_source: Some(source), .. }
            | AreaMutation::AddRoomTag { room_source: Some(source), .. }
            | AreaMutation::RemoveRoomTag { room_source: Some(source), .. }
                if !source.is_map())
        }) {
            let mut bundle: SourceBundle = serde_json::from_value(serde_json::json!({
                "source": "map", "rev": details.area.rev, "actions": ["read", "add", "edit", "remove"],
                "room_data": details.room_data,
            })).unwrap();
            for operation in &envelope.payload {
                apply(&mut bundle, operation);
            }
            details.room_data = bundle.room_data;
            details.area.rev += 1;
            return Ok(MutationResult {
                operation_id: envelope.operation_id,
                versions: vec![VersionInfo::map_source(area_id.0, details.area.rev)],
                data: vec![],
            });
        }
        let mut created = Vec::new();
        for operation in &envelope.payload {
            let AreaMutation::CreateRoom {
                room_number,
                room_source: None,
                body,
            } = operation
            else {
                return Err(CloudError::PermissionDenied(
                    "scripted: the map takes new rooms only".to_string(),
                ));
            };
            if details
                .rooms
                .iter()
                .chain(&created)
                .any(|room| room.room_number == *room_number)
            {
                return Err(CloudError::StructuralConflict(
                    "room_number_exists".to_string(),
                ));
            }
            created.push(
                serde_json::from_value(serde_json::json!({
                    "room_number": room_number, "title": body.title.clone().unwrap_or_default(),
                    "description": "", "color": "", "level": 0, "x": 0.0, "y": 0.0,
                    "exits": [], "tags": [], "properties": [],
                }))
                .expect("the new room parses"),
            );
        }
        details.rooms.extend(created);
        details.area.rev += 1;
        Ok(MutationResult {
            operation_id: envelope.operation_id,
            versions: vec![VersionInfo::map_source(area_id.0, details.area.rev)],
            data: vec![],
        })
    }
}

fn summary_of(bundle: &SourceBundle) -> SecretSummary {
    SecretSummary {
        source: bundle.source,
        name: bundle.name.clone().unwrap_or_default(),
        ownership: bundle.ownership.clone().unwrap_or_default(),
        clan_id: bundle.clan_id,
        color: bundle.color.clone(),
        actions: bundle.actions.clone(),
    }
}

/// One of `bundle`'s own rooms, which the write names.
fn own_room(bundle: &mut SourceBundle, room_number: RoomNumber) -> &mut RoomWithDetails {
    bundle
        .rooms
        .iter_mut()
        .find(|room| room.room_number == room_number)
        .unwrap_or_else(|| panic!("the place has no room {room_number}"))
}

/// Applies the place writes this test makes to `bundle`, as the server would.
fn apply(bundle: &mut SourceBundle, operation: &AreaMutation) {
    let source = bundle.source;
    let mut data_for = |room_number, room_source: Option<SourceId>| {
        if let Some(index) = bundle
            .room_data
            .iter()
            .position(|data| data.room_number == room_number && data.room_source == room_source)
        {
            return index;
        }
        bundle.room_data.push(RoomData {
            room_source,
            room_number,
            properties: Vec::new(),
            tags: std::collections::BTreeSet::default(),
            exits: Vec::new(),
        });
        bundle.room_data.len() - 1
    };
    match operation {
        AreaMutation::UpsertRoomProperty {
            room_number,
            room_source,
            name,
            value,
        } if room_source.unwrap_or(SourceId::Map) != source => {
            let index = data_for(*room_number, *room_source);
            let properties = &mut bundle.room_data[index].properties;
            properties.retain(|property| property.name != *name);
            properties.push(property(name, value));
        }
        AreaMutation::DeleteRoomProperty {
            room_number,
            room_source,
            name,
        } if room_source.unwrap_or(SourceId::Map) != source => {
            let index = data_for(*room_number, *room_source);
            bundle.room_data[index]
                .properties
                .retain(|property| property.name != *name);
        }
        AreaMutation::AddRoomTag {
            room_number,
            room_source,
            tag,
        } if room_source.unwrap_or(SourceId::Map) != source => {
            let index = data_for(*room_number, *room_source);
            bundle.room_data[index].tags.insert(tag.to_uppercase());
        }
        AreaMutation::RemoveRoomTag {
            room_number,
            room_source,
            tag,
        } if room_source.unwrap_or(SourceId::Map) != source => {
            let index = data_for(*room_number, *room_source);
            bundle.room_data[index].tags.remove(&tag.to_uppercase());
        }
        AreaMutation::UpsertAreaProperty { name, value } => {
            bundle.properties.retain(|property| property.name != *name);
            bundle.properties.push(property(name, value));
        }
        AreaMutation::DeleteAreaProperty { name } => {
            bundle.properties.retain(|property| property.name != *name);
        }
        AreaMutation::CreateRoom {
            room_number,
            room_source: Some(_),
            body,
        } => {
            assert!(
                bundle
                    .rooms
                    .iter()
                    .all(|room| room.room_number != *room_number),
                "the place already uses room {room_number}"
            );
            bundle.rooms.push(
                serde_json::from_value(serde_json::json!({
                    "room_number": room_number, "title": body.title.clone().unwrap_or_default(),
                    "description": "", "color": "", "level": 0, "x": 0.0, "y": 0.0,
                    "exits": [], "tags": [], "properties": [],
                }))
                .expect("the new room parses"),
            );
        }
        AreaMutation::UpsertRoom {
            room_number,
            room_source: Some(_),
            body,
        } => {
            let room = own_room(bundle, *room_number);
            if let Some(title) = &body.title {
                room.title.clone_from(title);
            }
            if let Some(description) = &body.description {
                room.description.clone_from(description);
            }
            if let Some(color) = &body.color {
                room.color.clone_from(color);
            }
            if let Some(level) = body.level {
                room.level = level;
            }
            if let Some(x) = body.x {
                room.x = x;
            }
            if let Some(y) = body.y {
                room.y = y;
            }
        }
        AreaMutation::DeleteRoom {
            room_number,
            room_source: Some(_),
        } => {
            bundle.rooms.retain(|room| room.room_number != *room_number);
        }
        AreaMutation::UpsertRoomProperty {
            room_number,
            room_source: Some(_),
            name,
            value,
        } => {
            let properties = &mut own_room(bundle, *room_number).properties;
            properties.retain(|property| property.name != *name);
            properties.push(property(name, value));
        }
        AreaMutation::DeleteRoomProperty {
            room_number,
            room_source: Some(_),
            name,
        } => {
            own_room(bundle, *room_number)
                .properties
                .retain(|property| property.name != *name);
        }
        AreaMutation::AddRoomTag {
            room_number,
            room_source: Some(_),
            tag,
        } => {
            own_room(bundle, *room_number)
                .tags
                .insert(tag.to_uppercase());
        }
        AreaMutation::RemoveRoomTag {
            room_number,
            room_source: Some(_),
            tag,
        } => {
            own_room(bundle, *room_number)
                .tags
                .remove(&tag.to_uppercase());
        }
        // Exits, links and drawings: the client's copy is what the script reads.
        AreaMutation::CreateExit { .. }
        | AreaMutation::UpdateExit { .. }
        | AreaMutation::DeleteExit { .. }
        | AreaMutation::CreateConnection { .. }
        | AreaMutation::UpdateConnection { .. }
        | AreaMutation::Pair { .. }
        | AreaMutation::Unlink { .. }
        | AreaMutation::DeleteLink { .. }
        | AreaMutation::CreateLabel { .. }
        | AreaMutation::UpdateLabel { .. }
        | AreaMutation::DeleteLabel { .. }
        | AreaMutation::CreateShape { .. }
        | AreaMutation::UpdateShape { .. }
        | AreaMutation::DeleteShape { .. } => {}
        other => panic!("unexpected operation {other:?}"),
    }
}

#[async_trait]
impl MapperBackend for PlacesCloud {
    async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
        Err(CloudError::PermissionDenied(
            "scripted: no new maps".to_string(),
        ))
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        let mut areas = vec![self.details.lock().unwrap().area.clone()];
        areas.extend(
            self.road
                .lock()
                .unwrap()
                .iter()
                .map(|road| road.area.clone()),
        );
        Ok(areas)
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        let details = self.details.lock().unwrap();
        if *area_id == details.area.id {
            Ok(details.clone())
        } else if let Some(road) = self
            .road
            .lock()
            .unwrap()
            .as_ref()
            .filter(|road| road.area.id == *area_id)
        {
            Ok(road.clone())
        } else {
            Err(CloudError::NotFoundOrNoAccess)
        }
    }

    // The server answers an id that names no map it holds (a Secret's among them) with
    // its uniform not-found.
    async fn update_area(&self, area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
        if *area_id == AreaId(MAP) {
            Ok(())
        } else {
            Err(CloudError::NotFoundOrNoAccess)
        }
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        if *area_id == AreaId(MAP) {
            Err(CloudError::PermissionDenied(
                "scripted: no deletes".to_string(),
            ))
        } else {
            Err(CloudError::NotFoundOrNoAccess)
        }
    }

    async fn create_secret_as(
        &self,
        _area_id: &AreaId,
        secret: &smudgy_cloud::clan_secrets::NewSecret,
        _auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let name = &secret.name;
        let color = secret.color.as_deref();
        let body = secret.body();
        let ownership = body["ownership"].as_str().unwrap_or("owner");
        self.lifecycle.lock().unwrap().push(match ownership {
            "owner" => format!("create {name} {color:?}"),
            _ => format!("create {name} {color:?} {body}"),
        });
        let bundle: SourceBundle = serde_json::from_value(serde_json::json!({
            "source": QUEST, "name": name, "ownership": ownership, "rev": 0,
            "clan_id": body.get("clan_id"),
            "color": color.map(str::to_lowercase),
            "actions": ["read", "add", "edit", "remove", "manage_access", "rename", "delete"],
        }))
        .expect("the new Secret parses");
        let summary = summary_of(&bundle);
        self.details.lock().unwrap().sources.push(bundle);
        Ok(summary)
    }

    async fn update_secret(
        &self,
        _area_id: &AreaId,
        secret: &SourceId,
        change: &SecretChange,
        _auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        self.lifecycle
            .lock()
            .unwrap()
            .push(format!("update {}", change.body()));
        let mut details = self.details.lock().unwrap();
        let bundle = details
            .sources
            .iter_mut()
            .find(|bundle| bundle.source == *secret)
            .ok_or(CloudError::NotFoundOrNoAccess)?;
        if let Some(name) = &change.name {
            bundle.name = Some(name.clone());
        }
        match &change.color {
            SecretColorChange::Keep => {}
            SecretColorChange::Clear => bundle.color = None,
            SecretColorChange::Set(color) => bundle.color = Some(color.to_lowercase()),
        }
        bundle.rev += 1;
        Ok(summary_of(bundle))
    }

    async fn delete_secret(
        &self,
        _area_id: &AreaId,
        secret: &SourceId,
        _auth_generation: u64,
    ) -> CloudResult<()> {
        let mut details = self.details.lock().unwrap();
        let before = details.sources.len();
        details.sources.retain(|bundle| bundle.source != *secret);
        if details.sources.len() == before {
            return Err(CloudError::NotFoundOrNoAccess);
        }
        Ok(())
    }

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        self.envelopes.lock().unwrap().push(envelope.clone());
        if *area_id == AreaId(ROAD) {
            let mut road = self.road.lock().unwrap();
            let road = road.as_mut().ok_or(CloudError::NotFoundOrNoAccess)?;
            for operation in &envelope.payload {
                match operation {
                    AreaMutation::UpdateExit { exit_id, body } => {
                        let exit = road
                            .rooms
                            .iter_mut()
                            .flat_map(|room| &mut room.exits)
                            .find(|exit| exit.id == *exit_id)
                            .ok_or(CloudError::ExitNotFound(*exit_id))?;
                        if let Some(weight) = body.weight {
                            exit.weight = weight;
                        }
                    }
                    AreaMutation::UpdateConnection {
                        connection_id,
                        body,
                    } => {
                        let connection = road
                            .connections
                            .iter_mut()
                            .find(|connection| connection.id == *connection_id)
                            .ok_or(CloudError::InvalidConnection("connection_not_found".into()))?;
                        if let Some(thickness) = body.thickness {
                            connection.thickness = thickness;
                        }
                    }
                    _ => {
                        return Err(CloudError::PermissionDenied(
                            "scripted: unsupported Road write".into(),
                        ));
                    }
                }
            }
            road.area.rev += 1;
            return Ok(MutationResult {
                operation_id: envelope.operation_id,
                versions: vec![VersionInfo::map_source(area_id.0, road.area.rev)],
                data: vec![],
            });
        }
        if envelope.source.is_map() {
            return self.create_map_rooms(*area_id, envelope);
        }
        let mut details = self.details.lock().unwrap();
        if !details
            .sources
            .iter()
            .any(|bundle| bundle.source == envelope.source)
        {
            let mut private: SourceBundle = serde_json::from_value(serde_json::json!({
                "source": "private", "rev": 0, "actions": ["read", "add", "edit", "remove"],
            }))
            .expect("the Private bundle parses");
            private.source = envelope.source;
            details.sources.push(private);
        }
        let bundle = details
            .sources
            .iter_mut()
            .find(|bundle| bundle.source == envelope.source)
            .expect("the written place");
        for operation in &envelope.payload {
            apply(bundle, operation);
        }
        bundle.rev += 1;
        Ok(MutationResult {
            operation_id: envelope.operation_id,
            versions: vec![VersionInfo {
                resource: ResourceKind::Source,
                id: area_id.0,
                source: envelope.source,
                rev: bundle.rev,
                deleted: false,
            }],
            data: vec![],
        })
    }
}

fn smudgy_home() -> PathBuf {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    smudgy_core::get_smudgy_home().expect("smudgy home")
}

/// Prepares `server`'s directories under the shared smudgy home.
fn prepare_server(server: &str) -> PathBuf {
    let home = smudgy_home();
    std::fs::create_dir_all(home.join(server).join("modules")).expect("create modules directory");
    std::fs::create_dir_all(home.join(server).join("logs")).expect("create logs directory");
    home
}

fn params(session: u32, server: &str, mapper: Mapper) -> Arc<SessionParams> {
    Arc::new(SessionParams {
        session_id: SessionId::from(session),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper),
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })
}

/// Collects a session's appended lines until one starts with `sentinel` (and
/// a moment after), or the deadline passes. When a line equal to the next
/// cue's text appears, that cue's actions go to the runtime, in order.
async fn run_until(
    events: impl futures::Stream<Item = smudgy_core::session::TaggedSessionEvent>,
    sentinel: &str,
    cues: Vec<(&str, Vec<RuntimeAction>)>,
) -> Vec<String> {
    run_until_with(events, sentinel, cues, |_| None).await
}

/// What a test does when a line appears: the actions it then sends to the runtime.
type Reaction = futures::future::BoxFuture<'static, Vec<RuntimeAction>>;

/// [`run_until`], also running `hook` on each appended line and sending the actions its
/// reaction ends with.
async fn run_until_with(
    events: impl futures::Stream<Item = smudgy_core::session::TaggedSessionEvent>,
    sentinel: &str,
    cues: Vec<(&str, Vec<RuntimeAction>)>,
    mut hook: impl FnMut(&str) -> Option<Reaction>,
) -> Vec<String> {
    let mut cues = std::collections::VecDeque::from(cues);
    let mut events = Box::pin(events);
    let mut lines = Vec::new();
    let mut runtime: Option<tokio::sync::mpsc::UnboundedSender<RuntimeAction>> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut quiet = None;
    loop {
        if quiet.is_none() && lines.iter().any(|line: &String| line.starts_with(sentinel)) {
            quiet = Some(Duration::from_millis(250));
        }
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            break;
        };
        let wait = quiet.map_or(remaining, |quiet: Duration| quiet.min(remaining));
        let Ok(Some(event)) = tokio::time::timeout(wait, events.next()).await else {
            break;
        };
        match event.event {
            SessionEvent::RuntimeReady(tx) => runtime = Some(tx),
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                        if let Some(reaction) = hook(&line.text) {
                            let actions = reaction.await;
                            for action in actions {
                                if let Some(tx) = runtime.as_ref() {
                                    tx.send(action).expect("the runtime takes the reaction");
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        while let (Some((cue, _)), Some(tx)) = (cues.front(), runtime.as_ref()) {
            if !lines.iter().any(|line| line == cue) {
                break;
            }
            let (_, actions) = cues.pop_front().expect("the cue just read");
            for action in actions {
                tx.send(action).expect("the runtime takes the cue's action");
            }
        }
    }
    if let Some(tx) = runtime {
        tx.send(RuntimeAction::Shutdown).ok();
    }
    lines
}

#[tokio::test]
async fn scripts_reach_a_maps_places() {
    const SERVER: &str = "MapperScriptPlaces";
    let home = prepare_server(SERVER);
    std::fs::write(
        home.join(SERVER).join("modules").join("places.ts"),
        trusted_module(),
    )
    .expect("write the module");
    let cloud = Arc::new(PlacesCloud::new());
    let mapper = Mapper::new(cloud.clone(), home.join("map-places-cloud"));

    let lines = run_until(
        spawn(params(9_388, SERVER, mapper)),
        "SCRIPT_PLACES_",
        Vec::new(),
    )
    .await;
    assert!(
        lines.iter().any(|line| line == "SCRIPT_PLACES_OK"),
        "places misbehaved:\n{}",
        lines.join("\n")
    );

    assert_eq!(
        *cloud.lifecycle.lock().unwrap(),
        [
            r##"create Quest Some("#ABCDEF")"##.to_string(),
            r##"update {"name":"Phylactery","color":"#123456"}"##.to_string(),
            r#"update {"color":null}"#.to_string(),
            r#"create Survey None {"name":"Survey","ownership":"clan","clan_id":"11111111-1111-4111-8111-111111111111"}"#.to_string(),
        ],
        "a create carries its color and owner, and an update its name and color, in one request each"
    );

    let envelopes = cloud.envelopes.lock().unwrap();
    let door = envelopes
        .iter()
        .position(|envelope| {
            envelope
                .payload
                .iter()
                .any(|operation| matches!(operation, AreaMutation::CreateExit { .. }))
        })
        .expect("the hidden door was written");
    let door = &envelopes[door];
    assert_eq!(door.source, SourceId::Secret(BOOKCASE));
    assert!(
        door.payload.iter().any(|operation| matches!(
            operation,
            AreaMutation::CreateExit {
                room_number,
                room_source: None,
                ..
            } if room_number.0 == 1
        )),
        "the door leaves map room 1 as the Secret's data: {:?}",
        door.payload
    );
    let written: Vec<(SourceId, serde_json::Value)> = envelopes
        .iter()
        .filter(|envelope| !std::ptr::eq(*envelope, door))
        .map(|envelope| {
            (
                envelope.source,
                serde_json::to_value(&envelope.payload).expect("the payload serializes"),
            )
        })
        .collect();
    let bookcase = SourceId::Secret(BOOKCASE);
    assert_eq!(
        written,
        [
            (
                bookcase,
                serde_json::json!([{ "op": "upsert_room_property", "room_number": 1, "name": "notes", "value": "Quest note" }]),
            ),
            (
                bookcase,
                serde_json::json!([{ "op": "add_room_tag", "room_number": 1, "tag": "quest" }]),
            ),
            (
                SourceId::Private,
                serde_json::json!([{ "op": "upsert_room_property", "room_number": 1, "name": "notes", "value": "Mine" }]),
            ),
            (
                bookcase,
                serde_json::json!([
                    { "op": "upsert_room_property", "room_number": 1, "name": "kws", "value": "lever" },
                    { "op": "upsert_area_property", "name": "summary", "value": "Two rooms" }
                ]),
            ),
            (
                bookcase,
                serde_json::json!([{ "op": "delete_room_property", "room_number": 1, "name": "kws" }]),
            ),
            (
                bookcase,
                serde_json::json!([{ "op": "remove_room_tag", "room_number": 1, "tag": "quest" }]),
            ),
            (
                bookcase,
                serde_json::json!([{ "op": "delete_area_property", "name": "summary" }]),
            ),
            (
                SourceId::Secret(QUEST),
                serde_json::json!([{
                    "op": "create_room", "room_number": 1, "room_source": QUEST,
                    "body": { "title": "Crypt", "description": null, "level": null, "x": null, "y": null, "color": null },
                }]),
            ),
            (
                bookcase,
                serde_json::json!([{
                    "op": "create_room", "room_number": 3, "room_source": BOOKCASE,
                    "body": { "title": "Alcove", "description": null, "level": null, "x": null, "y": null, "color": null },
                }]),
            ),
        ],
        "each write reaches exactly its place, keyed by the map room; adding a tag the place \
         keeps sends nothing; each Secret numbers its own new rooms"
    );
}

fn package(src: &str) -> ResolvedPackage {
    ResolvedPackage {
        key: PackageKey {
            owner: "wbk".to_string(),
            name: "cartographer".to_string(),
        },
        resolved_version: "1.0.0".to_string(),
        manifest: PackageManifest::parse(r#"{ "name": "cartographer", "version": "1.0.0" }"#)
            .expect("valid manifest"),
        integrity: "test-cartographer-1.0.0".to_string(),
        modules: vec![PackageModuleSource {
            subpath: "index.js".to_string(),
            text: src.to_string(),
        }],
    }
}

/// One session for [`run_package`]: the fixture map it reads, what the package is
/// consented beyond the map and `echo`, a trusted user module beside it, and the
/// actions to send on cue lines.
struct PackageRun<'a> {
    session: u32,
    server: &'a str,
    cloud: PlacesCloud,
    consent: fn(&mut SmudgyCapabilities),
    user_module: Option<String>,
    cues: Vec<(&'a str, Vec<RuntimeAction>)>,
}

/// Runs `src` as an installed package consented the map, `echo` and whatever the
/// run adds, against the run's fixture map, until a line starts with `sentinel`.
async fn run_package(run: PackageRun<'_>, src: &str, sentinel: &str) -> Vec<String> {
    run_package_with(run, src, sentinel, |_| |_: &str| None).await
}

/// [`run_package`], also running on each appended line the hook `hook` makes from the
/// session's mapper.
async fn run_package_with<H: FnMut(&str) -> Option<Reaction>>(
    run: PackageRun<'_>,
    src: &str,
    sentinel: &str,
    hook: impl FnOnce(Mapper) -> H,
) -> Vec<String> {
    let home = prepare_server(run.server);
    if let Some(user_module) = &run.user_module {
        std::fs::write(
            home.join(run.server).join("modules").join("user.ts"),
            user_module,
        )
        .expect("write the user module");
    }
    let spec = "smudgy://wbk/cartographer";
    shared_packages::install_package(run.server, spec, UpdateMode::Auto, true)
        .expect("install the package");
    let mut smudgy = SmudgyCapabilities {
        echo: true,
        mapper_read: true,
        mapper_write: true,
        ..Default::default()
    };
    (run.consent)(&mut smudgy);
    shared_packages::record_consent(
        run.server,
        spec,
        &PackagePermissions {
            smudgy,
            ..Default::default()
        },
    )
    .expect("record the consent");
    // Local copies land in a local tier beside the cloud.
    let local_root = home.join(format!("local-{}", run.server));
    let mapper = Mapper::new(
        Arc::new(smudgy_cloud::CompositeBackend::new(
            Arc::new(smudgy_cloud::LocalBackend::new(&local_root)),
            Arc::new(run.cloud),
        )),
        home.join(format!("map-{}", run.server)),
    );
    let resolved = package(src);
    let factory: PackageProviderFactory = Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        provider.insert(resolved.clone());
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    });
    let hook = hook(mapper.clone());
    run_until_with(
        spawn_with_package_provider(params(run.session, run.server, mapper), factory),
        sentinel,
        run.cues,
        hook,
    )
    .await
}

#[tokio::test]
async fn packages_without_secrets_see_the_map_alone() {
    let lines = run_package(
        PackageRun {
            session: 9_389,
            server: "MapperPlacesUncapable",
            cloud: PlacesCloud::new(),
            consent: |_| {},
            user_module: None,
            cues: Vec::new(),
        },
        &uncapable_module(),
        "PLACES_UNCAPABLE_",
    )
    .await;
    assert!(
        lines.iter().any(|line| line == "PLACES_UNCAPABLE_OK"),
        "a package without secrets reached past the map:\n{}",
        lines.join("\n")
    );
}

#[tokio::test]
async fn reading_secrets_neither_writes_nor_manages_them() {
    let lines = run_package(
        PackageRun {
            session: 9_390,
            server: "MapperPlacesReader",
            cloud: PlacesCloud::new(),
            consent: |caps| caps.secrets_read = true,
            user_module: None,
            cues: Vec::new(),
        },
        &reader_module(),
        "PLACES_READER_",
    )
    .await;
    assert!(
        lines.iter().any(|line| line == "PLACES_READER_OK"),
        "a reading package wrote or managed Secrets:\n{}",
        lines.join("\n")
    );
}

async fn write_tier_lines(session: u32, server: &str, write: bool) -> Vec<String> {
    let consent: fn(&mut SmudgyCapabilities) = if write {
        |caps| {
            caps.secrets_read = true;
            caps.secrets_write = true;
        }
    } else {
        |caps| caps.secrets_read = true
    };
    run_package(
        PackageRun {
            session,
            server,
            cloud: PlacesCloud::new(),
            consent,
            user_module: None,
            cues: Vec::new(),
        },
        &write_tier_module(!write),
        "PLACES_WRITE_TIER_",
    )
    .await
}

#[tokio::test]
async fn reading_secrets_never_writes_a_secrets_own_area() {
    let lines = write_tier_lines(9_396, "MapperPlacesReadTierWrites", false).await;
    assert!(
        lines.iter().any(|line| line == "PLACES_WRITE_TIER_OK"),
        "a reading package wrote through a Secret's own area:\n{}",
        lines.join("\n")
    );
}

#[tokio::test]
async fn writing_secrets_writes_a_secrets_own_area() {
    let lines = write_tier_lines(9_397, "MapperPlacesWriteTierWrites", true).await;
    assert!(
        lines.iter().any(|line| line == "PLACES_WRITE_TIER_OK"),
        "a writing package was refused a Secret's own area:\n{}",
        lines.join("\n")
    );
}

/// The probe's lines: everything it saw, in order.
fn probes(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.starts_with("PROBE "))
        .cloned()
        .collect()
}

/// Without `secrets`, nothing a package sees differs from the same map without
/// its Secrets and Private additions: the same probe, run against both, sees
/// the same thing line for line. Routes take the map's long way round rather
/// than the Bookcase's shortcut, a Secret's own area answers as an unknown id,
/// and every write naming it fails as a write to an unknown area does.
#[tokio::test]
async fn without_secrets_a_map_reads_as_if_it_had_none() {
    let with_secrets = run_package(
        PackageRun {
            session: 9_391,
            server: "MapperInvisibleWith",
            cloud: PlacesCloud::new(),
            consent: |_| {},
            user_module: None,
            cues: Vec::new(),
        },
        &probe_module(),
        "PROBE_DONE",
    )
    .await;
    let without = run_package(
        PackageRun {
            session: 9_392,
            server: "MapperInvisibleWithout",
            cloud: PlacesCloud::without_sources(),
            consent: |_| {},
            user_module: None,
            cues: Vec::new(),
        },
        &probe_module(),
        "PROBE_DONE",
    )
    .await;
    assert!(
        without.iter().any(|line| line == "PROBE_DONE"),
        "the probe finished:\n{}",
        without.join("\n")
    );
    let (seen, plain) = (probes(&with_secrets), probes(&without));
    assert!(plain.len() > 60, "the probe ran:\n{}", plain.join("\n"));
    for (seen, plain) in seen.iter().zip(&plain) {
        assert_eq!(seen, plain, "a package without secrets saw a difference");
    }
    assert_eq!(seen.len(), plain.len(), "{}", with_secrets.join("\n"));
    let route = plain
        .iter()
        .find(|line| line.starts_with("PROBE route map 1 to map 3: "))
        .expect("the route was probed");
    assert!(
        route.ends_with(&format!(
            r#"[["{MAP}",1],["{MAP}",4],["{MAP}",5],["{MAP}",3]]"#
        )),
        "the long way round: {route}"
    );
    let unknown = |label: &str| {
        plain
            .iter()
            .find(|line| line.starts_with(&format!("PROBE {label}: ")))
            .map(|line| line.split_once(": ").expect("a probe line").1.to_string())
            .expect("probed")
    };
    assert_eq!(
        unknown("getAreaById secret"),
        unknown("getAreaById unknown"),
        "a Secret's own area answers exactly as an unknown id"
    );
    assert_eq!(unknown("in secret"), unknown("in unknown"));
    assert_eq!(
        unknown("getSecretById secret"),
        unknown("getSecretById unknown")
    );
    // The map's rooms are 1, 3, 4 and 5; the Private den is 7.
    assert_eq!(unknown("nextRoomNumber"), "6", "the map's own rooms alone");
    assert_eq!(unknown("createRoom on the map"), "6");
    assert_eq!(unknown("nextRoomNumber after createRoom"), "7");
    assert_eq!(unknown("mutateArea createRoom on the map"), "[7,8]");
    assert_eq!(unknown("nextRoomNumber after mutateArea"), "9");
}

/// Everything a package can ask about the Road, whose milestone may hold an exit into
/// the Bookcase's room on the Library map, one `PROBE` line each.
fn road_probe_module() -> String {
    format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";

const S = "{BOOKCASE}";
const probe = async (label, run) => {{
    let seen;
    try {{
        seen = JSON.stringify(await run()) ?? "undefined";
    }} catch (error) {{
        seen = `!! ${{error?.constructor?.name}}: ${{error?.message ?? error}}`;
    }}
    echo(`PROBE ${{label}}: ${{seen}}`);
}};
const where = (room) => (room ? `${{room.areaId}}:${{room.roomNumber}}` : "none");

await mapper.ready();
const road = mapper.getAreaById("{ROAD}");
const milestone = road.room(1);
await probe("exits", () => milestone.exits.map((exit) => ({{
    id: exit.id, to: [exit.toAreaId, exit.toRoomNumber], door: exit.door, toRoom: where(exit.toRoom),
}})));
await probe("map exits", () => milestone.in("map").exits?.length ?? "no view");
await probe("connections", () => road.connections.map((link) => link.id));
await probe("export", async () => {{
    const exported = await mapper.exportArea(road);
    return {{
        exits: exported.rooms.map((room) => room.exits.map((exit) => exit.id)),
        connections: exported.connections.map((link) => link.id),
        linked: exported.linked_areas ?? [],
        sources: exported.sources ?? [],
    }};
}});
await probe("secret's area", () => mapper.getAreaById(S).id);
await probe("route into the secret", () => mapper.getPathBetweenRooms(road.id, 1, S, 2));
await probe("route to the tollgate", () => mapper.getPathBetweenRooms(road.id, 1, road.id, 2));
await probe("nearest vault", () => where(mapper.findNearestRoomWithTag(milestone, "vault")));
await probe("nearest in the secret", () => where(mapper.findNearestRoomInArea(milestone, S)));
await probe("visible exits east", () => mapper.listRoomsByTitleDescriptionAndVisibleExits("Milestone", "", ["East"]).map(where));
await probe("visible exits east north", () => mapper.listRoomsByTitleDescriptionAndVisibleExits("Milestone", "", ["East", "North"]).map(where));
await probe("tag mile", () => mapper.findRoomsWithTag("mile").map(where));
await probe("areas", () => mapper.areas.map((area) => area.id).sort());
echo("PROBE_DONE");
"#
    )
}

/// An exit into another map's Secret room leaves no trace for a package without
/// `secrets`: room exits, connections, `exportArea` (with the `linked_areas` entry only
/// it makes), lookups by visible exits, searches and routes read exactly as on the same
/// maps where the exit was never served. A package with `secrets` read sees it, reads
/// it under the Secret's own area, and routes through it.
#[tokio::test]
async fn without_secrets_exit_content_remains_visible_but_its_destination_is_redacted() {
    let module = road_probe_module();
    let run = |session: u32,
               server: &'static str,
               cloud: PlacesCloud,
               consent: fn(&mut SmudgyCapabilities)| {
        run_package(
            PackageRun {
                session,
                server,
                cloud,
                consent,
                user_module: None,
                cues: Vec::new(),
            },
            &module,
            "PROBE_DONE",
        )
    };
    let with_secret = run(
        9_411,
        "MapperForeignWith",
        PlacesCloud::new().with_road(true),
        |_| {},
    )
    .await;
    let without = run(
        9_412,
        "MapperForeignWithout",
        PlacesCloud::without_sources().with_road(false),
        |_| {},
    )
    .await;
    let (seen, plain) = (probes(&with_secret), probes(&without));
    assert!(plain.len() >= 13, "the probe ran:\n{}", without.join("\n"));
    let line = |lines: &[String], label: &str| {
        lines
            .iter()
            .find(|line| line.starts_with(&format!("PROBE {label}: ")))
            .map(|line| line.split_once(": ").expect("a probe line").1.to_string())
            .expect("probed")
    };
    assert!(
        line(&plain, "export").contains(r#""linked":[]"#),
        "{}",
        line(&plain, "export")
    );

    let exits = line(&seen, "exits");
    assert!(
        exits.contains("00000000-0000-0000-0000-000000000052"),
        "{exits}"
    );
    assert!(exits.contains(r#""to":[null,null]"#), "{exits}");
    assert!(exits.contains(r#""opensWith":"lift grate""#), "{exits}");
    assert!(!exits.contains(&BOOKCASE.to_string()), "{exits}");
    assert!(line(&seen, "connections").contains("00000000-0000-0000-0000-000000000152"));
    assert!(line(&seen, "export").contains("00000000-0000-0000-0000-000000000052"));
    for label in [
        "route into the secret",
        "nearest vault",
        "nearest in the secret",
        "areas",
    ] {
        assert_eq!(line(&seen, label), line(&plain, label), "{label}");
    }
    let reader = run(
        9_413,
        "MapperForeignReader",
        PlacesCloud::new().with_road(true),
        |caps| caps.secrets_read = true,
    )
    .await;
    let reader = probes(&reader);
    let exits = line(&reader, "exits");
    assert!(
        exits.contains(&format!(r#""to":["{BOOKCASE}",2]"#)),
        "the reader reads it under the Secret's own area: {exits}"
    );
    assert!(exits.contains(r#""opensWith":"lift grate""#), "{exits}");
    assert!(
        exits.contains(&format!(r#""toRoom":"{BOOKCASE}:2""#)),
        "{exits}"
    );
    let route = line(&reader, "route into the secret");
    assert!(
        route.ends_with(&format!(r#"["{BOOKCASE}",2]]"#)),
        "the route passes into the Secret: {route}"
    );
    assert!(line(&reader, "connections").contains("00000000-0000-0000-0000-000000000152"));
}

/// Standing in a Secret's or the Private additions' own room, a package without
/// `secrets` stands somewhere unmapped on their map: the location reads, and the move
/// announces, the map with no room, exactly as a move into an unmapped room of the map
/// does. The user's own scripts still see the Secret's room.
#[tokio::test]
async fn without_secrets_a_secrets_room_is_somewhere_unmapped() {
    let package = r#"
import { echo, mapper } from "smudgy:core";
import { room as moved } from "smudgy:events/map";

await mapper.ready();
let count = 0;
moved.on((payload) => {
    echo(`PKG_MOVE ${JSON.stringify(payload)} ${JSON.stringify(mapper.getCurrentLocation() ?? null)}`);
    if (++count === 4) echo("PKG_DONE");
});
echo("PKG_READY");
"#;
    let user = r#"
import { echo, mapper } from "smudgy:core";
import { room as moved } from "smudgy:events/map";

moved.on((payload) => {
    echo(`USER_MOVE ${JSON.stringify(payload)} ${JSON.stringify(mapper.getCurrentLocation() ?? null)}`);
});
"#;
    let lines = run_package(
        PackageRun {
            session: 9_393,
            server: "MapperInvisibleLocation",
            cloud: PlacesCloud::new(),
            consent: |caps| caps.interop_read = true,
            user_module: Some(user.to_string()),
            cues: vec![(
                "PKG_READY",
                vec![
                    RuntimeAction::SetCurrentLocation(AreaId(BOOKCASE), Some(2)),
                    RuntimeAction::SetCurrentLocation(AreaId(MAP), None),
                    RuntimeAction::SetCurrentLocation(private_area(), Some(7)),
                    RuntimeAction::SetCurrentLocation(AreaId(MAP), Some(1)),
                ],
            )],
        },
        package,
        "PKG_DONE",
    )
    .await;
    let moves = |prefix: &str| -> Vec<&String> {
        lines
            .iter()
            .filter(|line| line.starts_with(prefix))
            .collect()
    };
    let (package_moves, user_moves) = (moves("PKG_MOVE"), moves("USER_MOVE"));
    assert_eq!(package_moves.len(), 4, "{}", lines.join("\n"));
    assert_eq!(
        package_moves[0], package_moves[1],
        "a Secret's room reads as an unmapped room of its map"
    );
    assert_eq!(
        package_moves[2], package_moves[1],
        "so does a room of the Private additions"
    );
    let unmapped = format!(r#"PKG_MOVE {{"areaId":"{MAP}","roomNumber":null}} {{"area":"{MAP}"}}"#);
    assert_eq!(*package_moves[1], unmapped);
    assert_eq!(
        *package_moves[3],
        format!(r#"PKG_MOVE {{"areaId":"{MAP}","roomNumber":1}} {{"area":"{MAP}","room":1}}"#)
    );
    assert_eq!(
        *user_moves[0],
        format!(
            r#"USER_MOVE {{"areaId":"{BOOKCASE}","roomNumber":2}} {{"area":"{BOOKCASE}","room":2}}"#
        ),
        "the user's scripts see the Secret's room: {}",
        lines.join("\n")
    );
}

/// A location a package without `secrets` sets in a Secret's room reads back as it set it,
/// and only while the session stands there: when the player later walks into that room
/// again, it reads as somewhere unmapped like any other.
#[tokio::test]
async fn without_secrets_a_location_set_in_a_secret_exempts_nothing_after() {
    let package = format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
import {{ room as moved }} from "smudgy:events/map";

await mapper.ready();
let count = 0;
moved.on((payload) => {{
    echo(`PKG_MOVE ${{JSON.stringify(payload)}} ${{JSON.stringify(mapper.getCurrentLocation() ?? null)}}`);
    if (++count === 3) echo("PKG_DONE");
}});
mapper.setCurrentLocation("{BOOKCASE}", 2);
echo(`PKG_SET ${{JSON.stringify(mapper.getCurrentLocation() ?? null)}}`);
echo("PKG_READY");
"#
    );
    let lines = run_package(
        PackageRun {
            session: 9_394,
            server: "MapperNamedLocation",
            cloud: PlacesCloud::new(),
            consent: |caps| caps.interop_read = true,
            user_module: None,
            cues: vec![(
                "PKG_READY",
                vec![
                    RuntimeAction::SetCurrentLocation(AreaId(MAP), Some(1)),
                    RuntimeAction::SetCurrentLocation(AreaId(BOOKCASE), Some(2)),
                ],
            )],
        },
        &package,
        "PKG_DONE",
    )
    .await;
    let all = lines.join(
        "
",
    );
    assert!(
        lines
            .iter()
            .any(|line| *line == format!(r#"PKG_SET {{"area":"{BOOKCASE}","room":2}}"#)),
        "the package reads back what it set:
{all}"
    );
    let moves: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("PKG_MOVE"))
        .collect();
    assert_eq!(
        moves.last().map(|line| line.as_str()),
        Some(
            format!(r#"PKG_MOVE {{"areaId":"{MAP}","roomNumber":null}} {{"area":"{MAP}"}}"#)
                .as_str()
        ),
        "the player's own return to the Secret's room reads as somewhere unmapped:
{all}"
    );
}

/// The `map:click` payloads a package given `consent` beyond the map received, and those the
/// user's own scripts received, for clicks on the Bookcase's own room 2, the Private additions'
/// room 7, map room 3 and, last, map room 1.
async fn clicks_seen(
    session: u32,
    server: &str,
    consent: fn(&mut SmudgyCapabilities),
) -> (Vec<String>, Vec<String>) {
    let package = r#"
import { echo, mapper } from "smudgy:core";
import { click } from "smudgy:events/map";

await mapper.ready();
click.on((payload) => {
    echo(`PKG_CLICK ${JSON.stringify(payload)}`);
    if (payload.roomNumber === 1) echo("PKG_DONE");
});
echo("PKG_READY");
"#;
    let user = r#"
import { echo } from "smudgy:core";
import { click } from "smudgy:events/map";

click.on((payload) => echo(`USER_CLICK ${JSON.stringify(payload)}`));
"#;
    let clicked = |area_id: AreaId, room_number: i32| RuntimeAction::MapRoomClicked {
        area_id,
        room_number,
    };
    let lines = run_package(
        PackageRun {
            session,
            server,
            cloud: PlacesCloud::new(),
            consent,
            user_module: Some(user.to_string()),
            cues: vec![(
                "PKG_READY",
                vec![
                    clicked(AreaId(BOOKCASE), 2),
                    clicked(private_area(), 7),
                    clicked(AreaId(MAP), 3),
                    clicked(AreaId(MAP), 1),
                ],
            )],
        },
        package,
        "PKG_DONE",
    )
    .await;
    assert!(
        lines.iter().any(|line| line == "PKG_DONE"),
        "the clicks arrived:\n{}",
        lines.join("\n")
    );
    let seen = |prefix: &str| -> Vec<String> {
        lines
            .iter()
            .filter(|line| line.starts_with(prefix))
            .cloned()
            .collect()
    };
    (seen("PKG_CLICK "), seen("USER_CLICK "))
}

/// A click on a room in a session map reaches a package without `secrets` only for the map's
/// own rooms: a click on a Secret's or the Private additions' own room never fires for it, as
/// on a map without them, where nothing is there to click. The user's scripts, and a package
/// reading Secrets, hear every click, the place's room named by its own area.
#[tokio::test]
async fn without_secrets_a_click_on_a_secrets_room_is_never_heard() {
    let click = |area: Uuid, room: i32| format!(r#"{{"areaId":"{area}","roomNumber":{room}}}"#);
    let (package, user) = clicks_seen(9_784, "MapperClickWithout", |caps| {
        caps.interop_read = true;
    })
    .await;
    assert_eq!(
        package,
        vec![
            format!("PKG_CLICK {}", click(MAP, 3)),
            format!("PKG_CLICK {}", click(MAP, 1)),
        ],
        "only the map's own rooms reach a package without secrets"
    );
    assert_eq!(
        user.first(),
        Some(&format!("USER_CLICK {}", click(BOOKCASE, 2))),
        "the user's scripts hear the Secret's room: {user:?}"
    );
    assert!(
        user.get(1)
            .is_some_and(|line| line.contains(&private_area().to_string())),
        "and the Private additions' room: {user:?}"
    );

    let (reader, _) = clicks_seen(9_785, "MapperClickReader", |caps| {
        caps.interop_read = true;
        caps.secrets_read = true;
    })
    .await;
    assert_eq!(reader.len(), 4, "a reader hears every click: {reader:?}");
    assert_eq!(reader[0], format!("PKG_CLICK {}", click(BOOKCASE, 2)));
}

/// One room moved from `from` room `old` into `into` as room `new`.
fn merged(from: AreaId, old: i32, into: AreaId, new: i32) -> RuntimeAction {
    RuntimeAction::MapperRoomsMerged {
        into,
        rooms: vec![smudgy_cloud::RoomRemap {
            from: smudgy_cloud::mapper::RoomKey::new(from, smudgy_cloud::RoomNumber(old)),
            to: smudgy_cloud::RoomNumber(new),
        }]
        .into(),
    }
}

/// The `map:merged` payloads a package received, given `consent` beyond the map, for three
/// merges: map room 2 into the Bookcase, the Bookcase's room 2 into the map, and the Bookcase's
/// room 1 with map room 4 into the map together. The last ends the run.
async fn merges_seen(
    session: u32,
    server: &str,
    consent: fn(&mut SmudgyCapabilities),
) -> Vec<String> {
    let package = r#"
import { echo, mapper } from "smudgy:core";
import { merged } from "smudgy:events/map";

await mapper.ready();
merged.on((payload) => {
    echo(`PKG_MERGED ${JSON.stringify(payload)}`);
    if (payload.rooms.some((moved) => moved.to === 10)) echo("PKG_DONE");
});
echo("PKG_READY");
"#;
    let mut both = merged(AreaId(BOOKCASE), 1, AreaId(MAP), 9);
    if let RuntimeAction::MapperRoomsMerged { rooms, .. } = &mut both {
        let mut all = rooms.to_vec();
        all.push(smudgy_cloud::RoomRemap {
            from: smudgy_cloud::mapper::RoomKey::new(AreaId(MAP), smudgy_cloud::RoomNumber(4)),
            to: smudgy_cloud::RoomNumber(10),
        });
        *rooms = all.into();
    }
    let lines = run_package(
        PackageRun {
            session,
            server,
            cloud: PlacesCloud::new(),
            consent,
            user_module: None,
            cues: vec![(
                "PKG_READY",
                vec![
                    merged(AreaId(MAP), 2, AreaId(BOOKCASE), 5),
                    merged(AreaId(BOOKCASE), 2, AreaId(MAP), 6),
                    both,
                ],
            )],
        },
        package,
        "PKG_DONE",
    )
    .await;
    assert!(
        lines.iter().any(|line| line == "PKG_DONE"),
        "the merges arrived:
{}",
        lines.join(
            "
"
        )
    );
    lines
        .into_iter()
        .filter(|line| line.starts_with("PKG_MERGED "))
        .collect()
}

/// Without `secrets`, rooms moved into a Secret read as rooms leaving the map and rooms moved
/// out of one as rooms joining it: no remap names the Secret, and a merge left with none is not
/// delivered at all. With `secrets` read, every merge arrives as it is.
#[tokio::test]
async fn without_secrets_merges_never_name_a_place() {
    let seen = merges_seen(9_777, "MapperMergedWithout", |caps| {
        caps.interop_read = true;
    })
    .await;
    assert_eq!(
        seen,
        vec![format!(
            r#"PKG_MERGED {{"into":"{MAP}","rooms":[{{"from":{{"area":"{MAP}","room":4}},"to":10}}]}}"#
        )],
        "only the map's own remap reaches a package without secrets"
    );
    let read = merges_seen(9_778, "MapperMergedReader", |caps| {
        caps.interop_read = true;
        caps.secrets_read = true;
    })
    .await;
    assert_eq!(read.len(), 3, "a reader sees every merge: {read:?}");
    assert!(
        read[0].contains(&format!(r#""into":"{BOOKCASE}""#)),
        "{read:?}"
    );
    assert!(
        read[2].contains(&format!(r#""area":"{BOOKCASE}","room":1"#)),
        "{read:?}"
    );
}

/// A Secret that leaves the atlas while the user stands in one of its rooms stays hidden from a
/// package without `secrets`: the location and every later move into it read as somewhere
/// unmapped on its map, never as the Secret's id.
#[tokio::test]
async fn without_secrets_a_gone_secret_stays_hidden() {
    let package = r#"
import { echo, mapper } from "smudgy:core";
import { room as moved } from "smudgy:events/map";

await mapper.ready();
const look = () => JSON.stringify(mapper.getCurrentLocation() ?? null);
let last = "";
const poll = setInterval(() => {
    const seen = look();
    if (seen !== last) echo(`PKG_LOCATION ${seen}`);
    last = seen;
}, 5);
let count = 0;
moved.on((payload) => {
    echo(`PKG_MOVE ${JSON.stringify(payload)} ${look()}`);
    if (++count === 2) {
        clearInterval(poll);
        echo("PKG_DONE");
    }
});
echo("PKG_READY");
"#;
    let user = r#"
import { echo, mapper } from "smudgy:core";
import { room as moved } from "smudgy:events/map";

moved.on((payload) => {
    echo(`USER_MOVE ${JSON.stringify(payload)} ${JSON.stringify(mapper.getCurrentLocation())}`);
});
"#;
    // The Bookcase goes (revoked or deleted elsewhere) once the user stands in it; a while
    // later the user moves within it again.
    let lines = run_package_with(
        PackageRun {
            session: 9_779,
            server: "MapperGoneSecret",
            cloud: PlacesCloud::new(),
            consent: |caps| caps.interop_read = true,
            user_module: Some(user.to_string()),
            cues: vec![(
                "PKG_READY",
                vec![RuntimeAction::SetCurrentLocation(AreaId(BOOKCASE), Some(2))],
            )],
        },
        package,
        "PKG_DONE",
        |mapper| {
            let mut gone = false;
            move |line: &str| {
                if gone || !line.starts_with("USER_MOVE ") {
                    return None;
                }
                gone = true;
                let mapper = mapper.clone();
                let reaction: Reaction = Box::pin(async move {
                    mapper
                        .delete_secret(AreaId(MAP), &SourceId::Secret(BOOKCASE))
                        .await
                        .expect("the Bookcase goes");
                    assert!(
                        mapper
                            .get_current_atlas()
                            .get_area(&AreaId(BOOKCASE))
                            .is_none()
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    vec![RuntimeAction::SetCurrentLocation(AreaId(BOOKCASE), Some(3))]
                });
                Some(reaction)
            }
        },
    )
    .await;
    let all = lines.join("\n");
    assert!(lines.iter().any(|line| line == "PKG_DONE"), "{all}");
    assert!(
        lines.iter().any(|line| line.starts_with("USER_MOVE ")
            && line.ends_with(&format!(r#"{{"area":"{BOOKCASE}","room":3}}"#))),
        "the user's scripts still see where the user stands:\n{all}"
    );
    let package_lines: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("PKG_"))
        .collect();
    assert!(
        package_lines
            .iter()
            .all(|line| !line.contains(&BOOKCASE.to_string())),
        "a package without secrets saw the gone Secret's id:\n{all}"
    );
    let unmapped = format!(r#"{{"areaId":"{MAP}","roomNumber":null}} {{"area":"{MAP}"}}"#);
    let moves: Vec<&&String> = package_lines
        .iter()
        .filter(|line| line.starts_with("PKG_MOVE "))
        .collect();
    assert_eq!(moves.len(), 2, "{all}");
    assert!(
        moves.iter().all(|line| line.ends_with(&unmapped)),
        "both moves read as somewhere unmapped on the map:\n{all}"
    );
}

/// A trusted script's `map:room` handler awaits `secret.delete()` for the Secret the user
/// stands in. The delete resolves; what the handler then throws (the gone Secret's own area is
/// an unknown area now) reaches the session's output like any uncaught error, though the
/// session sat idle while the delete was in flight.
#[tokio::test]
async fn an_awaited_secret_delete_resolves_and_a_later_throw_is_reported() {
    let package = r#"
import { echo, mapper } from "smudgy:core";
import { room as moved } from "smudgy:events/map";

await mapper.ready();
const look = () => JSON.stringify(mapper.getCurrentLocation() ?? null);
let last = "";
setInterval(() => {
    const seen = look();
    if (seen !== last) echo(`PKG_LOCATION ${seen}`);
    last = seen;
}, 5);
moved.on((payload) => echo(`PKG_MOVE ${JSON.stringify(payload)}`));
echo("PKG_READY");
"#;
    let user = format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
import {{ room as moved }} from "smudgy:events/map";

let deleted = false;
moved.on(async (payload) => {{
    if (payload.areaId !== "{BOOKCASE}" || deleted) return;
    deleted = true;
    await mapper.getAreaById("{MAP}").secrets.get("{BOOKCASE}").delete();
    echo(`USER_DELETED ${{mapper.getAreaById("{MAP}").secrets.exists("{BOOKCASE}")}}`);
    mapper.getAreaById("{BOOKCASE}");
}});
"#
    );
    let lines = run_package(
        PackageRun {
            session: 9_781,
            server: "MapperDeleteInHandler",
            cloud: PlacesCloud::new(),
            consent: |caps| caps.interop_read = true,
            user_module: Some(user),
            cues: vec![(
                "PKG_READY",
                vec![RuntimeAction::SetCurrentLocation(AreaId(BOOKCASE), Some(2))],
            )],
        },
        package,
        "USER_DELETED",
    )
    .await;
    let all = lines.join("\n");
    assert!(
        lines.iter().any(|line| line == "USER_DELETED false"),
        "the awaited delete resolved:\n{all}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Area not found")),
        "the handler's later throw was reported:\n{all}"
    );
}

/// The server's projection lists in `linked_areas` the maps every readable
/// source's exits lead into (smudgy-cloudflare src/library/graph/projection.ts
/// exitView runs for every bundle). Here the Bookcase keeps a hidden exit on map
/// room 3 into "Road" room 2, so the map's `linked_areas` names Road although no
/// map exit leads there.
fn with_bookcase_exit_into_road(cloud: PlacesCloud) -> PlacesCloud {
    {
        let mut details = cloud.details.lock().unwrap();
        let bookcase = details
            .sources
            .iter_mut()
            .find(|bundle| bundle.source == SourceId::Secret(BOOKCASE))
            .expect("the Bookcase");
        let exit: smudgy_cloud::Exit = serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(0x61), "from_direction": "West",
            "to_area_id": ROAD, "to_room_number": 2,
            "to_direction": null, "to_unknown": false, "path": "", "command": "",
            "weight": 1.0, "connection_id": Uuid::from_u128(0x161),
            "is_hidden": true, "door": null
        }))
        .expect("the exit parses");
        let entry = bookcase
            .room_data
            .iter_mut()
            .find(|entry| entry.room_number == RoomNumber(3))
            .expect("Bookcase data on room 3");
        entry.exits.push(exit);
        bookcase.connections.push(
            serde_json::from_value(serde_json::json!({
                "id": Uuid::from_u128(0x161),
                "endpoint_a": { "room_number": 3, "side": "West", "port_offset": 0.5, "port_mode": "AutoPinned" },
                "kind": "External", "routing": "Simple", "segment_shape": "Direct",
                "corner": "Sharp", "route_points": [], "dash": "Solid", "color": "#A4A4A4",
                "thickness": 1.0
            }))
            .expect("the connection parses"),
        );
        details.linked_areas = serde_json::from_value(serde_json::json!([
            { "to_area_id": ROAD, "name": "Road", "visible": true }
        ]))
        .expect("linked areas parse");
    }
    cloud
}

/// Without `secrets`, an export's `linked_areas` names only the maps the map's own exits
/// lead into: the Bookcase's exit into Road leaves no entry behind.
#[tokio::test]
async fn without_secrets_an_exports_linked_areas_are_the_maps_own() {
    let module = format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
await mapper.ready();
const exported = await mapper.exportArea(mapper.getAreaById("{MAP}"));
echo(`PROBE linked: ${{JSON.stringify(exported.linked_areas ?? [])}}`);
echo(`PROBE sources: ${{JSON.stringify(exported.sources ?? [])}}`);
echo("PROBE_DONE");
"#
    );
    let run = |session: u32, server: &'static str, cloud: PlacesCloud| {
        run_package(
            PackageRun {
                session,
                server,
                cloud,
                consent: |_| {},
                user_module: None,
                cues: Vec::new(),
            },
            &module,
            "PROBE_DONE",
        )
    };
    let with_secret = run(
        9_871,
        "ExportLinkedWith",
        with_bookcase_exit_into_road(PlacesCloud::new()),
    )
    .await;
    let plain = run(9_872, "ExportLinkedWithout", PlacesCloud::without_sources()).await;
    let (seen, plain) = (probes(&with_secret), probes(&plain));
    eprintln!("WITH SECRET: {seen:#?}\nPLAIN: {plain:#?}");
    assert_eq!(
        seen, plain,
        "a package without secrets learned of the Bookcase's exit into Road"
    );
}

/// Probes a package's copies and exports of the Library map and the Road. The ids of the
/// copies differ by run, so they read as `ROAD_COPY` and `MAP_COPY`.
fn copies_probe_module() -> String {
    format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";

let roadCopy;
let mapCopy;
const probe = async (label, run) => {{
    let seen;
    try {{
        seen = JSON.stringify(await run()) ?? "undefined";
    }} catch (error) {{
        seen = `!! ${{error?.constructor?.name}}: ${{error?.message ?? error}}`;
    }}
    for (const [copy, name] of [[roadCopy, "ROAD_COPY"], [mapCopy, "MAP_COPY"]]) {{
        if (copy?.id) seen = seen.replaceAll(copy.id, name);
    }}
    echo(`PROBE ${{label}}: ${{seen}}`);
}};
const exitsOf = (area) => area.roomNumbers.map((n) => [n, area.room(n).exits.map((exit) => ({{
    dir: exit.fromDirection, to: [exit.toAreaId ?? null, exit.toRoomNumber ?? null],
    door: exit.door, hidden: exit.isHidden,
}}))]);

await mapper.ready();
const map = mapper.getAreaById("{MAP}");
const road = mapper.getAreaById("{ROAD}");
await probe("map connections", () => map.connections.map((link) => link.id));
await probe("map labels", () => map.labels.length);
await probe("map shapes", () => map.shapes.length);
await probe("findAreasWithProperty summary", () => mapper.findAreasWithProperty("summary"));
await probe("findAreasByProperty summary", () => mapper.findAreasByProperty("summary", "Hidden room"));
await probe("map storage", () => map.storage);
await probe("export map full", async () => {{
    const e = await mapper.exportArea(map);
    return {{ rooms: e.rooms, connections: e.connections, linked: e.linked_areas ?? [], props: e.properties, labels: e.labels, shapes: e.shapes }};
}});
await probe("export road full", async () => {{
    const e = await mapper.exportArea(road);
    return {{ rooms: e.rooms, connections: e.connections, linked: e.linked_areas ?? [] }};
}});
// A local copy of the Road, read back.
await probe("copy road to local", async () => {{
    roadCopy = await mapper.copyArea(road, {{ storage: "local" }});
    return typeof roadCopy?.id;
}});
await probe("road copy exits", () => exitsOf(mapper.getAreaById(roadCopy.id)));
await probe("road copy connections", () => mapper.getAreaById(roadCopy.id).connections.length);
await probe("road copy export", async () => {{
    const e = await mapper.exportArea(roadCopy.id);
    return {{ exits: e.rooms.map((room) => room.exits.map((exit) => [exit.from_direction, exit.to_area_id ?? null, exit.to_room_number ?? null, exit.door ?? null, exit.to_source ?? null])), connections: e.connections.length, sources: (e.sources ?? []).length }};
}});
// A local copy of the Library map, read back.
await probe("copy map to local", async () => {{
    mapCopy = await mapper.copyArea(map, {{ storage: "local" }});
    return typeof mapCopy?.id;
}});
await probe("map copy places", () => mapper.getAreaById(mapCopy.id).places);
await probe("map copy rooms", () => mapper.getAreaById(mapCopy.id).roomNumbers);
await probe("map copy exits", () => exitsOf(mapper.getAreaById(mapCopy.id)));
await probe("map copy tags", () => mapper.getAreaById(mapCopy.id).roomNumbers.map((n) => mapper.getAreaById(mapCopy.id).room(n).tags));
await probe("map copy export", async () => {{
    const e = await mapper.exportArea(mapCopy.id);
    return {{ rooms: e.rooms.map((room) => [room.room_number, room.exits.length, room.tags]), connections: e.connections.length, sources: (e.sources ?? []).length }};
}});
await probe("areas after copies", () => mapper.areas.length);
echo("PROBE_DONE");
"#
    )
}

async fn copies_run(
    session: u32,
    server: &'static str,
    cloud: PlacesCloud,
    consent: fn(&mut SmudgyCapabilities),
) -> Vec<String> {
    run_package(
        PackageRun {
            session,
            server,
            cloud,
            consent,
            user_module: None,
            cues: Vec::new(),
        },
        &copies_probe_module(),
        "PROBE_DONE",
    )
    .await
}

/// A package without `secrets` sees the same thing with and without the Secrets: copies,
/// exports, connections and area searches included. A local copy carries no exit into
/// another map's Secret room, whoever makes it, and of the map's Secrets only those the
/// copier may copy: here the map's owner keeps its owner Secret, the Bookcase, as a Secret of
/// the copy under a new id, and the Ledger, a Clan Secret without `copy`, stays behind.
#[tokio::test]
async fn without_secrets_copies_and_exports_read_as_if_there_were_none() {
    let with_secret = copies_run(
        9_811,
        "CopiesWith",
        PlacesCloud::new().with_road(true),
        |_| {},
    )
    .await;
    let without = copies_run(
        9_812,
        "CopiesWithout",
        PlacesCloud::without_sources().with_road(true),
        |_| {},
    )
    .await;
    let (seen, plain) = (probes(&with_secret), probes(&without));
    for line in &seen {
        println!("WITH    {line}");
    }
    for line in &plain {
        println!("WITHOUT {line}");
    }
    let reader = copies_run(
        9_813,
        "CopiesReader",
        PlacesCloud::new().with_road(true),
        |caps| caps.secrets_read = true,
    )
    .await;
    let reader = probes(&reader);
    for line in &reader {
        println!("READER  {line}");
    }
    assert!(
        plain
            .iter()
            .any(|l| l.starts_with("PROBE areas after copies")),
        "ran:\n{}",
        without.join("\n")
    );
    let road_copy = reader
        .iter()
        .find(|line| line.starts_with("PROBE road copy export: "))
        .expect("the reader exported the Road's copy");
    assert!(
        !road_copy.contains(&BOOKCASE.to_string()) && road_copy.contains("North"),
        "a local copy preserves the exit but carries no original Secret identity: {road_copy}"
    );
    let places = reader
        .iter()
        .find(|line| line.starts_with("PROBE map copy places: "))
        .expect("the reader listed the copy's places");
    assert_eq!(
        places.matches("\"id\"").count(),
        1,
        "the copy keeps one Secret: {places}"
    );
    assert!(
        !places.contains(&BOOKCASE.to_string()) && !places.contains(&LEDGER.to_string()),
        "the copy's Secret has a new id and the Ledger stays behind: {places}"
    );
    assert!(
        reader
            .iter()
            .any(|line| line.starts_with("PROBE map copy export: ")
                && line.ends_with(r#""sources":2}"#)),
        "the copy's export carries the Bookcase's copy and the Private additions: {reader:#?}"
    );
    let mut diffs = Vec::new();
    for (s, p) in seen.iter().zip(&plain) {
        if s != p {
            diffs.push(format!("WITH:    {s}\nWITHOUT: {p}"));
        }
    }
    assert!(diffs.is_empty(), "differences:\n{}", diffs.join("\n\n"));
}

fn door_module() -> String {
    format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";

const probe = async (label, run) => {{
    let seen;
    try {{
        seen = JSON.stringify(await run()) ?? "undefined";
    }} catch (error) {{
        seen = `!! ${{error?.constructor?.name}}: ${{error?.message ?? error}}`;
    }}
    echo(`PROBE ${{label}}: ${{seen}}`);
}};
const DOOR = "00000000-0000-0000-0000-000000000021";
const DEN = "00000000-0000-0000-0000-000000000041";
const live = () => mapper.getAreaById("{MAP}");
const doorOf = (n, id) => live().room(n).exits.filter((exit) => exit.id === id).map((exit) => ({{ place: typeof exit.place === "string" ? exit.place : exit.place?.name, weight: exit.weight }}));
await mapper.ready();
await probe("door before", () => doorOf(1, DOOR));
await probe("setRoomExit map door", () => mapper.setRoomExit("{MAP}", 1, DOOR, {{ weight: 9 }}));
await probe("door after set", () => doorOf(1, DOOR));
await probe("mutateArea map door", () => mapper.mutateArea("{MAP}", (m) => m.setRoomExit(1, DOOR, {{ weight: 3 }})));
await probe("door after mutate", () => doorOf(1, DOOR));
await probe("in map setRoomExit", () => live().in("map").room ? "has" : "none");
await probe("setRoomExit private den door", () => mapper.setRoomExit("{MAP}", 3, DEN, {{ weight: 4 }}));
await probe("den after set", () => doorOf(3, DEN));
await probe("deleteRoomExit map door", () => mapper.deleteRoomExit("{MAP}", 1, DOOR));
await probe("door after delete", () => doorOf(1, DOOR));
echo("PROBE_DONE");
"#
    )
}

async fn door_run(
    session: u32,
    server: &'static str,
    cloud: PlacesCloud,
    consent: fn(&mut SmudgyCapabilities),
) -> Vec<String> {
    run_package(
        PackageRun {
            session,
            server,
            cloud,
            consent,
            user_module: None,
            cues: Vec::new(),
        },
        &door_module(),
        "PROBE_DONE",
    )
    .await
}

/// A write by id through the map naming a hidden door that a Secret (or the Private
/// additions) keeps on a map room writes that place. With `secrets: ["read"]` it is refused
/// for `secrets-write` and nothing changes; without `secrets` it answers as on the same map
/// without the door, as an exit that does not exist.
#[tokio::test]
async fn reading_secrets_never_writes_a_secrets_door_through_the_map() {
    let reader = door_run(9_821, "DoorReader", PlacesCloud::new(), |caps| {
        caps.secrets_read = true;
    })
    .await;
    for line in probes(&reader) {
        println!("READER  {line}");
    }
    let none_with = door_run(9_822, "DoorNoneWith", PlacesCloud::new(), |_| {}).await;
    let none_without = door_run(
        9_823,
        "DoorNoneWithout",
        PlacesCloud::without_sources(),
        |_| {},
    )
    .await;
    for line in probes(&none_with) {
        println!("NOCAP-WITH    {line}");
    }
    for line in probes(&none_without) {
        println!("NOCAP-WITHOUT {line}");
    }
    assert_eq!(
        probes(&none_with),
        probes(&none_without),
        "without secrets, a write naming a hidden door answers as for an unknown exit"
    );
    let reader = probes(&reader);
    for write in [
        "setRoomExit map door",
        "mutateArea map door",
        "setRoomExit private den door",
        "deleteRoomExit map door",
    ] {
        assert!(
            reader
                .iter()
                .any(|line| line.starts_with(&format!("PROBE {write}: "))
                    && line.contains("secrets-write")),
            "{write} is refused for secrets-write: {reader:#?}"
        );
    }
    let wrote: Vec<&String> = reader
        .iter()
        .filter(|line| line.starts_with("PROBE door after") || line.starts_with("PROBE den after"))
        .collect();
    assert!(
        reader
            .iter()
            .any(|l| l == r#"PROBE door before: [{"place":"Bookcase","weight":1}]"#),
        "fixture: {reader:#?}"
    );
    assert!(
        wrote.iter().all(|line| line.ends_with(r#""weight":1}]"#)),
        "a package with secrets read only wrote a Secret's door through the map: {wrote:#?}"
    );
}

/// A map-owned exit and connection remain writable without destination-Secret
/// access. Reading them still hides destination coordinates.
#[tokio::test]
async fn without_secrets_map_exit_metadata_remains_writable() {
    let module = format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
await mapper.ready();
const road = mapper.getAreaById("{ROAD}");
const id = "00000000-0000-0000-0000-000000000052";
const link = "00000000-0000-0000-0000-000000000152";
await mapper.setRoomExit(road.id, 1, id, {{ weight: 5 }});
await mapper.setConnection(road.id, link, {{ thickness: 2 }});
const exit = mapper.getAreaById(road.id).room(1).exits.find((exit) => exit.id === id);
echo(`PROBE edited: ${{JSON.stringify({{weight: exit.weight, to: [exit.toAreaId, exit.toRoomNumber]}})}}`);
echo(`PROBE connection: ${{mapper.getAreaById(road.id).connections.find((connection) => connection.id === link).thickness}}`);
echo("PROBE_DONE");
"#
    );
    let output = run_package(
        PackageRun {
            session: 9_861,
            server: "RoadWritesWith",
            cloud: PlacesCloud::new().with_road(true),
            consent: |_| {},
            user_module: None,
            cues: Vec::new(),
        },
        &module,
        "PROBE_DONE",
    )
    .await;
    let lines = probes(&output);
    assert!(
        lines
            .iter()
            .any(|line| line == r#"PROBE edited: {"weight":5,"to":[null,null]}"#),
        "{output:#?}"
    );
    assert!(
        lines.iter().any(|line| line == "PROBE connection: 2"),
        "{output:#?}"
    );
}

/// A retained Map-owned link names its Secret anchor, never the ordinary room
/// with the same number. Packages without Secret consent cannot see the anchor
/// or retarget a link to it through a qualified endpoint.
#[tokio::test]
async fn retained_connection_anchors_keep_their_place_and_require_read_consent() {
    const LINK: Uuid = Uuid::from_u128(0x171);
    let fixture = |retained: bool| {
        let cloud = PlacesCloud::new();
        {
            let mut details = cloud.details.lock().unwrap();
            let mut ordinary = details.rooms[0].clone();
            ordinary.room_number = RoomNumber(2);
            ordinary.exits.clear();
            details.rooms.push(ordinary);
            if retained {
                details.connections.push(serde_json::from_value(serde_json::json!({
                    "id": LINK,
                    "endpoint_a": {"room_number": 2, "source": BOOKCASE, "side": "East", "port_offset": 0.5, "port_mode": "AutoPinned"},
                    "endpoint_b": null,
                    "kind": "Dangling", "routing": "Stub", "segment_shape": "Direct", "corner": "Sharp",
                    "route_points": [], "dash": "Solid", "color": "#a4a4a4", "thickness": 1.0
                })).expect("retained link parses"));
            }
        }
        cloud
    };
    let run = |session, server, retained, consent: fn(&mut SmudgyCapabilities)| {
        let module = format!(
            r#"
import {{ echo, mapper }} from "smudgy:core";
await mapper.ready();
const map = mapper.getAreaById("{MAP}");
const link = map.connections.find((link) => link.id === "{LINK}");
echo(`PROBE anchor: ${{JSON.stringify(link ? [link.endpointA.place, link.endpointA.roomNumber] : null)}}`);
if (!link) {{
    const probe = async (name, run) => {{
        try {{ await run(); echo(`PROBE ${{name}}: allowed`); }}
        catch (error) {{ echo(`PROBE ${{name}}: ${{error.message}}`); }}
    }};
    for (const place of ["{BOOKCASE}", "private"]) {{
        const endpoint = {{ place, roomNumber: 2, side: "East", portOffset: 0.5, portMode: "AutoPinned" }};
        await probe(`update ${{place}}`, () => mapper.setConnection(map.id, "{LINK}", {{ endpointA: endpoint }}));
        await probe(`create ${{place}}`, () => mapper.createLink(map.id, {{ endpointA: endpoint, traversals: [] }}));
        await probe(`batch ${{place}}`, () => mapper.mutateArea(map.id, (edit) => edit.setConnection("{LINK}", {{ endpointA: endpoint }})));
    }}
}}
echo("PROBE_DONE");
"#
        );
        async move {
            run_package(
                PackageRun {
                    session,
                    server,
                    cloud: fixture(retained),
                    consent,
                    user_module: None,
                    cues: Vec::new(),
                },
                &module,
                "PROBE_DONE",
            )
            .await
        }
    };
    let reader = run(9_891, "RetainedAnchorReader", true, |caps| {
        caps.secrets_read = true;
    })
    .await;
    assert!(
        probes(&reader).contains(&format!(r#"PROBE anchor: ["{BOOKCASE}",2]"#)),
        "{reader:#?}"
    );
    let hidden = run(9_892, "RetainedAnchorHidden", true, |_| {}).await;
    let absent = run(9_893, "RetainedAnchorAbsent", false, |_| {}).await;
    assert_eq!(probes(&hidden), probes(&absent));
    let results = probes(&hidden);
    assert_eq!(results.len(), 7, "{hidden:#?}");
    assert_eq!(results[0], "PROBE anchor: null");
    assert!(
        results[1..]
            .iter()
            .all(|line| line.contains("secrets-read")),
        "{hidden:#?}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Contrasts the same retained-data fixture with and without Secret consent.
async fn retained_room_data_reads_and_writes_its_owner_without_aliasing_the_map_room() {
    let fixture = |retained: bool| {
        let cloud = PlacesCloud::new();
        {
            let mut details = cloud.details.lock().unwrap();
            let mut namesake = details.rooms[0].clone();
            namesake.room_number = RoomNumber(2);
            namesake.exits.clear();
            details.rooms.push(namesake);
            if retained {
                let data: RoomData = serde_json::from_value(serde_json::json!({
                    "room_number": 2, "room_source": BOOKCASE,
                    "properties": [{"name": "retained", "value": "Map-owned note"}],
                    "tags": ["MAP-ATTACHMENT"]
                }))
                .unwrap();
                details.room_data.push(data.clone());
                let ledger = details
                    .sources
                    .iter_mut()
                    .find(|bundle| bundle.source == SourceId::Secret(LEDGER))
                    .unwrap();
                ledger.room_data.push(RoomData {
                    properties: vec![property("retained", "Ledger-owned note")],
                    tags: ["LEDGER-ATTACHMENT".to_string()].into_iter().collect(),
                    ..data
                });
            }
        }
        cloud
    };
    let module = format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
await mapper.ready();
try {{
    const map = () => mapper.getAreaById("{MAP}");
    const room = () => mapper.getAreaById("{BOOKCASE}").room(2);
    const check = (name, actual, expected) => {{
        if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${{name}}: ${{JSON.stringify(actual)}} != ${{JSON.stringify(expected)}}`);
    }};
    check("Map note", room().in("map").data("retained"), "Map-owned note");
    check("other Secret note", room().in("{LEDGER}").data("retained"), "Ledger-owned note");
    check("own note", room().data("notes"), "Vault notes");
    check("combined tags", room().tags, ["LEDGER-ATTACHMENT", "MAP-ATTACHMENT", "VAULT"]);
    check("namesake", map().room(2).in("map").data("retained"), undefined);
    const refs = (rooms) => rooms.map((r) => [r.areaId, r.roomNumber]);
    check("map tag search", refs(map().findRoomsWithTag("MAP-ATTACHMENT")), [["{BOOKCASE}", 2]]);
    check("source tag search", refs(mapper.getAreaById("{BOOKCASE}").findRoomsWithTag("MAP-ATTACHMENT")), [["{BOOKCASE}", 2]]);
    check("atlas property search", mapper.findRoomsByProperty("retained", "Map-owned note").map((r) => [r.areaId, r.roomNumber]), [["{BOOKCASE}", 2]]);
    await room().in("map").setData("retained", "Edited Map note");
    check("edited", room().in("map").data("retained"), "Edited Map note");
    await room().in("map").addTag("ADDED");
    check("tag added", room().in("map").hasTag("added"), true);
    await room().in("map").removeTag("ADDED");
    check("tag removed", room().in("map").hasTag("added"), false);
    await room().in("map").deleteData("retained");
    check("deleted", room().in("map").data("retained"), undefined);
    check("other Secret untouched", room().in("{LEDGER}").data("retained"), "Ledger-owned note");
    check("ordinary room untouched", map().room(2).data("notes"), "Ordinary");
    let refused = false;
    try {{ await room().in("{LEDGER}").setData("retained", "forbidden"); }}
    catch (error) {{ refused = error.message.includes("secrets-write"); }}
    check("read consent cannot write another Secret", refused, true);
    echo("RETAINED_OK");
}} catch (error) {{ echo(`RETAINED_FAIL ${{error.stack}}`); }}
"#
    );
    let reader = run_package(
        PackageRun {
            session: 9_894,
            server: "RetainedRoomDataReader",
            cloud: fixture(true),
            consent: |caps| caps.secrets_read = true,
            user_module: None,
            cues: Vec::new(),
        },
        &module,
        "RETAINED_",
    )
    .await;
    assert!(
        reader.iter().any(|line| line == "RETAINED_OK"),
        "{reader:#?}"
    );

    let probe = format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
await mapper.ready();
const map = mapper.getAreaById("{MAP}");
echo(`PROBE tags: ${{JSON.stringify(map.findRoomsWithTag("MAP-ATTACHMENT"))}}`);
echo(`PROBE properties: ${{JSON.stringify(mapper.findRoomsByProperty("retained", "Map-owned note"))}}`);
echo("PROBE_DONE");
"#
    );
    let mut outcomes = Vec::new();
    for (session, server, retained) in [
        (9_895, "RetainedRoomDataHidden", true),
        (9_896, "RetainedRoomDataAbsent", false),
    ] {
        outcomes.push(probes(
            &run_package(
                PackageRun {
                    session,
                    server,
                    cloud: fixture(retained),
                    consent: |_| {},
                    user_module: None,
                    cues: Vec::new(),
                },
                &probe,
                "PROBE_DONE",
            )
            .await,
        ));
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0], ["PROBE tags: []", "PROBE properties: []"]);
}

/// What a package without `secrets` reads of the map's export (everything but ids that
/// differ by run) after the user's own script wrote into the Bookcase, or wrote nothing.
async fn export_after_user_write(
    session: u32,
    server: &'static str,
    user_writes: bool,
) -> Vec<String> {
    let package = format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
await mapper.ready();
await new Promise((resolve) => setTimeout(resolve, 1500));
const e = await mapper.exportArea("{MAP}");
const {{ created_at, ...area }} = e;
echo(`PROBE export: ${{JSON.stringify(area)}}`);
echo("PROBE_DONE");
"#
    );
    let user = if user_writes {
        format!(
            r#"
import {{ echo, mapper }} from "smudgy:core";
await mapper.ready();
await mapper.setRoomTitle("{BOOKCASE}", 2, "Strongroom");
await mapper.mutateArea("{MAP}", (m) => m.addRoomTag(1, "secret-tag"), {{ in: "{BOOKCASE}" }});
echo("USER_WROTE");
"#
        )
    } else {
        "import { echo } from \"smudgy:core\";\necho(\"USER_WROTE\");\n".to_string()
    };
    run_package(
        PackageRun {
            session,
            server,
            cloud: PlacesCloud::new(),
            consent: |_| {},
            user_module: Some(user),
            cues: Vec::new(),
        },
        &package,
        "PROBE_DONE",
    )
    .await
}

/// A write into a Secret by the user's own script moves nothing a package without `secrets`
/// reads of the map's export.
#[tokio::test]
async fn without_secrets_a_secret_write_moves_nothing_in_the_export() {
    let wrote = export_after_user_write(9_831, "RevWrote", true).await;
    let quiet = export_after_user_write(9_832, "RevQuiet", false).await;
    for line in &wrote {
        if line.starts_with("PROBE") || line.starts_with("USER") || line.contains("rror") {
            println!("WROTE {line}");
        }
    }
    for line in probes(&quiet) {
        println!("QUIET {line}");
    }
    assert!(
        wrote.iter().any(|l| l == "USER_WROTE"),
        "{}",
        wrote.join("\n")
    );
    assert_eq!(
        probes(&wrote),
        probes(&quiet),
        "a Secret write moved something a package without secrets reads"
    );
}

fn merge_module() -> String {
    format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
const probe = async (label, run) => {{
    let seen;
    try {{
        seen = JSON.stringify(await run()) ?? "undefined";
    }} catch (error) {{
        seen = `!! ${{error?.constructor?.name}}: ${{error?.message ?? error}}`;
    }}
    echo(`PROBE ${{label}}: ${{seen}}`);
}};
await mapper.ready();
await probe("mergeRooms road remove milestone", () => mapper.mergeRooms("{ROAD}", 2, 1));
echo("PROBE_DONE");
"#
    )
}

/// Joining the Road's milestone into the tollgate: a package without `secrets` is answered
/// as on the same map without the exit into the Bookcase.
#[tokio::test]
async fn without_secrets_merge_rooms_answers_as_if_there_were_none() {
    let module = merge_module();
    let run = |session: u32,
               server: &'static str,
               cloud: PlacesCloud,
               consent: fn(&mut SmudgyCapabilities)| {
        run_package(
            PackageRun {
                session,
                server,
                cloud,
                consent,
                user_module: None,
                cues: Vec::new(),
            },
            &module,
            "PROBE_DONE",
        )
    };
    let with_secret = run(
        9_841,
        "MergeWith",
        PlacesCloud::new().with_road(true),
        |_| {},
    )
    .await;
    let without = run(
        9_842,
        "MergeWithout",
        PlacesCloud::without_sources().with_road(false),
        |_| {},
    )
    .await;
    let reader = run(
        9_843,
        "MergeReader",
        PlacesCloud::new().with_road(true),
        |caps| caps.secrets_read = true,
    )
    .await;
    for line in probes(&with_secret) {
        println!("WITH    {line}");
    }
    for line in probes(&without) {
        println!("WITHOUT {line}");
    }
    for line in probes(&reader) {
        println!("READER  {line}");
    }
    assert_eq!(probes(&with_secret), probes(&without));
}

fn map_write_module() -> String {
    format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";
const probe = async (label, run) => {{
    let seen;
    try {{
        const value = await Promise.race([Promise.resolve().then(run), new Promise((resolve) => setTimeout(() => resolve("TIMEOUT"), 2500))]);
        seen = typeof value === "string" && /^[0-9a-f-]{{36}}$/.test(value) ? "an id" : (JSON.stringify(value) ?? "undefined");
    }} catch (error) {{
        seen = `!! ${{error?.constructor?.name}}: ${{error?.message ?? error}}`;
    }}
    echo(`PROBE ${{label}}: ${{seen}}`);
}};
const M = "{MAP}";
const live = () => mapper.getAreaById(M);
await mapper.ready();
await probe("addRoomTag door", () => mapper.addRoomTag(M, 1, "door"));
await probe("tags 1", () => live().room(1).tags);
await probe("removeRoomTag loot", () => mapper.removeRoomTag(M, 3, "loot"));
await probe("tags 3", () => live().room(3).tags);
await probe("setRoomProperty notes", () => mapper.setRoomProperty(M, 1, "notes", "Behind the bookcase"));
await probe("data notes", () => live().room(1).data("notes"));
await probe("createRoomExit east", () => mapper.createRoomExit(M, 1, {{ fromDirection: "East", toAreaId: M, toRoomNumber: 3 }}));
await probe("createRoomExit down 3", () => mapper.createRoomExit(M, 3, {{ fromDirection: "Down" }}));
await probe("updateRoom level", () => mapper.updateRoom(M, 1, {{ level: 2 }}));
await probe("updateRoom x", () => mapper.updateRoom(M, 3, {{ x: 9 }}));
await probe("deleteRoom 3", () => mapper.deleteRoom(M, 3));
await probe("mergeRooms 1 3", () => mapper.mergeRooms(M, 1, 3));
await probe("mergeRooms 4 1", () => mapper.mergeRooms(M, 4, 1));
await probe("mutate createExit", () => mapper.mutateArea(M, async (m) => {{ await m.createRoomExit(1, {{ fromDirection: "East", toAreaId: M, toRoomNumber: 3 }}); }}));
echo("PROBE_DONE");
"#
    )
}

/// Writes to the map's own rooms answer a package without `secrets` as on the same map
/// without Secrets: what a Secret keeps on a room never refuses a join, and the server takes
/// it with the room.
#[tokio::test]
async fn without_secrets_map_writes_answer_as_if_there_were_none() {
    let module = map_write_module();
    let run = |session: u32, server: &'static str, cloud: PlacesCloud| {
        run_package(
            PackageRun {
                session,
                server,
                cloud,
                consent: |_| {},
                user_module: None,
                cues: Vec::new(),
            },
            &module,
            "PROBE_DONE",
        )
    };
    let with_secret = run(9_851, "MapWritesWith", PlacesCloud::new()).await;
    let without = run(9_852, "MapWritesWithout", PlacesCloud::without_sources()).await;
    for line in probes(&with_secret) {
        println!("WITH    {line}");
    }
    for line in probes(&without) {
        println!("WITHOUT {line}");
    }
    let mut diffs = Vec::new();
    for (s, p) in probes(&with_secret).iter().zip(&probes(&without)) {
        if s != p {
            diffs.push(format!("WITH:    {s}\nWITHOUT: {p}"));
        }
    }
    assert!(diffs.is_empty(), "differences:\n{}", diffs.join("\n\n"));
}

/// A package without `secrets` that set a location in a Secret's room reads it back only until
/// the session's location is next written, whether or not it hears of the write: one that never
/// listens to `map:room` reads the player's own later return to that room as somewhere unmapped.
#[tokio::test]
async fn a_location_exemption_ends_when_the_session_moves_unheard() {
    let package = format!(
        r#"
import {{ echo, mapper }} from "smudgy:core";

await mapper.ready();
mapper.setCurrentLocation("{BOOKCASE}", 2);
echo(`PKG_SET ${{JSON.stringify(mapper.getCurrentLocation() ?? null)}}`);
echo("PKG_READY");
setTimeout(() => {{
    echo(`PKG_LATER ${{JSON.stringify(mapper.getCurrentLocation() ?? null)}}`);
    echo("PKG_DONE");
}}, 2000);
"#
    );
    let lines = run_package(
        PackageRun {
            session: 9_398,
            server: "MapperNamedLocationUnheard",
            cloud: PlacesCloud::new(),
            consent: |caps| caps.interop_read = true,
            user_module: None,
            cues: vec![(
                "PKG_READY",
                vec![
                    RuntimeAction::SetCurrentLocation(AreaId(MAP), Some(1)),
                    RuntimeAction::SetCurrentLocation(AreaId(BOOKCASE), Some(2)),
                ],
            )],
        },
        &package,
        "PKG_DONE",
    )
    .await;
    let all = lines.join("\n");
    assert!(
        lines
            .iter()
            .any(|line| *line == format!(r#"PKG_SET {{"area":"{BOOKCASE}","room":2}}"#)),
        "the package reads back what it set:\n{all}"
    );
    assert!(
        lines
            .iter()
            .any(|line| *line == format!(r#"PKG_LATER {{"area":"{MAP}"}}"#)),
        "the player's own return to the Secret's room reads as somewhere unmapped:\n{all}"
    );
}
