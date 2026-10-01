/**
 * Regenerate `realistic-ratchet.json` from the realistic-map corpus.
 *
 * Run from this directory: `node realistic-ratchet-update.mjs`
 *
 * The ratchet records each fixture/pipeline's full public quality tuple; the
 * companion test (`realistic-ratchet.test.ts`) requires the live engine to be
 * equal or better in the engine's quality order, so rewriting the file is how
 * a genuine improvement is banked. The per-scenario summary below shows every old→new
 * tuple for human review before the new records are committed. Output is
 * stable — sorted keys, two-space indent, LF endings — so a regeneration diff
 * shows exactly which fields moved.
 */

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  normalizedQuality,
  QUALITY_TUPLE_FIELDS,
  realisticScenarios,
} from "./realistic-fixtures.ts";
import { compareLayoutQuality } from "./layout.ts";

const RATCHET_PATH = fileURLToPath(new URL("./realistic-ratchet.json", import.meta.url));

function compareStrings(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Canonical JSON: code-unit-sorted keys, two-space indent, LF endings. */
function stableStringify(value, indent = "") {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  const child = `${indent}  `;
  if (Array.isArray(value)) {
    if (value.length === 0) return "[]";
    const rows = value.map((entry) => `${child}${stableStringify(entry, child)}`);
    return `[\n${rows.join(",\n")}\n${indent}]`;
  }
  const keys = Object.keys(value)
    .filter((key) => value[key] !== undefined)
    .sort(compareStrings);
  if (keys.length === 0) return "{}";
  const rows = keys.map((key) =>
    `${child}${JSON.stringify(key)}: ${stableStringify(value[key], child)}`
  );
  return `{\n${rows.join(",\n")}\n${indent}}`;
}

function digest(quality) {
  if (!quality) return "(none)";
  return `[${QUALITY_TUPLE_FIELDS.map((field) => quality[field] ?? 0).join(",")}]`;
}

const previous = existsSync(RATCHET_PATH)
  ? JSON.parse(readFileSync(RATCHET_PATH, "utf8"))
  : { formatVersion: 1, scenarios: {} };

const scenarios = realisticScenarios();
const results = {};
const totalStarted = performance.now();
let nameWidth = 0;
for (const scenario of scenarios) nameWidth = Math.max(nameWidth, scenario.name.length);
for (const scenario of scenarios) {
  const started = performance.now();
  const quality = normalizedQuality(scenario.run());
  const elapsed = performance.now() - started;
  results[scenario.name] = {
    aspiration: scenario.aspiration,
    rooms: scenario.rooms,
    edges: scenario.edges,
    quality,
  };
  const old = previous.scenarios?.[scenario.name]?.quality;
  const movement = old === undefined
    ? "NEW"
    : compareLayoutQuality(quality, old) > 0
    ? "IMPROVED"
    : compareLayoutQuality(quality, old) < 0
    ? "REGRESSED"
    : "unchanged";
  console.log(
    scenario.name.padEnd(nameWidth),
    `${elapsed.toFixed(0)}ms`.padStart(8),
    movement.padEnd(10),
    old === undefined ? digest(quality) : `${digest(old)} -> ${digest(quality)}`,
  );
}

const ratchet = { formatVersion: 1, scenarios: results };
const text = `${stableStringify(ratchet)}\n`;
writeFileSync(RATCHET_PATH, text, { encoding: "utf8" });
console.log(
  `\nwrote ${RATCHET_PATH} (${text.length} bytes, ${scenarios.length} scenarios) ` +
    `in ${((performance.now() - totalStarted) / 1000).toFixed(1)}s`,
);
