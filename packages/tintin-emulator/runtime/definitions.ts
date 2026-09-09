// TinTin definition commands (#alias, #action, #gag, #highlight, #substitute,
// #ticker, #delay, #function, #macro, #event) mapped onto native smudgy
// automations, so they are visible in the automations window and compose with
// the user's own. TinTin's fire-one-by-priority is approximated with smudgy
// priority (-p * 1000, TinTin low-first -> smudgy high-first) and
// fallthrough: false.
//
// Definitions persist as raw TinTin text in the package's vars (sandboxed
// packages cannot reach userAutomations) and replay through the same define
// path on load. A definition made while a #class is open carries that class,
// and #class kill removes the class's definitions wholesale.

import {
  createAlias, createTrigger, createTimer, createHotkey, getProfile, line, sendRaw, vars,
} from "smudgy:core";
import type { Alias, Trigger, Timer, Hotkey, Matches } from "smudgy:core";
import {
  connect, disconnect, send as sentOutput, receive as receivedLine, submit as receivedInput,
} from "smudgy:events/sys";
import { parseTinTin } from "../engine/parse.ts";
import type { TinTinNode } from "../engine/parse.ts";
import { compileTinTinPattern, jsRegexSource } from "../engine/pattern.ts";
import type { CompiledPattern } from "../engine/pattern.ts";
import { highlightStyleOptions, occurrenceByteRanges } from "../engine/text.ts";
import { tinTinKeySpec, supportedKeyNames } from "../engine/keys.ts";
import { defaultPathDirs, expandLegacySpeedwalk } from "../engine/path.ts";
import type { PathDir } from "../engine/path.ts";
import { makeEnv } from "./env.ts";
import type { ExecutionEnv } from "./env.ts";
import { echoOk, echoError, echoNote, makeWarnSink, renderOutput } from "./output.ts";
import { hasSplit, splitEcho } from "./panes.ts";

export const STORE_KEY = "__tintin_definitions";

interface AliasDef { pattern: string; commands: string; priority: number; class?: string }
interface ActionDef { pattern: string; commands: string; priority: number; class?: string }
interface GagDef { pattern: string; class?: string }
interface HighlightDef { pattern: string; color: string; class?: string }
interface SubstituteDef { pattern: string; replacement: string; class?: string }
interface TickerDef { name: string; commands: string; seconds: number; class?: string }
interface FunctionDef { name: string; commands: string; class?: string }
interface MacroDef { key: string; commands: string; class?: string }
interface EventDef { name: string; commands: string; class?: string }
interface PromptDef { pattern: string; replacement: string; row: number | null; class?: string }

interface StoredDefinitions {
  aliases: Record<string, AliasDef>;
  actions: Record<string, ActionDef>;
  gags: Record<string, GagDef>;
  highlights: Record<string, HighlightDef>;
  substitutes: Record<string, SubstituteDef>;
  tickers: Record<string, TickerDef>;
  functions: Record<string, FunctionDef>;
  macros: Record<string, MacroDef>;
  events: Record<string, EventDef>;
  prompts: Record<string, PromptDef>;
  pathdirs: Record<string, PathDir>;
}

function emptyStore(): StoredDefinitions {
  return {
    aliases: {}, actions: {}, gags: {}, highlights: {}, substitutes: {},
    tickers: {}, functions: {}, macros: {}, events: {}, prompts: {}, pathdirs: {},
  };
}

export function getStored(): StoredDefinitions {
  const record = vars as Record<string, unknown>;
  const stored = record[STORE_KEY] as StoredDefinitions | undefined;
  if (!stored) return emptyStore();
  // A pre-param development build briefly persisted SPEEDWALK here. Ignore
  // that stale command-side setting; the manifest param is now authoritative.
  const definitions = { ...(stored as StoredDefinitions & { config?: unknown }) };
  delete definitions.config;
  return { ...emptyStore(), ...definitions };
}

function mutateStored(mutate: (stored: StoredDefinitions) => void): void {
  const stored = getStored();
  mutate(stored);
  (vars as Record<string, unknown>)[STORE_KEY] = stored;
}

let speedwalkHandle: Alias | null = null;

/**
 * Install SPEEDWALK as a lowest-priority native alias. Real TinTin checks
 * aliases before speedwalk syntax; the low priority preserves that ordering.
 */
