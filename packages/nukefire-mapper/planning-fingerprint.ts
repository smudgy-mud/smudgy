export interface PlanningFingerprintExit {
  direction: string;
  toRoomNumber: number;
}

export interface PlanningFingerprintRoom {
  roomNumber: number;
  vnum?: number;
  position: {
    x: number;
    y: number;
    level: number;
  };
  movable: boolean;
  internalExits: readonly PlanningFingerprintExit[];
}

function exitKey(exit: Readonly<PlanningFingerprintExit>): string {
  return JSON.stringify([exit.direction, exit.toRoomNumber]);
}

/**
 * Canonicalize the live area inputs that can affect integral-grid planning.
 * Host room/exit enumeration order is deliberately ignored.
 */
export function planningFingerprint(rooms: readonly PlanningFingerprintRoom[]): string {
  return JSON.stringify(
    rooms
      .map((room) => ({
        roomNumber: room.roomNumber,
        vnum: room.vnum ?? null,
        position: [room.position.x, room.position.y, room.position.level],
        movable: room.movable,
        internalExits: room.internalExits.map(exitKey).sort(),
      }))
      .sort((a, b) => a.roomNumber - b.roomNumber),
  );
}
