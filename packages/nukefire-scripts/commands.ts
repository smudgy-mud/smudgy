// =============================================================================
//  `nf` command-deck utilities
// =============================================================================

import {
  createAlias,
  createState,
  createTrigger,
  echo,
  link,
  mapper,
  send,
  sendRaw,
  style,
  submission,
} from "smudgy:core";
import { submit } from "smudgy:events/sys";
import { compareLayoutQuality, planAreaChange } from "smudgy://kapusniak/map-layout";
import { nukefire } from "smudgy://kapusniak/nukefire-gmcp";
import {
  buildNavigationRoute,
  formatRoute,
  sameAreaId,
  splitArrivalCommands,
  takeCommandArgument,
  type NavigationStep,
} from "./navigation.ts";
import * as welcome from "./welcome.tsx";

let reflowing = false;

interface RoomSummary {
  vnum: number | null;
  name: string;
  area: string;
}

export interface NfPathSnapshot {
  from: RoomSummary;
  to: RoomSummary;
  commands: string[];
  text: string;
}

export interface LastDeathSnapshot extends RoomSummary {
  recordedAt: number | null;
}

/** The last route produced by `nf path`, for other packages and sibling sessions. */
export const nfPath = createState<NfPathSnapshot | null>("nfPath");
if (!nfPath.value) nfPath.set(null);

/** The last death location observed in this live session. */
export const lastDeath = createState<LastDeathSnapshot>("lastDeath");
if (!lastDeath.value) {
  lastDeath.set({ vnum: null, name: "", area: "", recordedAt: null });
}

function nukeFireAreas(): Area[] {
  const managed = mapper.areas.filter((area) =>
    area.data("nukefire.mapper") === "NukeFire.Map.Local"
  );
  return managed.length > 0 ? managed : mapper.areas;
}

function allMappedRooms(): Room[] {
  return nukeFireAreas().flatMap((area) =>
    area.room_numbers.flatMap((number) => {
      const room = area.room(number);
      return room ? [room] : [];
    })
  );
}

function roomVnum(room: Room): number | null {
  const externalId = room.externalId?.trim();
  if (!externalId) return null;
  const parsed = Number(externalId);
  return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : null;
}

function summarizeRoom(room: Room): RoomSummary {
  return {
    vnum: roomVnum(room),
    name: room.title,
    area: mapper.getAreaById(room.area_id).name,
  };
}

function describeRoom(room: Room): string {
  const summary = summarizeRoom(room);
  return `${summary.vnum === null ? "#?" : `#${summary.vnum}`}  ${summary.area} — ${summary.name}`;
}

function matchingRooms(rawQuery: string): Room[] {
  const query = rawQuery.trim();
  const numeric = /^#?(\d+)$/.exec(query);
  const rooms = allMappedRooms();
  if (numeric) {
    const wanted = Number(numeric[1]);
    return rooms.filter((room) => roomVnum(room) === wanted);
  }

  const wanted = query.toLowerCase();
  const exact = rooms.filter((room) => room.title.trim().toLowerCase() === wanted);
  return exact.length > 0
    ? exact
    : rooms.filter((room) => room.title.toLowerCase().includes(wanted));
}

function uniqueRoom(rawQuery: string, label: string): Room | undefined {
  const matches = matchingRooms(rawQuery);
  if (matches.length === 1) return matches[0];
  if (matches.length === 0) {
    echo(style.warn`[nf] No mapped room matches ${label} “${rawQuery}”.`);
    return undefined;
  }

  echo(style.warn`[nf] ${label} “${rawQuery}” is ambiguous; use a VNUM:`);
  for (const room of matches.slice(0, 10)) echo(`  ${describeRoom(room)}`);
  if (matches.length > 10) echo(`  …and ${matches.length - 10} more`);
  return undefined;
}

function currentRoom(): Room | undefined {
  const location = mapper.getCurrentLocation();
  return location?.room === undefined
    ? undefined
    : mapper.getAreaById(location.area).room(location.room);
}

interface RouteResolution {
  steps: NavigationStep[];
  error: string | null;
}

