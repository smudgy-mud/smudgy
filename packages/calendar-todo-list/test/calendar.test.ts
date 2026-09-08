// Drives the package as installed: the aliases index.ts registers are called
// with the captures Smudgy would pass, and their echoes are checked.

import assert from "node:assert/strict";
import test from "node:test";
import { register } from "node:module";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

register("./smudgy-loader.mjs", import.meta.url);

interface Alias {
  readonly name: string;
  readonly pattern: RegExp;
  readonly script: (matches: Record<string, string | undefined>) => void;
}

interface Stub {
  dataDir: string;
  echoed: string[];
  aliases: Alias[];
}

const dataDir = mkdtempSync(join(tmpdir(), "calendar-"));
const stub: Stub = { dataDir, echoed: [], aliases: [] };
(globalThis as { __smudgyStub?: Stub }).__smudgyStub = stub;

await import("../index.ts");

/** Type a line the way the input would: the first alias whose pattern matches runs. */
function type(line: string): string[] {
  stub.echoed = [];
  const alias = stub.aliases.find((candidate) => candidate.pattern.test(line));
  assert.ok(alias, `no alias matches ${JSON.stringify(line)}`);
  const match = alias.pattern.exec(line)!;
  alias.script({ ...match.groups });
  return stub.echoed;
}

test("the package registers the six aliases of the source and creates the database", () => {
  assert.deepEqual(
    stub.aliases.map((alias) => alias.name),
    ["add todo", "done todo", "reset done", "add event", "delete event", "recycle old events"],
  );
  assert.ok(existsSync(join(dataDir, "Database_calendar.db")));
});

test("todo: add, list, complete by position, list completed, reset", () => {
  assert.deepEqual(type("todo visit shipyard"), ["To-do item 'visit shipyard' added to the database."]);
  assert.deepEqual(type("todo buy bait"), ["To-do item 'buy bait' added to the database."]);

  const listing = type("todo");
  assert.equal(listing.length, 6, "rule, header, rule, two rows, rule");
  assert.equal(listing[0], "%".repeat(80));
  assert.equal(listing[1], "%% ID     %% Todo                                 %% Date (since)             %%");
  assert.match(listing[3], /^%% 1 {6}%% visit shipyard {23}%% \d\d\/\d\d\/\d\d \d\d:\d\d:\d\d [AP]M {5}%%$/);
  assert.match(listing[4], /^%% 2 {6}%% buy bait/);

  assert.deepEqual(type("done 1"), ["To-do item 'visit shipyard' marked as done."]);
  assert.deepEqual(type("done 9"), ["Could not find entry #9 in database."]);

  const open = type("todo");
  assert.equal(open.length, 5);
  assert.match(open[3], /^%% 2 {6}%% buy bait/, "positions count the completed row, as the source's ipairs did");

  const done = type("done");
  assert.match(done[1], /Date \(done\)/);
  assert.match(done[3], /^%% 1 {6}%% visit shipyard/);

  assert.deepEqual(type("reset done"), ["Cleared all todos marked as complete."]);
  assert.match(type("todo")[3], /^%% 1 {6}%% buy bait/, "the remaining row moved up to position 1");
});

test("event: add, list with [TODAY], reject malformed input, delete, recycle stale ones", () => {
  const now = new Date();
  const two = (value: number) => String(value).padStart(2, "0");
  const todayText = `${two(now.getDate())}/${two(now.getMonth() + 1)}/${two(now.getFullYear() % 100)}`;

  assert.deepEqual(type("event Council = 01/01/01"), ["Event 'Council' at '01/01/01' added to the database."]);
  assert.deepEqual(type(`event Market day = ${todayText}`), [`Event 'Market day' at '${todayText}' added to the database.`]);
  assert.deepEqual(type("event Feast = 31/12/99"), ["Event 'Feast' at '31/12/99' added to the database."]);
  assert.deepEqual(type("event Picnic = someday"), ["Event 'Picnic' at 'someday' added to the database."]);
  assert.deepEqual(type("event no separator here"), ["An event is written as: event <what> = <dd/mm/yy>"]);

  const listing = type("event");
  assert.equal(listing.length, 8);
  assert.equal(listing[3], "%% 1      %% Council                              %% 01/01/01                 %%");
  assert.match(listing[4], new RegExp(`^%% 2 {6}%% Market day {27}%% ${todayText} \\[TODAY\\]`));
  assert.match(listing[6], /^%% 4 {6}%% Picnic {31}%% someday/, "an unparseable date lists without a marker");

  assert.deepEqual(type("delevent 3"), ["Event 'Feast' at '31/12/99' deleted from database."]);
  assert.deepEqual(type("delevent 9"), ["Could not find entry #9 in database."]);

  assert.deepEqual(type("recycle events"), ["Event 'Council' at '01/01/01' recycled."]);
  const after = type("event");
  assert.equal(after.length, 6, `today's event and the undated one stay:\n${after.join("\n")}`);
  assert.match(after[3], /^%% 1 {6}%% Market day/);
  assert.match(after[4], /^%% 2 {6}%% Picnic/);
});

test.after(() => {
  // The package holds Database_calendar.db open for its lifetime, as it does when installed;
  // Windows refuses to remove an open file, so a leftover temp directory is accepted there.
  try {
    rmSync(dataDir, { recursive: true, force: true });
  } catch {
    // see above
  }
});
