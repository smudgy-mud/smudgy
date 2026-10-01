import assert from "node:assert/strict";
import test from "node:test";
import { AreaNames, fold, rulesToSeed } from "./area-names.ts";
import { NUKEFIRE_AREA_NAME_RULES } from "./nukefire-maps.ts";

const defaults = new AreaNames(NUKEFIRE_AREA_NAME_RULES);

test("a name without a rule is its own map, whatever its case or surrounding space", () => {
  assert.deepEqual(defaults.resolve("  The Deathlands "), { key: "the deathlands", display: "The Deathlands" });
  assert.equal(defaults.resolve("THE DEATHLANDS")?.key, "the deathlands");
});

test("canonically equivalent Unicode spellings share a map", () => {
  const composed = defaults.resolve("Café Noir");
  const decomposed = defaults.resolve("Café NOIR");
  assert.equal(composed?.key, decomposed?.key);
  assert.equal(decomposed?.display, "Café NOIR");
});

test("lowercasing follows Unicode, including a word-final sigma", () => {
  assert.equal(fold("ΟΔΟΣ"), "οδος");
});

test("the default rules fold numbered and renamed areas into their family's map", () => {
  assert.deepEqual(defaults.resolve("Vega Jane IV"), { key: "vega jane", display: "Vega Jane" });
  assert.deepEqual(defaults.resolve("jurassic WORLD"), { key: "jurassic park", display: "Jurassic Park" });
  assert.deepEqual(defaults.resolve("SST - Federation Station"), { key: "sst", display: "SST" });
});

test("an exact-case rule tells two spellings apart", () => {
  assert.deepEqual(defaults.resolve("DARK PLEASURES"), { key: "darker pleasures", display: "Darker Pleasures" });
  assert.equal(defaults.resolve("Dark Pleasures")?.key, "dark pleasures");
  assert.equal(defaults.resolve("Dark PLEASURES")?.key, "dark pleasures");
});

test("an exact-case rule wins over a case-insensitive rule for the same spelling", () => {
  const names = new AreaNames([
    { name: "Xyz", map: "Any case", exactCase: false },
    { name: "XYZ", map: "Exact", exactCase: true },
  ]);
  assert.equal(names.resolve("XYZ")?.display, "Exact");
  assert.equal(names.resolve("xyz")?.display, "Any case");
});

test("exactly one rule applies: rules never chain", () => {
  const names = new AreaNames([
    { name: "Hermit's Knob", map: "Eastern Forest", exactCase: false },
    { name: "Eastern Forest", map: "Great Forest", exactCase: false },
  ]);
  assert.equal(names.resolve("Hermit's Knob")?.display, "Eastern Forest");
});

test("an empty area name belongs to no map", () => {
  assert.equal(defaults.resolve(""), undefined);
  assert.equal(defaults.resolve("  ﻿"), undefined);
});

test("unusable rows are skipped and reported while the rest still apply", () => {
  const names = new AreaNames([
    { name: "", map: "Somewhere", exactCase: false },
    { name: "Foo", map: "  ", exactCase: false },
    { name: "Foo", map: "Bar", exactCase: false },
    { name: "FOO", map: "Baz", exactCase: false },
  ]);
  assert.deepEqual(names.problems, [
    "row 1: the area name is empty",
    "row 2: the map name is empty",
    'row 4: "FOO" already has a rule',
  ]);
  assert.equal(names.resolve("foo")?.display, "Bar");
});

test("a settings value that is not a table means no rules", () => {
  const names = new AreaNames(null);
  assert.deepEqual(names.problems, []);
  assert.equal(names.resolve("Vega Jane II")?.key, "vega jane ii");
});

test("every default rule is usable and sends its name to its map", () => {
  assert.deepEqual(defaults.problems, []);
  for (const rule of NUKEFIRE_AREA_NAME_RULES) {
    assert.equal(defaults.resolve(rule.name)?.key, fold(rule.map), rule.name);
  }
});

test("an unset rules table is seeded with the defaults", () => {
  assert.equal(rulesToSeed(NUKEFIRE_AREA_NAME_RULES, undefined, undefined), NUKEFIRE_AREA_NAME_RULES);
  assert.equal(rulesToSeed(NUKEFIRE_AREA_NAME_RULES, null, null), NUKEFIRE_AREA_NAME_RULES);
});

test("an untouched table follows new defaults, whatever order its cells were stored in", () => {
  const older = NUKEFIRE_AREA_NAME_RULES.slice(0, 3);
  const stored = older.map(({ name, map, exactCase }) => ({ exactCase, map, name }));
  assert.equal(rulesToSeed(NUKEFIRE_AREA_NAME_RULES, stored, older), NUKEFIRE_AREA_NAME_RULES);
  assert.equal(rulesToSeed(NUKEFIRE_AREA_NAME_RULES, NUKEFIRE_AREA_NAME_RULES, NUKEFIRE_AREA_NAME_RULES), undefined);
});

test("a table the player edited, emptied, or never saw seeded is theirs", () => {
  const edited = [...NUKEFIRE_AREA_NAME_RULES, { name: "Hermit's Knob", map: "Eastern Forest", exactCase: false }];
  assert.equal(rulesToSeed(NUKEFIRE_AREA_NAME_RULES, edited, NUKEFIRE_AREA_NAME_RULES), undefined);
  assert.equal(rulesToSeed(NUKEFIRE_AREA_NAME_RULES, [], NUKEFIRE_AREA_NAME_RULES), undefined);
  assert.equal(rulesToSeed(NUKEFIRE_AREA_NAME_RULES, NUKEFIRE_AREA_NAME_RULES.slice(0, 3), null), undefined);
});