export function syncSpeedwalkAutomation(
  enabled: boolean,
  onDirection?: (direction: string) => void,
): void {
  if (!enabled) {
    speedwalkHandle?.delete();
    speedwalkHandle = null;
    return;
  }
  if (speedwalkHandle) return;
  speedwalkHandle = createAlias(
    "^(?:\\d{0,3}[neswud])+$",
    (matches) => {
      const directions = expandLegacySpeedwalk(matches[0] ?? "");
      // A normal send would re-enter alias matching and recursively match
      // SPEEDWALK again. TinTin writes expanded directions straight to the MUD.
      if (directions) {
        for (const direction of directions) {
          // Raw sends intentionally skip sys:send, so tell #PATH about the
          // movement explicitly before it goes to the wire.
          onDirection?.(direction);
          sendRaw(direction + "\n");
        }
      }
    },
    {
      name: automationName(`${activeCommandChar}SPEEDWALK`),
      priority: -1_000_000,
      fallthrough: false,
    },
  );
}

// Live automation handles, keyed the TinTin way (by the pattern/name text).
const aliasHandles = new Map<string, Alias>();
const actionHandles = new Map<string, Trigger>();
const gagHandles = new Map<string, Trigger>();
const highlightHandles = new Map<string, Trigger>();
const substituteHandles = new Map<string, Trigger>();
const tickerHandles = new Map<string, Timer>();
const delayHandles = new Map<string, Timer>();
const macroHandles = new Map<string, Hotkey>();
const promptHandles = new Map<string, Trigger>();
const eventSubscriptions = new Map<string, { off(): void }>();
const functionBodies = new Map<string, TinTinNode[]>();
let unnamedDelayCounter = 0;

/** A control-flow outcome threaded up through body execution. */
export type Signal =
  | { kind: "break" }
  | { kind: "continue" }
  | { kind: "return"; value: string }
  | null;

/** The dispatcher's executeNodes, injected to avoid a module cycle. */
export type Executor = (nodes: TinTinNode[], env: ExecutionEnv, depth?: number) => Signal;
let execute: Executor = () => {
  throw new Error("tintin-emulator executor is not wired");
};
export function setExecutor(executor: Executor): void {
  execute = executor;
}

let activeCommandChar = "#";
export function setCommandChar(commandChar: string): void {
  activeCommandChar = commandChar;
}

function parseBody(commands: string): TinTinNode[] {
  return parseTinTin(commands, { commandChar: activeCommandChar }).nodes;
}

/** A fresh execution environment with @fn{} calls wired in. */
export function makeExecutionEnv(slots: Record<number, string> = {}): ExecutionEnv {
  return makeEnv({ slots, warn: makeWarnSink(), callFunction: callTinTinFunction });
}

function runBody(nodes: TinTinNode[], slots: Record<number, string>): void {
  // Depth 1: fired bodies are "inside" execution, so confirmations stay quiet
  // (TinTin's interactive/verbose distinction).
  try {
    execute(nodes, makeExecutionEnv(slots), 1);
  } catch (error) {
    echoError(`#ERROR: a fired body failed: ${error instanceof Error ? error.message : String(error)}.`);
  }
}

function seedVariables(name: string): unknown {
  return (vars as Record<string, unknown>)[name];
}

function compileDefinitionPattern(kind: string, patternText: string): CompiledPattern | null {
  const warn = makeWarnSink();
  const compiled = compileTinTinPattern(patternText, { variables: seedVariables });
  for (const warning of compiled.warnings) warn(`${kind} {${patternText}}: ${warning}`);
  if (compiled.unsupported) {
    echoError(`#ERROR: {${patternText}} needs ${compiled.unsupportedFeatures.join(", ")}, which smudgy's pattern engine cannot match.`);
    return null;
  }
  return compiled;
}

/** TinTin priorities run 1-9 (default 5), low first; smudgy runs high first. */
export function tinTinPriority(value: unknown): number {
  const priority = Number(value ?? 5);
  return Number.isFinite(priority) && priority >= 1 && priority <= 9 ? priority : 5;
}

