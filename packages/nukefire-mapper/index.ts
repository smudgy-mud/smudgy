// NukeFire-specific automatic mapper. Map.Local is authoritative; Room.Info
// contributes the human-readable current-area name and vertical topology.
// Mapping is shared client state, so only the oldest live session may mutate it.

import { getSessions, mapper, session, type EventSubscription } from "smudgy:core";
import { created, destroyed } from "smudgy:events/sessions";
import { get } from "smudgy:params";
import { nukefire, watchMessage } from "smudgy://kapusniak/nukefire-gmcp";
import { DEFAULT_DECISION_LOG_FILE } from "./decision-log.ts";
import { externalRoomId, isUsableVnum } from "./model.ts";
import { NukeFireMapper } from "./mapper.ts";
import { resolveFollowedLocation } from "./location-follow.ts";
import { ownsSharedMapping } from "./ownership.ts";

export * from "./model.ts";
export * from "./layout.ts";
export * from "./routing.ts";
export * from "./atlas-resolution.ts";
export * from "./area-resolution.ts";
export * from "./decision-log.ts";
export * from "./location-follow.ts";
export * from "./room-info.ts";
export * from "./ownership.ts";
export * from "./reflow-policy.ts";
export * from "./mapper.ts";

export const nukefireMapper = new NukeFireMapper({
  storage: "local",
  decisionLogFile: get("debugMappingDecisions") === true
    ? DEFAULT_DECISION_LOG_FILE
    : false,
});

let ownershipTimer: ReturnType<typeof setTimeout> | undefined;

// Non-owner sessions still track the player: the current-location marker is
// per-session, so follow Room.Info against the owner-written map, locate-only.
let followSubscription: EventSubscription | undefined;
let followedLocation = "";

function followRoomInfo(vnum: number | undefined): void {
  if (vnum === undefined || !isUsableVnum(vnum)) return;
  const located = resolveFollowedLocation(mapper, externalRoomId(vnum));
  if (!located) return;
  const key = `${located.area[0]}:${located.area[1]}:${located.room}`;
  if (key === followedLocation) return;
  mapper.setCurrentLocation(located.area, located.room);
  followedLocation = key;
}

function startFollowing(): void {
  if (followSubscription) return;
  followSubscription = watchMessage("Room.Info", (info) => followRoomInfo(info?.num));
  // onMessage/watchMessage have no replay; seed from the retained tree so a
  // stationary player is located immediately after losing ownership.
  followRoomInfo(nukefire.value?.Room?.Info?.num);
}

function stopFollowing(): void {
  followSubscription?.off();
  followSubscription = undefined;
  followedLocation = "";
}

function ownsMapping(): boolean {
  // Enumerating sibling sessions is itself the `reach-others` capability, so
  // the manifest keeps that grant even though nothing here acts on them.
  return ownsSharedMapping(session.id, getSessions());
}

function reconcileMappingOwner(): void {
  ownershipTimer = undefined;
  if (ownsMapping()) {
    stopFollowing();
    nukefireMapper.start();
  } else {
    nukefireMapper.stop();
    startFollowing();
  }
}

function scheduleOwnershipCheck(): void {
  if (ownershipTimer !== undefined) clearTimeout(ownershipTimer);
  // Lifecycle events are emitted as the registry changes; defer one turn so
  // a destroyed session has disappeared before electing its successor.
  ownershipTimer = setTimeout(reconcileMappingOwner, 100);
}

created.on(scheduleOwnershipCheck);
destroyed.on(scheduleOwnershipCheck);
reconcileMappingOwner();