function routeBetween(from: Room, to: Room): RouteResolution {
  if (sameAreaId(from.area_id, to.area_id) && from.room_number === to.room_number) {
    return { steps: [], error: null };
  }

  const path = mapper.getPathBetweenRooms(
    from.area_id,
    from.room_number,
    to.area_id,
    to.room_number,
  );
  if (path.length < 2) return { steps: [], error: "no mapped path is reachable" };

  const route = buildNavigationRoute(path, (area, room) =>
    mapper.getAreaById(area).room(room)
  );
  return route.error
    ? { steps: route.steps, error: route.error }
    : { steps: route.steps, error: null };
}

function sendTraversalCommand(raw: string): void {
  // A mapped special exit can itself be compound. Raw sends prevent the
  // emitted movement from recursively firing aliases.
  for (const part of raw.split(";")) {
    const command = part.trim();
    if (command) sendRaw(command + "\n");
  }
}

function walkRoute(steps: readonly NavigationStep[], arrivalCommands: readonly string[] = []): void {
  for (const step of steps) sendTraversalCommand(step.command);
  // Arrival commands intentionally take the ordinary path so they can be
  // aliases as well as literal MUD commands.
  for (const command of arrivalCommands) send(command);
}

function warnAboutDoors(steps: readonly NavigationStep[]): void {
  const doors = steps.filter((step) => step.closed || step.locked).length;
  if (doors > 0) {
    echo(style.warn`[nf] ${doors} mapped door${doors === 1 ? " is" : "s are"} closed or locked; this route will not open them.`);
  }
}

function walkToMappedRoom(destination: Room): void {
  const here = currentRoom();
  if (!here) {
    echo(style.warn`[nf] The mapper does not know your current room.`);
    return;
  }
  const route = routeBetween(here, destination);
  if (route.error) {
    echo(style.warn`[nf] Cannot walk to ${describeRoom(destination)}: ${route.error}.`);
    return;
  }
  warnAboutDoors(route.steps);
  walkRoute(route.steps);
}

function showRouteResult(room: Room, route: RouteResolution): void {
  if (route.error) {
    echo(`  !walk  ${describeRoom(room)} (${route.error})`);
    return;
  }
  const steps = route.steps;
  if (steps.length === 0) {
    echo(`  HERE   ${describeRoom(room)}`);
    return;
  }

  // Re-resolve on click: a result remains safe to use after the player has
  // moved since running `find` or `death`.
  echo`${link(() => walkToMappedRoom(room))`+WALK`}  ${describeRoom(room)}`;
  echo(`         ${formatRoute(steps)}`);
}