/**
 * Automation names become file names, so smudgy rejects `< > : " / \ | ? *`,
 * control characters, and anything over 64 characters. TinTin pattern text
 * uses several of those freely (`^<PORT> %0`, `%?`), so the display name gets
 * scrubbed. Our own lookups key on the raw pattern; only the automations
 * window sees this.
 */
export function automationName(text: string): string {
  let name = text
    .replace(/[<>:"/\\|?*]/g, "")
    .replace(/[\u0000-\u001f]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (name.length > 64) name = `${name.slice(0, 61).trimEnd()}...`;
  name = name.replace(/^\.+|[. ]+$/g, "");
  return name || "tintin definition";
}

function automationOptions(name: string, priority: number) {
  return { name: automationName(name), priority: -Math.round(priority * 1000), fallthrough: false };
}

function captureSlots(compiled: CompiledPattern, matches: Matches): Record<number, string> {
  const slots: Record<number, string> = { 0: matches[0] ?? "" };
  for (const [slot, group] of Object.entries(compiled.captureMap)) {
    slots[Number(slot)] = matches[group] ?? "";
  }
  return slots;
}

function guardedByNegativeLiterals(compiled: CompiledPattern, text: string): boolean {
  return compiled.negativeLiterals.some((literal) => text.includes(literal));
}

// ---- Classes ---------------------------------------------------------------

let currentClass: string | null = null;

export function getCurrentClass(): string | null {
  return currentClass;
}

export function setCurrentClass(name: string | null): void {
  currentClass = name;
}

function withClass<T>(def: T, cls: string | null | undefined): T {
  const assigned = cls === undefined ? currentClass : cls;
  return assigned ? { ...def, class: assigned } : def;
}

/** Remove every definition tagged with `name`; returns how many went. */
export function classKill(name: string): number {
  let count = 0;
  mutateStored((stored) => {
    const sweep = <D extends { class?: string }>(
      table: Record<string, D>,
      drop: (key: string) => void,
    ) => {
      for (const [key, def] of Object.entries(table)) {
        if (def.class !== name) continue;
        drop(key);
        delete table[key];
        count += 1;
      }
    };
    sweep(stored.aliases, (key) => { aliasHandles.get(key)?.delete(); aliasHandles.delete(key); });
    sweep(stored.actions, (key) => { actionHandles.get(key)?.delete(); actionHandles.delete(key); });
    sweep(stored.gags, (key) => { gagHandles.get(key)?.delete(); gagHandles.delete(key); });
    sweep(stored.highlights, (key) => { highlightHandles.get(key)?.delete(); highlightHandles.delete(key); });
    sweep(stored.substitutes, (key) => { substituteHandles.get(key)?.delete(); substituteHandles.delete(key); });
    sweep(stored.tickers, (key) => { tickerHandles.get(key)?.delete(); tickerHandles.delete(key); });
    sweep(stored.functions, (key) => { functionBodies.delete(key); });
    sweep(stored.macros, (key) => { macroHandles.get(key)?.delete(); macroHandles.delete(key); });
    sweep(stored.events, (key) => { eventSubscriptions.get(key)?.off(); eventSubscriptions.delete(key); });
    sweep(stored.prompts, (key) => { promptHandles.get(key)?.delete(); promptHandles.delete(key); });
  });
  if (currentClass === name) currentClass = null;
  return count;
}

export function classSize(name: string): number {
  const stored = getStored();
  return Object.values(stored)
    .flatMap((table) => Object.values(table as Record<string, { class?: string }>))
    .filter((def) => def.class === name).length;
}

// ---- Ignore ----------------------------------------------------------------

const IGNORABLE_KINDS = [
  "aliases", "actions", "gags", "highlights", "substitutes", "tickers", "macros", "events", "prompts",
] as const;
type IgnorableKind = (typeof IGNORABLE_KINDS)[number];

const ignoredKinds = new Set<IgnorableKind>();

function handleMapFor(kind: IgnorableKind): Map<string, { enabled: boolean }> | null {
  switch (kind) {
    case "aliases": return aliasHandles;
    case "actions": return actionHandles;
    case "gags": return gagHandles;
    case "highlights": return highlightHandles;
    case "substitutes": return substituteHandles;
    case "tickers": return tickerHandles;
    case "macros": return macroHandles;
    case "events": return null;
    case "prompts": return promptHandles;
  }
}

export function resolveIgnoreKind(value: string): IgnorableKind | null {
  const input = value.trim().toLowerCase();
  if (!input) return null;
  return IGNORABLE_KINDS.find((kind) => kind.startsWith(input)) ?? null;
}

/** Flip a kind's ignore state; returns the new "ignored" flag. */
export function toggleIgnore(kind: IgnorableKind): boolean {
  const ignored = !ignoredKinds.has(kind);
  if (ignored) ignoredKinds.add(kind); else ignoredKinds.delete(kind);
  const handles = handleMapFor(kind);
  if (handles) for (const handle of handles.values()) handle.enabled = !ignored;
  return ignored;
}

export function ignoreStates(): Array<[IgnorableKind, boolean]> {
  return IGNORABLE_KINDS.map((kind) => [kind, ignoredKinds.has(kind)]);
}

function applyIgnoreState(kind: IgnorableKind, handle: { enabled: boolean }): void {
  if (ignoredKinds.has(kind)) handle.enabled = false;
}

// ---- Wildcard removal ------------------------------------------------------

/** A full-match key predicate for a TinTin wildcard argument, else `null`. */
function keyMatcher(patternText: string): ((key: string) => boolean) | null {
  if (!/[%{]/.test(patternText)) return null;
  const compiled = compileTinTinPattern(patternText);
  if (compiled.unsupported) return null;
  const js = jsRegexSource(compiled);
  if (!js) return null;
  const matcher = new RegExp(`^(?:${js.source})$`, js.flags);
  return (key) => matcher.test(key);
}

function removeKeys<D>(
  table: Record<string, D>,
  pattern: string,
  drop: (key: string) => void,
): number {
  const matcher = keyMatcher(pattern);
  const keys = matcher
    ? Object.keys(table).filter(matcher)
    : (Object.hasOwn(table, pattern) ? [pattern] : []);
  for (const key of keys) {
    drop(key);
    delete table[key];
  }
  return keys.length;
}

// ---- #alias ----------------------------------------------------------------

export function defineAlias(pattern: string, commands: string, priority: number, quiet = false, cls?: string | null): boolean {
  const compiled = compileDefinitionPattern("alias", pattern);
  if (!compiled) return false;
  if (compiled.raw) {
    echoError(`#ERROR: raw (~) alias patterns have no smudgy input equivalent.`);
    return false;
  }

  // TinTin aliases match from the start of the typed line; a pattern with no
  // captures still receives its argument tail as %0 and word slots %1..%9.
  const argsMode = compiled.groups === 0;
  const source = `^${compiled.source}${argsMode ? "(?:\\s(.*))?$" : ""}`;
  const body = parseBody(commands);

  aliasHandles.get(pattern)?.delete();
  const handle = createAlias(source, (matches) => {
    let slots: Record<number, string>;
    if (argsMode) {
      const tail = matches[1] ?? "";
      slots = { 0: tail };
      tail.split(/\s+/).filter(Boolean).slice(0, 99).forEach((word, index) => {
        slots[index + 1] = word;
      });
    } else {
      slots = captureSlots(compiled, matches);
    }
    runBody(body, slots);
  }, automationOptions(`${activeCommandChar}alias {${pattern}}`, priority));
  aliasHandles.set(pattern, handle);
  applyIgnoreState("aliases", handle);

  mutateStored((stored) => {
    stored.aliases[pattern] = withClass({ pattern, commands, priority }, cls);
  });
  if (!quiet) echoOk(`#OK. {${pattern}} IS NOW AN ALIAS.`);
  return true;
}

export function removeAlias(pattern: string): number {
  return (() => {
    let removed = 0;
    mutateStored((stored) => {
      removed = removeKeys(stored.aliases, pattern, (key) => {
        aliasHandles.get(key)?.delete();
        aliasHandles.delete(key);
      });
    });
    return removed;
  })();
}

// ---- #action ---------------------------------------------------------------

export function defineAction(pattern: string, commands: string, priority: number, quiet = false, cls?: string | null): boolean {
  const compiled = compileDefinitionPattern("action", pattern);
  if (!compiled) return false;
  const body = parseBody(commands);
  const patterns = compiled.raw ? { rawPatterns: [compiled.source] } : compiled.source;

  actionHandles.get(pattern)?.delete();
  const handle = createTrigger(patterns, (matches) => {
    if (guardedByNegativeLiterals(compiled, line.text)) return;
    runBody(body, captureSlots(compiled, matches));
  }, automationOptions(`${activeCommandChar}action {${pattern}}`, priority));
  actionHandles.set(pattern, handle);
  applyIgnoreState("actions", handle);

  mutateStored((stored) => {
    stored.actions[pattern] = withClass({ pattern, commands, priority }, cls);
  });
  if (!quiet) echoOk(`#OK. {${pattern}} IS NOW AN ACTION.`);
  return true;
}

export function removeAction(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.actions, pattern, (key) => {
      actionHandles.get(key)?.delete();
      actionHandles.delete(key);
    });
  });
  return removed;
}

// ---- #gag ------------------------------------------------------------------

export function defineGag(pattern: string, quiet = false, cls?: string | null): boolean {
  const compiled = compileDefinitionPattern("gag", pattern);
  if (!compiled) return false;
  const patterns = compiled.raw ? { rawPatterns: [compiled.source] } : compiled.source;

  gagHandles.get(pattern)?.delete();
  const handle = createTrigger(patterns, () => {
    if (guardedByNegativeLiterals(compiled, line.text)) return;
    line.gag();
  }, { name: automationName(`${activeCommandChar}gag {${pattern}}`) });
  gagHandles.set(pattern, handle);
  applyIgnoreState("gags", handle);

  mutateStored((stored) => {
    stored.gags[pattern] = withClass({ pattern }, cls);
  });
  if (!quiet) echoOk(`#OK. {${pattern}} IS NOW A GAG.`);
  return true;
}

export function removeGag(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.gags, pattern, (key) => {
      gagHandles.get(key)?.delete();
      gagHandles.delete(key);
    });
  });
  return removed;
}

