import assert from "node:assert/strict";
import test from "node:test";
import { deleteDockingRecord, deleteShip, historyFor, openShipDatabase, recordSighting, shipsFor } from "./shipdb-storage.ts";
import { tempDir } from "./support.ts";

test("ShipDB storage: sightings upsert the registry and always append history", () => {
  const dir = tempDir();
  const db = openShipDatabase({ name: "shipdatabase", directory: dir.path });
  try {
    assert.match(db.path, /Database_shipdatabase\.db$/);
    const first = recordSighting(db, { character: "Han", name: "Millennium Falcon", type: "YT-1300", owner: "Lando", planet: "Bespin", dock: "Cloud City", time: "2026-09-07 10:00" });
    assert.equal(first.kind, "added");
    assert.equal(first.ship._row_id, 1);

    const second = recordSighting(db, { character: "Han", name: "Millennium Falcon", type: "YT-1300", owner: "Han", planet: "Tatooine", dock: "Mos Eisley 94", time: "2026-09-07 11:00" });
    assert.equal(second.kind, "updated");
    if (second.kind === "updated") {
      assert.deepEqual(second.changes.map((change) => change.field), ["owner", "planet", "dock", "time"]);
      assert.deepEqual(second.changes[0], { field: "owner", from: "Lando", to: "Han" });
    }

    recordSighting(db, { character: "Leia", name: "Tantive IV", type: "Corvette", owner: "Alderaan", planet: "Alderaan", dock: "Royal", time: "2026-09-07 12:00" });
    recordSighting(db, { character: "Han", name: "Millennium Falcon", type: "YT-1300", owner: "Han", planet: "Tatooine", dock: "Mos Eisley 94", time: "2026-09-07 11:00" });

    const hans = shipsFor(db, "Han");
    assert.equal(hans.length, 1, "one registry row per (character, name)");
    assert.equal(hans[0]._row_id, 1, "the row keeps its identity across updates");
    assert.equal(hans[0].dock, "Mos Eisley 94");
    assert.equal(shipsFor(db, "Leia").length, 1);
    assert.equal(historyFor(db, "Han").length, 3);
    assert.equal(historyFor(db, "Han", "Millennium Falcon").length, 3);
    assert.equal(historyFor(db, "Leia").length, 1);

    const [oldest] = historyFor(db, "Han");
    deleteDockingRecord(db, oldest);
    assert.equal(historyFor(db, "Han").length, 2);

    assert.deepEqual(deleteShip(db, "Han", "Millennium Falcon"), { ship: true, records: 2 });
    assert.deepEqual(deleteShip(db, "Han", "Millennium Falcon"), { ship: false, records: 0 });
    assert.equal(shipsFor(db, "Han").length, 0);
    assert.equal(historyFor(db, "Han").length, 0);
    assert.equal(historyFor(db, "Leia").length, 1, "another character's history is untouched");
  } finally {
    db.close();
    dir.remove();
  }
});
