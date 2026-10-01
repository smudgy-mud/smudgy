/**
 * Regenerate `layout-golden.json` from the shared scenario corpus.
 *
 * Run from this directory: `node layout-golden-update.mjs`
 *
 * The golden file pins the engine's exact outputs, so rewriting it is a
 * deliberate act: the per-scenario summary below shows every quality tuple
 * and a work-counter digest for human review before the new goldens are
 * committed. Output is stable — sorted keys, two-space indent, LF endings —
 * so a regeneration diff shows exactly which fields moved.
 */

import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { goldenScenarios } from "./layout-golden-scenarios.ts";

const GOLDEN_PATH = fileURLToPath(new URL("./layout-golden.json", import.meta.url));

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

/** The quality tuple in declaration order. */
const QUALITY_FIELDS = [
  "levelViolations",
  "cardinalRayViolations",
  "reciprocalRayViolations",
  "routingViolations",
  "exitPortViolations",
  "reciprocalExitPortViolations",
  "roomObstructions",
  "linkCrossings",
  "levelSlack",
  "footprintArea",
  "footprintPerimeter",
  "cardinalSlack",
];

function qualityDigest(result) {
  const quality = result?.plan?.quality ?? result?.quality;
  if (!quality) return "(no quality)";
  return `[${QUALITY_FIELDS.map((field) => quality[field] ?? 0).join(",")}]`;
}

function counterDigest(result) {
  const parts = [];
  const events = result?.trace?.events;
  if (events) {
    const total = Object.values(events).reduce((sum, count) => sum + count, 0);
    parts.push(`events=${total}`);
  }
  const batches = result?.trace?.batches;
  if (batches) {
    const generated = Object.values(batches).reduce((sum, batch) => sum + batch.generated, 0);
    parts.push(`generated=${generated}`);
  }
  if (result?.stats) {
    parts.push(`macros=${result.stats.macrosConsidered}`, `visited=${result.stats.visitedStates}`);
  }
  const report = result?.report;
  if (report) {
    parts.push(
      `restarts=${report.restarts}`,
      `feasibility=${report.feasibilityChecks}`,
      `sepStates=${report.separatorStates}`,
      `crossingMacros=${report.crossingRepair.macrosConsidered}`,
      `cutoff=${report.cutoff}`,
    );
  }
  if (typeof result?.passes === "number") {
    parts.push(`passes=${result.passes}`, `improvements=${result.improvements}`);
  }
  return parts.join(" ");
}

const scenarios = goldenScenarios();
const results = {};
const totalStarted = performance.now();
let nameWidth = 0;
for (const scenario of scenarios) nameWidth = Math.max(nameWidth, scenario.name.length);
for (const scenario of scenarios) {
  const started = performance.now();
  const result = JSON.parse(JSON.stringify(scenario.run()));
  const elapsed = performance.now() - started;
  results[scenario.name] = result;
  console.log(
    scenario.name.padEnd(nameWidth),
    `${elapsed.toFixed(0)}ms`.padStart(8),
    qualityDigest(result).padEnd(32),
    counterDigest(result),
  );
}

const golden = { formatVersion: 1, scenarios: results };
const text = `${stableStringify(golden)}\n`;
writeFileSync(GOLDEN_PATH, text, { encoding: "utf8" });
console.log(
  `\nwrote ${GOLDEN_PATH} (${text.length} bytes, ${scenarios.length} scenarios) ` +
    `in ${((performance.now() - totalStarted) / 1000).toFixed(1)}s`,
);