// ---- #highlight ------------------------------------------------------------

/** TinTin's #highlight repaints matched text wholesale, so every channel the
 *  color spec leaves out is pinned to an explicit default — smudgy's highlight
 *  leaves unset channels untouched, which is not TinTin's contract. */
const HIGHLIGHT_RESET_ATTRIBUTES = {
  bold: false, faint: false, italic: false, underline: "none", blink: "none",
  crossedOut: false, reverse: false,
} as const;

export function defineHighlight(pattern: string, color: string, quiet = false, cls?: string | null): boolean {
  const compiled = compileDefinitionPattern("highlight", pattern);
  if (!compiled) return false;
  const parsed = highlightStyleOptions(color, makeWarnSink());
  const options = {
    fg: parsed.fg ?? "default",
    bg: parsed.bg ?? "default",
    attributes: HIGHLIGHT_RESET_ATTRIBUTES,
  };
  const patterns = compiled.raw ? { rawPatterns: [compiled.source] } : compiled.source;

  highlightHandles.get(pattern)?.delete();
  const handle = createTrigger(patterns, (matches) => {
    if (guardedByNegativeLiterals(compiled, line.text)) return;
    for (const range of occurrenceByteRanges(line.text, matches[0])) {
      line.highlightAt(range.begin, range.end, options);
    }
  }, { name: automationName(`${activeCommandChar}highlight {${pattern}}`) });
  highlightHandles.set(pattern, handle);
  applyIgnoreState("highlights", handle);

  mutateStored((stored) => {
    stored.highlights[pattern] = withClass({ pattern, color }, cls);
  });
  if (!quiet) echoOk(`#OK. {${pattern}} IS NOW A HIGHLIGHT.`);
  return true;
}