function findRooms(rawQuery: string): void {
  const query = rawQuery.trim().replace(/^(["'])(.*)\1$/, "$2");
  if (!query) {
    echo(style.warn`[nf] Usage: nf find <room name|VNUM>`);
    return;
  }

  const matches = matchingRooms(query);
  if (matches.length === 0) {
    echo(style.warn`[nf] No mapped room matches “${query}”.`);
    return;
  }

  const here = currentRoom();
  echo(`[nf] ${matches.length} mapped match${matches.length === 1 ? "" : "es"} for “${query}”:`);
  for (const room of matches.slice(0, 50)) {
    showRouteResult(
      room,
      here ? routeBetween(here, room) : { steps: [], error: "current room is unknown" },
    );
  }
  if (matches.length > 50) echo(`  …${matches.length - 50} more; use the full room name or VNUM`);
}

function runTo(rawArgs: string): void {
  const targetArg = takeCommandArgument(rawArgs);
  if (!targetArg) {
    echo(style.warn`[nf] Usage: nf run <room name|VNUM|death> [arrival commands]`);
    return;
  }

  const death = lastDeath.value;
  const targetQuery = targetArg.value.toLowerCase() === "death"
    ? death?.vnum === null || death?.vnum === undefined ? undefined : String(death.vnum)
    : targetArg.value;
  if (!targetQuery) {
    echo(style.warn`[nf] No death room has been recorded in this session.`);
    return;
  }

  const destination = uniqueRoom(targetQuery, "destination");
  const here = currentRoom();
  if (!destination || !here) {
    if (!here) echo(style.warn`[nf] The mapper does not know your current room.`);
    return;
  }

  const route = routeBetween(here, destination);
  if (route.error) {
    showRouteResult(destination, route);
    return;
  }

  const arrival = splitArrivalCommands(targetArg.rest);
  echo(`[nf] Run to ${describeRoom(destination)}`);
  echo(`     ${route.steps.length === 0 ? "already there" : formatRoute(route.steps)}`);
  if (arrival.length > 0) echo(`     then: ${arrival.join(" ; ")}`);
  warnAboutDoors(route.steps);
  walkRoute(route.steps, arrival);
}

function showDeath(): void {
  const death = lastDeath.value;
  if (!death || death.vnum === null) {
    echo(style.warn`[nf] No death room has been recorded in this session.`);
    return;
  }
  const room = uniqueRoom(String(death.vnum), "death room");
  if (!room) return;
  const here = currentRoom();
  echo(`[nf] Last death: ${describeRoom(room)}`);
  showRouteResult(
    room,
    here ? routeBetween(here, room) : { steps: [], error: "current room is unknown" },
  );
}

function showPath(rawArgs: string): void {
  const fromArg = takeCommandArgument(rawArgs);
  const toArg = fromArg ? takeCommandArgument(fromArg.rest) : undefined;
  if (!fromArg || !toArg || toArg.rest.trim()) {
    echo(style.warn`[nf] Usage: nf path <FROM name|VNUM> <TO name|VNUM>`);
    echo(`     Quote room names containing spaces.`);
    return;
  }

  const from = uniqueRoom(fromArg.value, "FROM room");
  const to = uniqueRoom(toArg.value, "TO room");
  if (!from || !to) return;
  const route = routeBetween(from, to);
  if (route.error) {
    echo(style.warn`[nf] Cannot path from ${describeRoom(from)} to ${describeRoom(to)}: ${route.error}.`);
    return;
  }

  const text = formatRoute(route.steps);
  nfPath.set({
    from: summarizeRoom(from),
    to: summarizeRoom(to),
    commands: route.steps.map((step) => step.command),
    text,
  });
  echo(`[nf] Path from ${describeRoom(from)}`);
  echo(`          to ${describeRoom(to)}`);
  echo(`     ${text || "already there"}`);
  echo(`     Available to scripts as shared state nfPath.commands.`);
}

function rememberDeath(): void {
  const info = nukefire.value?.Room?.Info;
  const vnum = info?.num;
  if (vnum === undefined || !Number.isSafeInteger(vnum) || vnum < 0) {
    echo(style.warn`[nf] You died, but Room.Info did not contain a usable VNUM.`);
    return;
  }

  const mapped = matchingRooms(String(vnum))[0];
  lastDeath.set({
    vnum,
    name: info?.name || mapped?.title || "",
    area: info?.area || (mapped ? mapper.getAreaById(mapped.area_id).name : ""),
    recordedAt: Date.now(),
  });
  echo(`[nf] Remembered death room #${vnum}${info?.name ? ` — ${info.name}` : ""}.`);
}

function qualitySummary(quality: {
  cardinalRayViolations: number;
  roomObstructions: number;
  linkCrossings: number;
}): string {
  return `${quality.cardinalRayViolations} directional / ` +
    `${quality.roomObstructions} obstructed / ${quality.linkCrossings} crossing`;
}

export function showHelp(): void {
  echo("");
  echo(style.yellow`NukeFire Scripts utilities`);
  echo("  nf find <name|VNUM>                   Find mapped rooms and show clickable routes");
  echo("  nf run <name|VNUM|death> [commands]   Walk a route, then run optional commands");
  echo("  nf death                              Show and route to the last death room");
  echo("  nf path <FROM> <TO>                   Publish a route between two mapped rooms");
  echo("  nf welcome                            Open the welcome and multi-session guide");
  echo("  nf reflow                             Thoroughly search and reflow the current area");
  echo("  nf help                               Show this help text");
  echo("");
  echo("Quote names containing spaces in run/path; separate arrival commands with semicolons.");
  echo(style.warn`Routes do not open doors or avoid hazardous rooms.`);
  echo("");
  echo("Session routing:");
  echo("  F1..F4 / Ctrl+F1..F4  Select or magnify a session");
  echo("  134 look              Send from sessions 1, 3, and 4");
  echo("  * look                Send from every session");
  echo("  -4 look               Send from every session except 4");
}

async function reflowCurrentArea(): Promise<void> {
  if (reflowing) {
    echo(style.warn`[nf] A map reflow is already running.`);
    return;
  }
  const location = mapper.getCurrentLocation();
  if (!location) {
    echo(style.warn`[nf] No mapped room is selected.`);
    return;
  }

  reflowing = true;
  try {
    echo("[nf] Searching violation-prioritized anchors for a better layout…");
    const result = await planAreaChange(location.area, {
      type: "reflow",
      anchor: location.room,
    }, {
      effort: "thorough",
      // Honor both map-layout's generic lock conventions and the property used
      // by nukefire-mapper's automatic planner.
      isRoomMovable: (room) =>
        !room.hasTag("LAYOUT_LOCKED") &&
        room.data("layoutLocked") !== "true" &&
        room.data("nukefire.layout.locked") !== "true",
    });
    const updates: [RoomNumber, UpdateRoomParams][] = result.patch.moves.map((move) => {
      if (move.roomNumber === undefined) {
        throw new Error(`layout move ${move.id} has no Smudgy room number`);
      }
      return [move.roomNumber, {
        x: move.to.x,
        y: move.to.y,
        level: move.to.level,
      }];
    });
    if (updates.length > 0) {
      await mapper.mutateArea(location.area, (mutation) => mutation.updateRooms(updates), {
        description: "Reflow NukeFire rooms",
      });
      // Re-sending the active location makes mounted MapViews derive their
      // translation from the player's newly committed room coordinates.
      const current = mapper.getCurrentLocation();
      if (current) mapper.setCurrentLocation(current.area, current.room);
    }
    const search = result.search;
    const improvementText = search && compareLayoutQuality(result.quality, search.baselineQuality) > 0
      ? ` Improved the regular anchored score (${qualitySummary(search.baselineQuality)} → ` +
        `${qualitySummary(result.quality)}).`
      : "";
    const searchText = search
      ? ` Tried ${search.anchorsTried.length} anchors across ${search.planningPasses} passes; ` +
        `selected ${search.selectedAnchor === null ? "the unanchored result" : `room ${search.selectedAnchor} as anchor`}.`
      : "";
    echo(
      `[nf] Thorough reflow moved ${updates.length} room${updates.length === 1 ? "" : "s"}; ` +
        `${result.quality.cardinalRayViolations} directional violation${
          result.quality.cardinalRayViolations === 1 ? "" : "s"
        } remain.${improvementText}${searchText}`,
    );
  } catch (caught) {
    const message = caught instanceof Error ? caught.message : String(caught);
    echo(style.warn`[nf] Reflow failed: ${message}`);
  } finally {
    reflowing = false;
  }
}

function runUtility(rawArgs: string): void {
  const args = rawArgs.trim();
  const command = args.split(/\s+/, 1)[0]?.toLowerCase() || "help";
  const commandArgs = command === "help" && !args ? "" : args.slice(command.length).trimStart();
  switch (command) {
    case "help":
      showHelp();
      break;
    case "find":
      findRooms(commandArgs);
      break;
    case "run":
      runTo(commandArgs);
      break;
    case "death":
      showDeath();
      break;
    case "path":
      showPath(commandArgs);
      break;
    case "welcome":
      welcome.open();
      break;
    case "reflow":
      void reflowCurrentArea();
      break;
    default:
      echo(style.warn`[nf] Unknown utility “${command}”.`);
      showHelp();
      break;
  }
}

createAlias(/^nf(?:\s+(?<args>.*))?$/i, ({ args }) => {
  runUtility(args ?? "");
}, { name: "nf-utilities" });

// Typed submissions normally split on Smudgy's command separator before an
// alias sees them. Intercept `run` while the line is still whole so NF's
// `look north;look south` arrival syntax remains meaningful.
submit.on(() => {
  const match = /^nf\s+(?<args>run(?:\s+.*)?)$/i.exec(submission.text.trim());
  if (!match?.groups?.args) return;
  submission.cancel();
  runUtility(match.groups.args);
});

createTrigger(/^You are dead!\s+Sorry\.\.\.$/, rememberDeath, {
  name: "nf-remember-death-room",
});
