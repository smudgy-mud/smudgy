// =============================================================================
//  Smudgy inline automation globals — TypeScript declarations
// =============================================================================
//
// Inline alias, trigger, and hotkey bodies execute inside Smudgy's user API
// scope. This ambient bridge gives the authoring language service that same
// public surface without changing or wrapping the user's source text.

import type { Matches, SmudgyApi } from "smudgy:core";

declare global {
  /**
   * Shared persistent variables inferred from assignments in this user's inline
   * aliases, triggers, and hotkeys. Known properties are merged into this
   * interface by the embedded authoring service; dynamic keys retain the
   * runtime's permissive behavior.
   */
  interface SmudgyUserVars {
    [key: string]: any;
  }

  const echo: SmudgyApi["echo"];
  const style: SmudgyApi["style"];
  const link: SmudgyApi["link"];
  const pattern: SmudgyApi["pattern"];
  const command: SmudgyApi["command"];
  const send: SmudgyApi["send"];
  const sendRaw: SmudgyApi["sendRaw"];
  const reload: SmudgyApi["reload"];
  const capture: SmudgyApi["capture"];
  const fallthrough: SmudgyApi["fallthrough"];
  const skipInner: SmudgyApi["skipInner"];
  const stopWatching: SmudgyApi["stopWatching"];
  const byName: SmudgyApi["byName"];
  const byId: SmudgyApi["byId"];
  const getSessions: SmudgyApi["getSessions"];
  const getProfile: SmudgyApi["getProfile"];
  const getSettings: SmudgyApi["getSettings"];
  const getDataDir: SmudgyApi["getDataDir"];
  const userAutomations: SmudgyApi["userAutomations"];
  const createState: SmudgyApi["createState"];
  const createEvent: SmudgyApi["createEvent"];
  const createProcedure: SmudgyApi["createProcedure"];
  const createDerived: SmudgyApi["createDerived"];
  const events: SmudgyApi["events"];
  const gmcp: SmudgyApi["gmcp"];
  const layout: SmudgyApi["layout"];
  const createAlias: SmudgyApi["createAlias"];
  const createTrigger: SmudgyApi["createTrigger"];
  const createTriggers: SmudgyApi["createTriggers"];
  const createTimer: SmudgyApi["createTimer"];
  const createHotkey: SmudgyApi["createHotkey"];
  const aliases: SmudgyApi["aliases"];
  const triggers: SmudgyApi["triggers"];
  const timers: SmudgyApi["timers"];
  const hotkeys: SmudgyApi["hotkeys"];
  const vars: SmudgyUserVars;
  const line: SmudgyApi["line"];
  const buffer: SmudgyApi["buffer"];
  const submission: SmudgyApi["submission"];
  const mapper: SmudgyApi["mapper"];
  const Area: SmudgyApi["Area"];
  const session: SmudgyApi["session"];
  const input: SmudgyApi["input"];
  const id: SmudgyApi["id"];

  const matches: Matches;
  /** In a trigger inside another: the matched values of the triggers it is inside. */
  const outer: Matches | undefined;
}

export {};
