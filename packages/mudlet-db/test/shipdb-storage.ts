// The storage layer of ShipDB (Xavious, MIT, mudlet-package-repository) on
// top of official/mudlet-db: the `ships` registry with one row per
// (character, ship name) and the `docking_records` history with one row per
// sighting. This is the part of ShipDB that touches `db:`; its triggers,
// GMCP character lookup and Geyser window are not ported here.
//
// Kept from the source: the two-sheet schema with every column a text
// default of "" (including `time`, which ShipDB fills with a formatted
// string, not a db:Timestamp), the upsert on (character, name), the
// unconditional history row per sighting, and the delete that removes a
// ship together with its history.
//
// Changed on purpose: the source ignored errors from db:add/db:update
// (their `err` return is never set by DB.lua on success and callers only
// printed it); here a failure throws. `recordSighting` returns what changed
// so the caller can print the "field: old -> new" lines the source printed
// inline.

import { eq, openDatabase, type Database, type DatabaseLocation, type Row } from "../index.ts";

export const SHIPDB_SCHEMA = {
  ships: { character: "", name: "", type: "", owner: "", planet: "", dock: "", time: "" },
  docking_records: { character: "", name: "", type: "", owner: "", planet: "", dock: "", time: "" },
} as const;

export type ShipDatabase = Database<typeof SHIPDB_SCHEMA>;
export type Ship = Row<typeof SHIPDB_SCHEMA.ships>;
export type DockingRecord = Row<typeof SHIPDB_SCHEMA.docking_records>;

export interface Sighting {
  readonly character: string;
  readonly name: string;
  readonly type: string;
  readonly owner: string;
  readonly planet: string;
  readonly dock: string;
  /** ShipDB stored `getTime(true, "yyyy-MM-dd hh:mm")`; any text is accepted. */
  readonly time: string;
}

export type SightingOutcome =
  | { readonly kind: "added"; readonly ship: Ship }
  | { readonly kind: "updated"; readonly ship: Ship; readonly changes: ReadonlyArray<{ field: keyof Sighting; from: string | null; to: string }> };

const SIGHTING_FIELDS = ["type", "owner", "planet", "dock", "time"] as const;

/** ShipDB's `db:create(shipdb.config.db_name, {...})`; the name was "shipdatabase". */
export function openShipDatabase(location: DatabaseLocation): ShipDatabase {
  return openDatabase(location, SHIPDB_SCHEMA);
}

/** `shipdb.triggerShipLocated` without the trigger, GMCP and window parts. */
export function recordSighting(db: ShipDatabase, sighting: Sighting): SightingOutcome {
  return db.transaction(() => {
    const { ships, docking_records } = db.sheets;
    const existing = ships.fetchOne([eq(ships.fields.character, sighting.character), eq(ships.fields.name, sighting.name)]);
    let outcome: SightingOutcome;
    if (existing) {
      const changes = SIGHTING_FIELDS.filter((field) => existing[field] !== sighting[field]).map((field) => ({
        field,
        from: existing[field],
        to: sighting[field],
      }));
      const ship = { ...existing, ...sighting, _row_id: existing._row_id };
      ships.update(ship);
      outcome = { kind: "updated", ship, changes };
    } else {
      const [rowId] = ships.add(sighting);
      outcome = { kind: "added", ship: { _row_id: rowId!, ...sighting } };
    }
    docking_records.add(sighting);
    return outcome;
  });
}

/** `shipdb.sortShips`: one character's ships; sorting stays with the caller as it did. */
export function shipsFor(db: ShipDatabase, character: string): Ship[] {
  return db.sheets.ships.fetch(eq(db.sheets.ships.fields.character, character));
}

/** `shipdb.updateHistoryView`'s query: all sightings for a character, or one ship's. */
export function historyFor(db: ShipDatabase, character: string, shipName?: string): DockingRecord[] {
  const f = db.sheets.docking_records.fields;
  const query = shipName === undefined ? [eq(f.character, character)] : [eq(f.character, character), eq(f.name, shipName)];
  return db.sheets.docking_records.fetch(query, { orderBy: f._row_id });
}

/** `shipdb.deleteShip`: the ship and every docking record it has. Returns what was removed. */
export function deleteShip(db: ShipDatabase, character: string, shipName: string): { ship: boolean; records: number } {
  return db.transaction(() => {
    const { ships, docking_records } = db.sheets;
    const ship = ships.fetchOne([eq(ships.fields.character, character), eq(ships.fields.name, shipName)]);
    if (!ship) return { ship: false, records: 0 };
    ships.delete(ship);
    const records = docking_records.delete([eq(docking_records.fields.character, character), eq(docking_records.fields.name, shipName)]);
    return { ship: true, records };
  });
}

/** `shipdb.deleteDockingRecord`: one history row, by the row a fetch returned. */
export function deleteDockingRecord(db: ShipDatabase, record: DockingRecord): void {
  db.sheets.docking_records.delete(record);
}
