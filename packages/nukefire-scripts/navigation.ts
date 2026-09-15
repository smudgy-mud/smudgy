// Pure helpers for the NF command layer. Keeping route construction
// independent from smudgy:core makes the edge cases cheap to exercise in Node.

/** An area id as the mapper spells it: a canonical UUID string. */
export type NavigationAreaId = string;

export interface NavigationExit {
  from_direction: string;
  to_area_id: NavigationAreaId | null;
  to_room_number: number | null;
  command: string | null;
  is_closed?: boolean;
  is_locked?: boolean;
}

export interface NavigationRoom {
  area_id: NavigationAreaId;
  room_number: number;
  exits: readonly NavigationExit[];
}

export interface NavigationStep {
  from: readonly [NavigationAreaId, number];
  to: readonly [NavigationAreaId, number];
  direction: string;
  command: string;
  closed: boolean;
  locked: boolean;
}

export interface NavigationRoute {
  steps: NavigationStep[];
  error?: string;
}

const DIRECTION_COMMANDS: Readonly<Record<string, string>> = {
  north: "n",
  east: "e",
  south: "s",
  west: "w",
  up: "u",
  down: "d",
  northeast: "ne",
  northwest: "nw",
  southeast: "se",
  southwest: "sw",
  in: "in",
  out: "out",
};

export function sameAreaId(left: NavigationAreaId, right: NavigationAreaId): boolean {
  return left === right;
}

function exitCommand(exit: NavigationExit): string | undefined {
  const explicit = exit.command?.trim();
  if (explicit) return explicit;
  return DIRECTION_COMMANDS[exit.from_direction.trim().toLowerCase()];
}

/** Convert mapper path keys into concrete commands, validating every hop. */
export function buildNavigationRoute(
  path: readonly (readonly [NavigationAreaId, number])[],
  roomAt: (area: NavigationAreaId, room: number) => NavigationRoom | undefined,
): NavigationRoute {
  const steps: NavigationStep[] = [];

  for (let index = 0; index < path.length - 1; index += 1) {
    const from = path[index];
    const to = path[index + 1];
    const room = roomAt(from[0], from[1]);
    if (!room) {
      return { steps, error: `mapped room ${from[1]} disappeared while resolving the route` };
    }

    const exit = room.exits.find((candidate) =>
      candidate.to_area_id !== null &&
      candidate.to_room_number === to[1] &&
      sameAreaId(candidate.to_area_id, to[0])
    );
    if (!exit) {
      return { steps, error: `mapped path has no exit from room ${from[1]} to ${to[1]}` };
    }

    const command = exitCommand(exit);
    if (!command) {
      return {
        steps,
        error: `the ${exit.from_direction} exit from room ${from[1]} has no traversal command`,
      };
    }

    steps.push({
      from,
      to,
      direction: exit.from_direction,
      command,
      closed: exit.is_closed === true,
      locked: exit.is_locked === true,
    });
  }

  return { steps };
}

/** Split NF's single-semicolon arrival-command syntax. */
export function splitArrivalCommands(raw: string): string[] {
  return raw.split(";").map((command) => command.trim()).filter(Boolean);
}

/** Read one bare or quoted command argument and return the untouched remainder. */
export function takeCommandArgument(raw: string): { value: string; rest: string } | undefined {
  const input = raw.trimStart();
  if (!input) return undefined;

  const quote = input[0] === "\"" || input[0] === "'" ? input[0] : undefined;
  if (!quote) {
    const whitespace = input.search(/\s/);
    return whitespace < 0
      ? { value: input, rest: "" }
      : { value: input.slice(0, whitespace), rest: input.slice(whitespace).trimStart() };
  }

  let value = "";
  let escaped = false;
  for (let index = 1; index < input.length; index += 1) {
    const character = input[index];
    if (escaped) {
      value += character;
      escaped = false;
    } else if (character === "\\") {
      escaped = true;
    } else if (character === quote) {
      return { value, rest: input.slice(index + 1).trimStart() };
    } else {
      value += character;
    }
  }

  // Treat an unmatched quote as part of the room name instead of silently
  // discarding the user's target.
  return { value: input, rest: "" };
}

export function formatRoute(steps: readonly Pick<NavigationStep, "command">[]): string {
  return steps.map(({ command }) => command).join(" ; ");
}
