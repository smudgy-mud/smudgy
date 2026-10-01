/** The room placement needed to order ports along a target wall. */
export interface ConnectionPortRoomPosition {
  x: number;
  y: number;
}

/**
 * One same-level, two-ended NukeFire connection whose ports may participate
 * in arrival disambiguation.
 *
 * `oneWayOriginRoom` must be the exact room which owns the Connection's sole
 * member exit. Omit it for reciprocal, protected, or otherwise ambiguous
 * topology; those endpoints remain fixed but still reserve their wall slots.
 */
export interface OneWayPortConnection {
  key: string;
  endpointA: ConnectionEndpoint;
  endpointB: ConnectionEndpoint;
  positionA: ConnectionPortRoomPosition;
  positionB: ConnectionPortRoomPosition;
  oneWayOriginRoom?: RoomNumber;
  /** Keep both endpoints fixed even when exact one-way membership is known. */
  protected?: boolean;
}

export interface DisambiguatedConnectionPorts {
  endpointA: ConnectionEndpoint;
  endpointB: ConnectionEndpoint;
}

/**
 * Manual routing marks an author-drawn centerline; every generated route is
 * stored as solver-produced. Like Manual ports, a Manual route is user-owned:
 * route recomputation leaves its geometry untouched.
 */
export function routeIsManuallyAuthored(routing: ConnectionRouting): boolean {
  return routing === "Manual";
}

/**
 * A Connection that runs straight but still stores route points keeps a drawn
 * route dormant, for switching back to restore. The mapper stores points only
 * with its own `Automatic` routes and drops them when it straightens one, so
 * dormant points are an author's, and route recomputation leaves them alone
 * as it does a `Manual` route.
 */
export function routeIsDormant(
  routing: ConnectionRouting,
  routePoints: readonly unknown[],
): boolean {
  return (routing === "Simple" || routing === "Stub") && routePoints.length > 0;
}

/** Manual ports keep their authored wall; only AutoPinned ports follow routing. */
export function routedEndpointSide(
  endpoint: Readonly<ConnectionEndpoint>,
  automaticSide: ConnectionEndpoint["side"],
): ConnectionEndpoint["side"] {
  return endpoint.port_mode === "Manual" ? endpoint.side : automaticSide;
}

type EndpointRole = "a" | "b";

interface EndpointOccupant {
  connection: OneWayPortConnection;
  endpoint: ConnectionEndpoint;
  role: EndpointRole;
  movableArrival: boolean;
  projection: number;
}

const HOME_OFFSET = 0.5;
const EARLY_OFFSET = 0.2;
const LATE_OFFSET = 0.8;
const OFFSET_EPSILON = 1e-6;

function endpointCopy(endpoint: ConnectionEndpoint): ConnectionEndpoint {
  return {
    room_number: endpoint.room_number,
    side: endpoint.side,
    port_offset: endpoint.port_offset,
    port_mode: endpoint.port_mode,
  };
}

function wallKey(endpoint: ConnectionEndpoint): string {
  return `${endpoint.room_number}\u0000${endpoint.side}`;
}

function arrivalRole(connection: OneWayPortConnection): EndpointRole | undefined {
  if (connection.protected || connection.oneWayOriginRoom === undefined) return undefined;
  const aIsOrigin = connection.endpointA.room_number === connection.oneWayOriginRoom;
  const bIsOrigin = connection.endpointB.room_number === connection.oneWayOriginRoom;
  // This also excludes self-loops and corrupt/ambiguous endpoint pairs.
  if (aIsOrigin === bIsOrigin) return undefined;
  return aIsOrigin ? "b" : "a";
}

function projectedSourceBearing(
  connection: OneWayPortConnection,
  targetRole: EndpointRole,
): number {
  const origin = connection.endpointA.room_number === connection.oneWayOriginRoom
    ? connection.positionA
    : connection.positionB;
  const destination = targetRole === "a"
    ? connection.positionA
    : connection.positionB;
  const target = targetRole === "a" ? connection.endpointA : connection.endpointB;
  const value = target.side === "North" || target.side === "South"
    ? origin.x - destination.x
    : origin.y - destination.y;
  return Number.isFinite(value) ? value : 0;
}

function usesOffset(occupants: readonly EndpointOccupant[], offset: number): boolean {
  return occupants.some((occupant) =>
    !occupant.movableArrival &&
    Math.abs(occupant.endpoint.port_offset - offset) <= OFFSET_EPSILON
  );
}