export function removeHighlight(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.highlights, pattern, (key) => {
      highlightHandles.get(key)?.delete();
      highlightHandles.delete(key);
    });
  });
  return removed;
}

// ---- #substitute -----------------------------------------------------------

export function defineSubstitute(pattern: string, replacement: string, quiet = false, cls?: string | null): boolean {
  const compiled = compileDefinitionPattern("substitute", pattern);
  if (!compiled) return false;
  const patterns = compiled.raw ? { rawPatterns: [compiled.source] } : compiled.source;

  substituteHandles.get(pattern)?.delete();
  const handle = createTrigger(patterns, (matches) => {
    if (guardedByNegativeLiterals(compiled, line.text)) return;
    const env = makeExecutionEnv(captureSlots(compiled, matches));
    const rendered = renderOutput(replacement, env);
    // Right to left, so earlier ranges stay valid as each edit reflows the line.
    const ranges = occurrenceByteRanges(line.text, matches[0]);
    for (let index = ranges.length - 1; index >= 0; index--) {
      line.replaceAt(rendered, ranges[index].begin, ranges[index].end);
    }
  }, { name: automationName(`${activeCommandChar}substitute {${pattern}}`) });
  substituteHandles.set(pattern, handle);
  applyIgnoreState("substitutes", handle);

  mutateStored((stored) => {
    stored.substitutes[pattern] = withClass({ pattern, replacement }, cls);
  });
  if (!quiet) echoOk(`#OK. {${pattern}} IS NOW A SUBSTITUTE.`);
  return true;
}

