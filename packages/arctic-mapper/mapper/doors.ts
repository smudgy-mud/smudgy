/** True when the exit's door is closed; a locked door is closed too. */
export function doorIsClosed(exit: Pick<Exit, "door">): boolean {
    const state = exit.door?.state;
    return state === "closed" || state === "locked";
}

/**
 * The command that opens the exit's door before moving through it: the door's
 * `opensWith`, else `fallback` when the door is mapped closed or `closedNow`
 * says it is, else null.
 */
export function openCommandFor(exit: Pick<Exit, "door">, fallback: string, closedNow = false): string | null {
    return exit.door?.opensWith || ((doorIsClosed(exit) || closedNow) ? fallback : null);
}

/** A door as `map` output shows it: its state, then its name in quotes when it has one. */
export function describeDoor(door: Pick<DoorArgs, "state" | "name">): string {
    return door.name ? `${door.state} "${door.name}"` : door.state;
}

/**
 * `door` with `changes` applied, as an exit update takes it. An update replaces
 * the door whole, so the name and open command `changes` leaves out keep their
 * current values; an exit without a door gets a closed one.
 */
export function doorUpdate(door: Door | null, changes: Partial<DoorArgs>): DoorArgs {
    return {
        state: changes.state ?? door?.state ?? "closed",
        name: changes.name !== undefined ? changes.name : door?.name ?? null,
        opensWith: changes.opensWith !== undefined ? changes.opensWith : door?.opensWith ?? null,
    };
}

/**
 * The door state `map exit <dir> closed|locked <value>` leaves. Locked implies
 * closed, so closing a locked door keeps it locked, opening any door unlocks it,
 * and unlocking a locked door leaves it closed. Opening or unlocking adds no door
 * to an exit without one.
 */
export function nextDoorState(current: DoorState | null, flag: "closed" | "locked", value: boolean): DoorState | null {
    if (flag === "closed") {
        if (value) {
            return current === "locked" ? "locked" : "closed";
        }
        return current === null ? null : "open";
    }
    if (value) {
        return "locked";
    }
    return current === "locked" ? "closed" : current;
}