function assignCanonicalSlots(
  occupants: readonly EndpointOccupant[],
  assign: (occupant: EndpointOccupant, offset: number) => void,
): void {
  const arrivals = occupants
    .filter((occupant) => occupant.movableArrival)
    .sort((a, b) =>
      a.projection - b.projection ||
      a.connection.key.localeCompare(b.connection.key) ||
      a.role.localeCompare(b.role)
    );
  if (arrivals.length === 0 || occupants.length < 2) return;
  const fixedAtHome = occupants.filter((occupant) =>
    !occupant.movableArrival &&
    Math.abs(occupant.endpoint.port_offset - HOME_OFFSET) <= OFFSET_EPSILON
  ).length;
  // An endpoint elsewhere on the same wall is already visually distinct. Fan
  // arrivals only when their semantic midpoint homes would actually collide.
  if (arrivals.length + fixedAtHome < 2) return;

  const available = [EARLY_OFFSET, HOME_OFFSET, LATE_OFFSET].filter((offset) =>
    !usesOffset(occupants, offset)
  );
  if (available.length === 0) return;

  if (arrivals.length === 1) {
    // Fan the arrival toward its source along the target wall. A centered or
    // missing bearing uses the early lane as the stable tie-break. The home
    // slot is occupied in this branch because a lone arrival only reaches it
    // when a fixed endpoint also occupies the midpoint.
    const preferred = arrivals[0].projection > 0 ? LATE_OFFSET : EARLY_OFFSET;
    const fallback = arrivals[0].projection > 0 ? EARLY_OFFSET : LATE_OFFSET;
    const selected = available.includes(preferred)
      ? preferred
      : available.includes(fallback)
      ? fallback
      : available[0];
    assign(arrivals[0], selected);
    return;
  }

  // Source order maps monotonically onto wall order, preventing two arrivals
  // from crossing each other in the short fan immediately outside the room.
  // Two arrivals take the widest free pair; three use all canonical lanes.
  const selected = arrivals.length < available.length
    ? [available[0], available[available.length - 1]]
    : available;
  for (let index = 0; index < Math.min(arrivals.length, selected.length); index += 1) {
    assign(arrivals[index], selected[index]);
  }
}

/**
 * Repositions crowded AutoPinned one-way *arrival* ports without changing
 * reciprocal, protected, Manual, or one-way origin ports.
 *
 * Every input key is returned. Eligible NukeFire arrivals first heal to their
 * midpoint home (`0.5`); when at least one other endpoint occupies the same
 * room wall, they fan across the canonical editor lanes (`0.2`/`0.5`/`0.8`)
 * ordered by source projection and then key. A reciprocal or Manual midpoint
 * reserves `0.5`, leaving one-way arrivals on either side. Recomputing from
 * those homes makes the result independent of input order and idempotent, and
 * recenters a formerly crowded arrival after its wall-mates disappear.
 */
export function disambiguateOneWayArrivalPorts(
  connections: readonly OneWayPortConnection[],
): Map<string, DisambiguatedConnectionPorts> {
  const result = new Map<string, DisambiguatedConnectionPorts>();
  const cohorts = new Map<string, EndpointOccupant[]>();

  const ordered = [...connections].sort((a, b) => a.key.localeCompare(b.key));
  for (const connection of ordered) {
    if (result.has(connection.key)) {
      throw new Error(`duplicate Connection port key: ${connection.key}`);
    }
    const endpointA = endpointCopy(connection.endpointA);
    const endpointB = endpointCopy(connection.endpointB);
    const resolved = { endpointA, endpointB };
    result.set(connection.key, resolved);

    const movableRole = arrivalRole(connection);
    for (const role of ["a", "b"] as const) {
      const endpoint = role === "a" ? endpointA : endpointB;
      const movableArrival = role === movableRole && endpoint.port_mode === "AutoPinned";
      if (movableArrival) endpoint.port_offset = HOME_OFFSET;
      const occupant: EndpointOccupant = {
        connection,
        endpoint,
        role,
        movableArrival,
        projection: movableArrival
          ? projectedSourceBearing(connection, role)
          : 0,
      };
      const cohort = cohorts.get(wallKey(endpoint)) ?? [];
      cohort.push(occupant);
      cohorts.set(wallKey(endpoint), cohort);
    }
  }

  for (const occupants of cohorts.values()) {
    assignCanonicalSlots(occupants, (occupant, offset) => {
      occupant.endpoint.port_offset = offset;
    });
  }
  return result;
}