export function removeSubstitute(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.substitutes, pattern, (key) => {
      substituteHandles.get(key)?.delete();
      substituteHandles.delete(key);
    });
  });
  return removed;
}

// ---- #prompt ---------------------------------------------------------------

export function definePrompt(pattern: string, replacement: string, row: number | null, quiet = false, cls?: string | null): boolean {
  const compiled = compileDefinitionPattern("prompt", pattern);
  if (!compiled) return false;
  const patterns = compiled.raw ? { rawPatterns: [compiled.source] } : compiled.source;

  promptHandles.get(pattern)?.delete();
  const handle = createTrigger(patterns, (matches) => {
    if (guardedByNegativeLiterals(compiled, line.text)) return;
    const env = makeExecutionEnv(captureSlots(compiled, matches));
    const rendered = replacement ? renderOutput(replacement, env) : null;
    // With a row and an open #split, the prompt moves to the split pane the
    // way TinTin parks it in the split bar; row numbers select the pane, not
    // a position within it.
    if (row !== null && hasSplit()) {
      splitEcho(rendered ?? line.text);
      line.gag();
      return;
    }
    if (rendered !== null) {
      const ranges = occurrenceByteRanges(line.text, matches[0]);
      for (let index = ranges.length - 1; index >= 0; index--) {
        line.replaceAt(rendered, ranges[index].begin, ranges[index].end);
      }
    }
  }, { name: automationName(`${activeCommandChar}prompt {${pattern}}`), prompt: true });
  promptHandles.set(pattern, handle);
  applyIgnoreState("prompts", handle);

  mutateStored((stored) => {
    stored.prompts[pattern] = withClass({ pattern, replacement, row }, cls);
  });
  if (!quiet) echoOk(`#OK. {${pattern}} IS NOW A PROMPT.`);
  return true;
}

export function removePrompt(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.prompts, pattern, (key) => {
      promptHandles.get(key)?.delete();
      promptHandles.delete(key);
    });
  });
  return removed;
}

// ---- #pathdir --------------------------------------------------------------

/** The active pathdir table: TinTin's defaults, overridden by stored entries. */
export function getPathDirs(): Record<string, PathDir> {
  return { ...defaultPathDirs(), ...getStored().pathdirs };
}

export function setPathDir(dir: string, reverse: string): boolean {
  // Assigning "__proto__" on a plain object sets its prototype, not an
  // entry; the name can never work as a pathdir, so refuse it.
  if (dir === "__proto__" || reverse === "__proto__") return false;
  mutateStored((stored) => {
    stored.pathdirs[dir] = { dir, reverse };
  });
  return true;
}

export function removePathDir(dir: string): boolean {
  let removed = false;
  mutateStored((stored) => {
    removed = Object.hasOwn(stored.pathdirs, dir);
    delete stored.pathdirs[dir];
  });
  return removed;
}

// ---- #ticker / #delay ------------------------------------------------------

export function defineTicker(name: string, commands: string, seconds: number, quiet = false, cls?: string | null): boolean {
  if (!Number.isFinite(seconds) || seconds <= 0) {
    echoError(`#ERROR: ticker interval {${seconds}} is not a positive number of seconds.`);
    return false;
  }
  const body = parseBody(commands);
  tickerHandles.get(name)?.delete();
  const handle = createTimer(
    { name: automationName(`${activeCommandChar}ticker {${name}}`), intervalMs: Math.round(seconds * 1000), repeat: true },
    () => runBody(body, {}),
  );
  tickerHandles.set(name, handle);
  applyIgnoreState("tickers", handle);
  mutateStored((stored) => {
    stored.tickers[name] = withClass({ name, commands, seconds }, cls);
  });
  if (!quiet) echoOk(`#OK. {${name}} IS NOW A TICKER.`);
  return true;
}

