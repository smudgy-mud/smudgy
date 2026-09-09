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
  /**
   * Send text or bytes directly to the game without running aliases.
   * Smudgy does not split this input at command separators.
   *
   * Use a string to send text with the connection's character encoding.
   * Use binary input when your script must control each byte.
   *
   * @remarks
   * **Sending strings**
   *
   * Smudgy converts each line feed ("\n") to a carriage return followed
   * by a line feed ("\r\n"). This pair is called CRLF.
   * Smudgy preserves existing CRLF pairs and standalone "\r" characters.
   *
   * Smudgy converts the text to bytes with the target connection's character
   * encoding. If that encoding cannot represent the text, Smudgy rejects
   * the complete send. Smudgy displays the text in your output window.
   *
   * **Before version 0.6.0**
   *
   * After converting the line endings, Smudgy checks for a final CRLF.
   * If the string lacks a final CRLF, Smudgy adds one for compatibility.
   * This automatic addition is deprecated. The function itself is not deprecated.
   *
   * Smudgy warns once per calling script when it needs this automatic addition.
   * The warning identifies the script. Reloading the session's scripts resets
   * the warnings. Strings ending with "\n" or "\r\n" do not cause this warning.
   *
   * **Starting in version 0.6.0**
   *
   * Smudgy still converts "\n" to "\r\n", but it does not add a missing
   * final CRLF. For example, sendRaw("look") sends text without CRLF.
   * The game might wait for more input before processing that text.
   * An empty string sends no bytes.
   *
   * To preserve complete commands across both versions, add "\n" explicitly.
   *
   * **Sending binary input**
   *
   * Binary input accepts an ArrayBuffer, a typed array, or a DataView.
   * A Uint8Array is usually the simplest choice.
   * For a view, Smudgy sends only the bytes within that view.
   * Other typed arrays send their stored bytes, not converted numeric values.
   * Use DataView when you must specify the byte order of multi-byte numbers.
   *
   * Smudgy copies the bytes during the call. Later changes cannot affect
   * the queued send. Shared memory, detached buffers, ordinary arrays, and
   * iterables are not supported. Invalid input throws a TypeError.
   * An empty buffer sends no bytes.
   *
   * Smudgy sends binary input without changes in both versions.
   * It does not add CRLF, convert line endings, or apply a character encoding.
   * Binary input does not cause the deprecation warning.
   *
   * Smudgy does not display the bytes in your output window.
   * The game can still send a response or send the bytes back.
   *
   * **Sending Telnet IAC bytes**
   *
   * Telnet uses byte 0xFF, called IAC, to introduce a protocol command.
   * To send one literal 0xFF data byte through Telnet, send two 0xFF bytes.
   * This rule also applies when Telnet BINARY mode is active.
   *
   * For strings, Smudgy doubles each 0xFF byte after converting the text.
   * For binary input, Smudgy does not double any bytes.
   * Your script must supply the required Telnet commands and doubled bytes.
   *
   * @returns Nothing. The call queues the send without waiting for delivery.
   * Existing send permissions and limits apply to both strings and binary input.
   *
   * @example
   * // Send a complete command before and after version 0.6.0.
   * // Smudgy sends "look\r\n". No deprecation warning occurs.
   * sendRaw("look\n");
   *
   * @example
   * // Preserve an existing CRLF without adding another one.
   * sendRaw("look\r\n");
   *
   * @example
   * // Before 0.6.0: send "look\r\n" and warn once per script.
   * // Starting in 0.6.0: send "look" without CRLF.
   * sendRaw("look");
   *
   * @example
   * // Send text without CRLF in either version through binary input.
   * // TextEncoder returns a Uint8Array containing UTF-8 bytes.
   * // It uses UTF-8 regardless of the connection's character encoding.
   * const bytes = new TextEncoder().encode("look");
   * // Send exactly 0x6C, 0x6F, 0x6F, 0x6B without local display.
   * sendRaw(bytes);
   *
   * @example
   * // Send one literal 0xFF data byte through Telnet.
   * sendRaw(Uint8Array.of(0xff, 0xff));
   *
   * @example
   * // Send the Telnet NOP command: IAC followed by NOP.
   * sendRaw(Uint8Array.of(0xff, 0xf1));
   */
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
