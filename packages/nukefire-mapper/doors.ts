/** `door` as an exit reads it back: a missing name or opening command is null. */
export function copyDoor(door: Readonly<DoorArgs> | null | undefined): Door | null {
  return door
    ? { state: door.state, name: door.name ?? null, opensWith: door.opensWith ?? null }
    : null;
}

/** True when both exits lack a door, or both doors agree on state, name and opening command. */
export function sameDoor(a: Door | null, b: Door | null): boolean {
  if (a === null || b === null) return a === b;
  return a.state === b.state && a.name === b.name && a.opensWith === b.opensWith;
}

/** True when the door is closed; a locked door is closed too. */
export function doorIsClosed(door: Door | null): boolean {
  return door?.state === "closed" || door?.state === "locked";
}

/**
 * The door an exit gets from NukeFire's `closed` and `locked` report, given the
 * exit's door `current`: locked or closed as reported; open when the report is
 * neither and the exit already has a door, since an open door is still a door;
 * otherwise none. An exit update replaces its door whole, so the current door's
 * name and opening command carry over.
 */
export function reportedDoor(closed: boolean, locked: boolean, current: Door | null): DoorArgs | null {
  const state: DoorState | null = locked ? "locked" : closed ? "closed" : current ? "open" : null;
  return state === null
    ? null
    : { state, name: current?.name ?? null, opensWith: current?.opensWith ?? null };
}