export function removeTicker(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.tickers, pattern, (key) => {
      tickerHandles.get(key)?.delete();
      tickerHandles.delete(key);
    });
  });
  return removed;
}

/** One-shot delays are session-scoped and never persisted. */
export function defineDelay(name: string | null, commands: string, seconds: number): boolean {
  if (!Number.isFinite(seconds) || seconds < 0) {
    echoError(`#ERROR: delay time {${seconds}} is not a number of seconds.`);
    return false;
  }
  const key = name ?? `unnamed-${++unnamedDelayCounter}`;
  const body = parseBody(commands);
  delayHandles.get(key)?.delete();
  const handle = createTimer(
    { name: automationName(`${activeCommandChar}delay {${key}}`), intervalMs: Math.round(seconds * 1000), repeat: false },
    () => {
      delayHandles.delete(key);
      runBody(body, {});
    },
  );
  delayHandles.set(key, handle);
  return true;
}

export function removeDelay(name: string): boolean {
  const handle = delayHandles.get(name);
  if (!handle) return false;
  handle.delete();
  delayHandles.delete(name);
  return true;
}

// ---- #function -------------------------------------------------------------

const MAX_FUNCTION_DEPTH = 25;
let functionCallDepth = 0;

export function defineFunction(name: string, commands: string, quiet = false, cls?: string | null): boolean {
  const key = name.trim().toLowerCase();
  if (!/^[a-z_][a-z0-9_]*$/.test(key)) {
    echoError(`#ERROR: {${name}} is not a usable function name.`);
    return false;
  }
  functionBodies.set(key, parseBody(commands));
  mutateStored((stored) => {
    stored.functions[key] = withClass({ name: key, commands }, cls);
  });
  if (!quiet) echoOk(`#OK. {${key}} IS NOW A FUNCTION.`);
  return true;
}

export function removeFunction(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.functions, pattern.trim().toLowerCase(), (key) => {
      functionBodies.delete(key);
    });
  });
  return removed;
}

/**
 * `@fn{args}` — run a defined function. Returns `#return`'s value, else the
 * function's `result` variable, else `""`; `undefined` when no such function
 * exists (the interpolator then keeps the call literal).
 */
export function callTinTinFunction(name: string, args: string[]): unknown | undefined {
  const body = functionBodies.get(name.trim().toLowerCase());
  if (!body) return undefined;
  if (functionCallDepth >= MAX_FUNCTION_DEPTH) {
    echoError(`#ERROR: function calls nested past ${MAX_FUNCTION_DEPTH} levels; @${name} returned empty.`);
    return "";
  }
  functionCallDepth += 1;
  try {
    const slots: Record<number, string> = { 0: args.join(";") };
    args.slice(0, 99).forEach((argument, index) => {
      slots[index + 1] = argument;
    });
    const env = makeExecutionEnv(slots);
    const signal = execute(body, env, 1);
    if (signal?.kind === "return") return signal.value;
    const result = env.getVar(["result"]);
    return result === undefined ? "" : result;
  } catch (error) {
    echoError(`#ERROR: @${name} failed: ${error instanceof Error ? error.message : String(error)}.`);
    return "";
  } finally {
    functionCallDepth -= 1;
  }
}

// ---- #macro ----------------------------------------------------------------

export function defineMacro(key: string, commands: string, quiet = false, cls?: string | null): boolean {
  const spec = tinTinKeySpec(key);
  if (!spec) {
    echoError(`#ERROR: {${key}} is not a recognized key sequence; smudgy can bind: ${supportedKeyNames().join(", ")}.`);
    return false;
  }
  const body = parseBody(commands);
  macroHandles.get(key)?.delete();
  const handle = createHotkey(spec, () => runBody(body, {}), { name: automationName(`${activeCommandChar}macro {${spec.key}}`) });
  macroHandles.set(key, handle);
  applyIgnoreState("macros", handle);
  mutateStored((stored) => {
    stored.macros[key] = withClass({ key, commands }, cls);
  });
  if (!quiet) echoOk(`#OK. {${key}} (${spec.key}) IS NOW A MACRO.`);
  return true;
}

export function removeMacro(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.macros, pattern, (key) => {
      macroHandles.get(key)?.delete();
      macroHandles.delete(key);
    });
  });
  return removed;
}

// ---- #event ----------------------------------------------------------------

type EventSource =
  | { subscribe: (fire: (slots: Record<number, string>) => void) => { off(): void } }
  | { omitted: string };

const EVENT_SOURCES: Record<string, EventSource> = {
  "SESSION CONNECTED": {
    subscribe: (fire) => connect.on(() => fire({ 0: getProfile().name })),
  },
  "SESSION DISCONNECTED": {
    subscribe: (fire) => disconnect.on(() => fire({ 0: getProfile().name })),
  },
  "SEND OUTPUT": {
    subscribe: (fire) => sentOutput.on((payload) => fire({ 0: payload.command, 1: String(payload.command.length) })),
  },
  "SENT OUTPUT": {
    subscribe: (fire) => sentOutput.on((payload) => fire({ 0: payload.command, 1: String(payload.command.length) })),
  },
  "RECEIVED LINE": {
    subscribe: (fire) => receivedLine.on((payload) => fire({ 0: payload.text, 1: payload.text })),
  },
  "RECEIVED OUTPUT": {
    subscribe: (fire) => receivedLine.on((payload) => fire({ 0: payload.text, 1: payload.text })),
  },
  "RECEIVED INPUT": {
    subscribe: (fire) => receivedInput.on((payload) => fire({ 0: payload.text, 1: String(payload.text.length) })),
  },
  "SCREEN RESIZE": { omitted: "smudgy panes resize automatically" },
  "SESSION ACTIVATED": { omitted: "smudgy repaints automatically when a session activates" },
};

export function defineEvent(rawName: string, commands: string, quiet = false, cls?: string | null): boolean {
  const name = rawName.trim().toUpperCase();
  const source = EVENT_SOURCES[name];
  if (!source) {
    echoError(`#ERROR: event {${rawName}} has no smudgy equivalent.`);
    return false;
  }
  if ("omitted" in source) {
    echoNote(`event {${name}} is unnecessary here: ${source.omitted}.`);
    return false;
  }
  const body = parseBody(commands);
  eventSubscriptions.get(name)?.off();
  const subscription = source.subscribe((slots) => {
    if (!ignoredKinds.has("events")) runBody(body, slots);
  });
  eventSubscriptions.set(name, subscription);
  mutateStored((stored) => {
    stored.events[name] = withClass({ name, commands }, cls);
  });
  if (!quiet) echoOk(`#OK. {${name}} IS NOW AN EVENT.`);
  return true;
}

export function removeEvent(pattern: string): number {
  let removed = 0;
  mutateStored((stored) => {
    removed = removeKeys(stored.events, pattern.trim().toUpperCase(), (key) => {
      eventSubscriptions.get(key)?.off();
      eventSubscriptions.delete(key);
    });
  });
  return removed;
}

// ---- Replay ----------------------------------------------------------------

/** Recreate every persisted definition; called once at package load. */
export function replayStored(): number {
  const stored = getStored();
  let count = 0;
  const cls = (def: { class?: string }) => def.class ?? null;
  for (const def of Object.values(stored.functions)) count += defineFunction(def.name, def.commands, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.aliases)) count += defineAlias(def.pattern, def.commands, def.priority, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.actions)) count += defineAction(def.pattern, def.commands, def.priority, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.gags)) count += defineGag(def.pattern, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.highlights)) count += defineHighlight(def.pattern, def.color, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.substitutes)) count += defineSubstitute(def.pattern, def.replacement, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.tickers)) count += defineTicker(def.name, def.commands, def.seconds, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.macros)) count += defineMacro(def.key, def.commands, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.events)) count += defineEvent(def.name, def.commands, true, cls(def)) ? 1 : 0;
  for (const def of Object.values(stored.prompts)) count += definePrompt(def.pattern, def.replacement, def.row ?? null, true, cls(def)) ? 1 : 0;
  return count;
}
