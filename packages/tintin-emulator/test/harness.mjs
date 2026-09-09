// End-to-end smoke test: evaluate the tintin-emulator package under Node with
// stubbed smudgy: modules. Run with: node test/harness.mjs
// then push typed lines through the submit handler.
import { register } from "node:module";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

register("./smudgy-loader.mjs", import.meta.url);

const manifest = JSON.parse(readFileSync(new URL("../smudgy.package.json", import.meta.url), "utf8"));

// A style builder standing in for smudgy's: property access and option calls
// return the builder; tag application flattens to plain text.
function makeStyleBuilder() {
  const builder = new Proxy(function () {}, {
    get: () => builder,
    apply(_target, _self, args) {
      const [first, ...values] = args;
      if (Array.isArray(first) && "raw" in first) {
        let text = "";
        first.forEach((part, index) => {
          text += part;
          if (index < values.length) text += textOf(values[index], []);
        });
        return { __styled: text };
      }
      return builder;
    },
  });
  return builder;
}

function textOf(value, rest) {
  if (Array.isArray(value) && "raw" in value) {
    let text = "";
    value.forEach((part, index) => {
      text += part;
      if (index < rest.length) text += textOf(rest[index], []);
    });
    return text;
  }
  if (value && typeof value === "object" && "__styled" in value) return value.__styled;
  return String(value);
}

const created = [];
const calls = [];
const fakeFiles = new Map();
globalThis.Deno = {
  readTextFileSync(path) {
    if (!fakeFiles.has(path)) throw new Error(`no such file: ${path}`);
    return fakeFiles.get(path);
  },
  writeTextFileSync(path, text) { fakeFiles.set(path, text); },
  mkdirSync() {},
};
const stub = {
  vars: { __tintin_definitions: { config: { speedwalk: false } } },
  params: { commandChar: "#", speedwalk: true },
  calls,
  record: (kind, value) => calls.push([kind, value]),
  requireGrant(group, capability) {
    assert.ok(
      manifest.permissions?.smudgy?.[group]?.includes(capability),
      `package manifest must grant permissions.smudgy.${group} ${capability}`,
    );
  },
  textOf,
  style: makeStyleBuilder(),
  input: { completion: { add: (...words) => { stub.completionWords.push(...words); } } },
  completionWords: [],
  submission: null,
  eventHandlers: {},
  dataDir: "C:/fake-data",
  sessions: [],
  panes: [],
  session: {
    profile: { name: "Tester" },
    send: (command) => calls.push(["send", command]),
    echo: (value) => calls.push(["echo", textOf(value, [])]),
    mainPane: {
      split(direction, options) {
        const pane = {
          direction,
          options,
          name: options.name,
          echoes: [],
          closed: false,
          echo(value) { this.echoes.push(textOf(value, [])); },
          close() { this.closed = true; },
        };
        stub.panes.push(pane);
        return pane;
      },
    },
  },
  create(kind, patterns, script, options) {
    const handle = {
      kind, patterns, script, options,
      name: options?.name ?? String(patterns),
      enabled: true,
      deleted: false,
      delete() { this.deleted = true; },
    };
    created.push(handle);
    return handle;
  },
  line: {
    text: "",
    gagged: false,
    edits: [],
    gag() { this.gagged = true; },
    highlight(text, options) { this.edits.push(["highlight", text, options]); },
    replace(oldStr, newStr) { this.edits.push(["replace", oldStr, textOf(newStr, [])]); return true; },
    highlightAt(begin, end, options) { this.edits.push(["highlightAt", begin, end, options]); },
    replaceAt(newStr, begin, end) { this.edits.push(["replaceAt", textOf(newStr, []), begin, end]); },
  },
};
globalThis.__smudgyStub = stub;

await import(new URL("../index.ts", import.meta.url));
assert.ok((stub.eventHandlers.submit ?? []).length >= 1, "submit handler registered");
assert.ok(stub.completionWords.includes("#alias"), "completion words registered");

function type(text) {
  let cancelled = false;
  stub.submission = {
    text,
    cancel() { cancelled = true; },
    replace() {},
  };
  for (const handler of stub.eventHandlers.submit ?? []) handler({ text });
  return cancelled;
}

/** Submit a line through sys:submit and then Smudgy's priority-ordered aliases. */
function submitInput(text) {
  if (type(text)) return;
  const aliases = created
    .filter((handle) => handle.kind === "alias" && handle.enabled && !handle.deleted)
    .sort((left, right) => (right.options?.priority ?? 0) - (left.options?.priority ?? 0));
  for (const alias of aliases) {
    const match = new RegExp(alias.patterns).exec(text);
    if (!match) continue;
    alias.script(Object.assign(match, match.groups ?? {}));
    if (alias.options?.fallthrough === false) return;
  }
  calls.push(["wire", text]);
}

function lastCall(kind) {
  return calls.filter(([k]) => k === kind).at(-1)?.[1];
}

// SPEEDWALK is a load-time smudgy param. The manifest defaults it off; this
// harness opts in to exercise the lowest-priority native alias it installs.
assert.equal(manifest.params.find((param) => param.key === "speedwalk")?.default, false);
assert.ok(
  manifest.permissions.smudgy.session.includes("send-direct"),
  "speedwalk's direct sends are explicitly granted",
);
const speedwalkAlias = created.filter((handle) =>
  handle.kind === "alias" && String(handle.name).includes("SPEEDWALK")).at(-1);
assert.ok(speedwalkAlias, "speedwalk fallback alias created when the param is true");
assert.equal(speedwalkAlias.options.priority, -1_000_000, "ordinary aliases take precedence");
calls.length = 0;
submitInput("ne2s");
assert.deepEqual(
  calls.filter(([kind]) => kind === "sendRaw").map(([, value]) => value),
  ["n\n", "e\n", "s\n", "s\n"],
  "expanded steps bypass alias matching instead of recursively matching SPEEDWALK",
);
calls.length = 0;
submitInput("look");
assert.deepEqual(calls, [["wire", "look"]], "non-speedwalk input reaches the MUD untouched");
calls.length = 0;
submitInput("unde");
assert.deepEqual(
  calls.filter(([kind]) => kind === "sendRaw").map(([, value]) => value),
  ["u\n", "n\n", "d\n", "e\n"],
  "all valid one-letter direction sequences are speedwalks",
);
assert.equal(type("NEWS"), false, "uppercase avoids speedwalking");

// Plain lines pass through untouched.
assert.equal(type("kill orc"), false);

// #showme echoes, with interpolation and color codes flattened.
assert.equal(type("#showme hello there"), true);
assert.equal(lastCall("echo"), "hello there");
type("#variable {target} {orc}");
assert.equal(stub.vars.target, "orc");
type("#showme kill $target");
assert.equal(lastCall("echo"), "kill orc");
type("#showme <118>alert<088> done");
assert.equal(lastCall("echo"), "alert done");

// #alias creates a native alias; firing it interprets the body.
type("#alias {gt} {guildtell %0}");
const alias = created.find((handle) => handle.kind === "alias" && handle.options.name.includes("{gt}"));
assert.ok(alias, "alias created");
assert.equal(alias.patterns, "^gt(?:\\s(.*))?$");
assert.equal(alias.options.fallthrough, false);
alias.script({ 0: "gt hello world", 1: "hello world" });
assert.equal(lastCall("send"), "guildtell hello world");

// Pattern alias: slots resolve through the capture map.
type("#alias {^cast %1 at %2} {say casting %1 on %2}");
const castAlias = created.filter((handle) => handle.kind === "alias").at(-1);
castAlias.script({ 0: "cast fire at orc", 1: "fire", 2: "orc" });
assert.equal(lastCall("send"), "say casting fire on orc");

// Semicolons split inside bodies; nested #commands recurse.
type("#alias {panic} {flee;#showme fled!}");
const panic = created.filter((handle) => handle.kind === "alias").at(-1);
panic.script({ 0: "panic", 1: "" });
assert.equal(lastCall("send"), "flee");
assert.equal(lastCall("echo"), "fled!");

// #action creates a trigger whose body sees captures.
type("#action {%1 tells you %2} {tell %1 heard you}");
const action = created.find((handle) => handle.kind === "trigger");
assert.ok(action, "action created");
action.script({ 0: "Bob tells you hi", 1: "Bob", 2: "hi" });
assert.equal(lastCall("send"), "tell Bob heard you");

// #gag / #highlight / #substitute wire line edits.
type("#gag {^You are hungry}");
const gag = created.filter((handle) => handle.kind === "trigger").at(-1);
stub.line.text = "You are hungry.";
gag.script({ 0: "You are hungry" });
assert.equal(stub.line.gagged, true);

type("#substitute {%1 miss} {%1 MISS!}");
const substitute = created.filter((handle) => handle.kind === "trigger").at(-1);
stub.line.text = "The orc miss";
substitute.script({ 0: "The orc miss", 1: "The orc" });
assert.deepEqual(stub.line.edits.at(-1), ["replaceAt", "The orc MISS!", 0, 12]);

// #if / #elseif / #else and #math.
type("#math {hp} {40 + 2 * 5}");
assert.equal(stub.vars.hp, 50);
type("#if {$hp > 60} {#showme high} {#showme low}");
assert.equal(lastCall("echo"), "low");
type("#if {$hp > 60} {#showme high};#elseif {$hp > 40} {#showme mid};#else {#showme tiny}");
assert.equal(lastCall("echo"), "mid");

// A PCRE-only pattern is rejected with an error, not created.
const before = created.length;
type("#action {{(?=look)}ahead} {say no}");
assert.equal(created.length, before);
assert.ok(String(lastCall("echo")).includes("lookaround"), "lookaround rejection echoed");

// Unknown and declined commands produce TinTin-flavored errors.
type("#frobnicate now");
assert.ok(String(lastCall("echo")).includes("IS NOT A COMMAND"));
type("#system rm -rf /");
assert.ok(String(lastCall("echo")).includes("sandboxed"));
type("#config {speedwalk} {off}");
assert.ok(String(lastCall("echo")).includes("not supported"), "speedwalk is configured through package params");
assert.equal(speedwalkAlias.deleted, false);

// Definitions persisted for replay.
const storedDefs = stub.vars.__tintin_definitions;
assert.equal(storedDefs.config, undefined, "stale command-side speedwalk state is discarded");
assert.ok(storedDefs.aliases.gt, "alias persisted");
assert.ok(storedDefs.actions["%1 tells you %2"], "action persisted");

// #unalias removes handle and store.
type("#unalias {gt}");
assert.equal(alias.deleted, true);
assert.equal(stub.vars.__tintin_definitions.aliases.gt, undefined);

// Abbreviations dispatch too.
type("#sh abbreviated");
assert.equal(lastCall("echo"), "abbreviated");

// Ticker creates a timer with seconds -> ms.
type("#ticker {sip} {drink potion} {30}");
const ticker = created.find((handle) => handle.kind === "timer");
assert.ok(ticker, "ticker created");
assert.equal(ticker.patterns.intervalMs, 30000);
ticker.script();
assert.equal(lastCall("send"), "drink potion");

// ---- Tier 2 ----------------------------------------------------------------

// #loop with direction and a local loop variable.
calls.length = 0;
type("#loop {1} {3} {i} {say loop $i}");
assert.deepEqual(calls.filter(([k]) => k === "send").map(([, v]) => v),
  ["say loop 1", "say loop 2", "say loop 3"]);

// #foreach over braced items.
calls.length = 0;
type("#foreach {{a}{b}} {x} {say item $x}");
assert.deepEqual(calls.filter(([k]) => k === "send").map(([, v]) => v),
  ["say item a", "say item b"]);

// #while with #math and #break.
type("#math {n} {0};#while {1} {#math {n} {$n + 1};#if {$n >= 3} {#break}}");
assert.equal(stub.vars.n, 3);

// #switch / #case / #default, no fall-through.
type("#switch {1 + 1} {#case {1} {#showme one};#case {2} {#showme two};#default {#showme other}}");
assert.equal(lastCall("echo"), "two");
type("#switch {9} {#case {1} {#showme one};#default {#showme other}}");
assert.equal(lastCall("echo"), "other");

// #function via #return and via the result variable; @fn{} interpolates.
type("#function {double} {#return %1%1}");
type("#showme @double{ha}");
assert.equal(lastCall("echo"), "haha");
type("#function {answer} {#variable {result} {42}}");
type("#showme the answer is @answer{}");
assert.equal(lastCall("echo"), "the answer is 42");

// #format and formatted #echo.
type("#format {greet} {[%5s] %d} {hi} {7}");
assert.equal(stub.vars.greet, "[   hi] 7");
type("#echo {%s and %s} {a} {b}");
assert.equal(lastCall("echo"), "a and b");

// #replace rewrites every occurrence in a variable.
type("#variable {t} {red apple red}");
type("#replace {t} {red} {blue}");
assert.equal(stub.vars.t, "blue apple blue");

// #regexp runs the matching body; its captures arrive as &N (TinTin form).
type("#regexp {abc123} {c%d} {#showme digits &1} {#showme none}");
assert.equal(lastCall("echo"), "digits 123");
type("#regexp {abc} {x%d} {#showme yes} {#showme no match}");
assert.equal(lastCall("echo"), "no match");

// #cat appends.
type("#variable {word} {fog};#cat {word} {horn}");
assert.equal(stub.vars.word, "foghorn");

// #list operations.
type("#list {l} {create} {c;a;b}");
assert.deepEqual(stub.vars.l, ["c", "a", "b"]);
type("#list {l} {sort}");
assert.deepEqual(stub.vars.l, ["a", "b", "c"]);
type("#list {l} {get} {2} {second}");
assert.equal(stub.vars.second, "b");
type("#list {l} {size} {len}");
assert.equal(stub.vars.len, 3);
type("#list {l} {add} {d}");
assert.deepEqual(stub.vars.l, ["a", "b", "c", "d"]);
type("#list {l} {delete} {1}");
assert.deepEqual(stub.vars.l, ["b", "c", "d"]);
type("#list {l} {collapse} {,}");
assert.equal(stub.vars.l, "b,c,d");

// #macro binds a hotkey by escape sequence.
type("#macro {\\e[15~} {say f5}");
const macro = created.find((handle) => handle.kind === "hotkey");
assert.ok(macro, "macro created");
assert.equal(macro.patterns.key, "F5");
macro.script();
assert.equal(lastCall("send"), "say f5");

// #event subscribes to the sys event bus.
type("#event {RECEIVED LINE} {#showme got %0}");
assert.ok((stub.eventHandlers.receive ?? []).length === 1, "event subscribed");
stub.eventHandlers.receive[0]({ text: "Zap!" });
assert.equal(lastCall("echo"), "got Zap!");

// #class open/close tag definitions; kill removes them.
type("#class {combat} {open}");
type("#action {%1 hits you} {say ouch}");
type("#class {combat} {close}");
const combatAction = created.filter((handle) => handle.kind === "trigger").at(-1);
assert.equal(stub.vars.__tintin_definitions.actions["%1 hits you"].class, "combat");
type("#class {combat} {kill}");
assert.equal(combatAction.deleted, true);
assert.equal(stub.vars.__tintin_definitions.actions["%1 hits you"], undefined);

// Wildcard #unalias removes every matching definition.
type("#alias {aa1} {say one}");
type("#alias {aa2} {say two}");
type("#unalias {aa%d}");
assert.equal(stub.vars.__tintin_definitions.aliases.aa1, undefined);
assert.equal(stub.vars.__tintin_definitions.aliases.aa2, undefined);

// #ignore disables a kind's handles and re-enables on toggle.
type("#action {ping} {say pong}");
const ping = created.filter((handle) => handle.kind === "trigger").at(-1);
type("#ignore {actions}");
assert.equal(ping.enabled, false);
type("#ignore {actions}");
assert.equal(ping.enabled, true);

// #info summarizes.
type("#info");
assert.ok(String(lastCall("echo")).includes("aliases:"));

// ---- Adversarial-review regressions ----------------------------------------

// Formatted #echo must not re-interpolate sigils inside user data.
stub.vars.gold = "123";
stub.vars.price = "100$gold";
stub.vars.co = "AT&T";
type("#echo {%s} {$price}");
assert.equal(lastCall("echo"), "100$gold");
type("#echo {%s} {$co}");
assert.equal(lastCall("echo"), "AT&T");

// Nested writes address the same 1-based element reads and deletes use.
type("#list {lx} {create} {a;b;c}");
type("#variable {lx[1]} {X}");
assert.deepEqual(stub.vars.lx, ["X", "b", "c"]);
type("#showme $lx[1]");
assert.equal(lastCall("echo"), "X");

// #replace/#regexp captures live in &N; the automation's %N stays intact.
stub.vars.t2 = "foo bar foo";
type("#alias {rep %1} {#replace {t2} {foo} {%1}}");
const rep = created.filter((handle) => handle.kind === "alias").at(-1);
rep.script({ 0: "rep BAR", 1: "BAR" });
assert.equal(stub.vars.t2, "BAR bar BAR");
type("#alias {rx %1} {#regexp {abc} {b} {#showme said %1 grabbed &0}}");
const rx = created.filter((handle) => handle.kind === "alias").at(-1);
rx.script({ 0: "rx WORLD", 1: "WORLD" });
assert.equal(lastCall("echo"), "said WORLD grabbed b");
type("#regexp {hello} {h%w} {#showme cap &1}");
assert.equal(lastCall("echo"), "cap ello");

// Fired bodies stay quiet: no #OK confirmations per fire.
type("#alias {setx} {#variable {x9} {1}}");
const setx = created.filter((handle) => handle.kind === "alias").at(-1);
calls.length = 0;
setx.script({ 0: "setx", 1: "" });
assert.equal(stub.vars.x9, 1);
assert.ok(!calls.some(([kind, value]) => kind === "echo" && String(value).includes("#OK")),
  "no confirmation spam from fired bodies");

// Unbraced trailing arguments take the remainder (TinTin GET_ALL).
type("#alias gt2 guildtell %0");
const gt2 = created.filter((handle) => handle.kind === "alias").at(-1);
gt2.script({ 0: "gt2 hello world", 1: "hello world" });
assert.equal(lastCall("send"), "guildtell hello world");
type("#delay 2 say hello world");
const delayed = created.filter((handle) => handle.kind === "timer").at(-1);
assert.equal(delayed.patterns.intervalMs, 2000);
delayed.script();
assert.equal(lastCall("send"), "say hello world");

// A bare command character is an error, not #action.
type("#");
assert.ok(String(lastCall("echo")).includes("IS NOT A COMMAND"));

// Variables named after Object.prototype members behave like variables.
type("#variable {toString} {hi}");
assert.equal(stub.vars.toString, "hi");
type("#showme $toString");
assert.equal(lastCall("echo"), "hi");

// Non-finite #math results are refused (the real vars store is JSON-backed).
type("#math {inf} {1/0}");
assert.equal(stub.vars.inf, undefined);
assert.ok(String(lastCall("echo")).includes("not a finite number"));

// ---- Tier 3 ----------------------------------------------------------------

// #split opens a top pane; #showme with a row routes into it; #unsplit closes.
type("#split {3}");
const splitPane = stub.panes.at(-1);
assert.equal(splitPane.direction, "top");
assert.equal(splitPane.options.height, 60);
type("#showme {status line} {1}");
assert.equal(splitPane.echoes.at(-1), "status line");
assert.notEqual(lastCall("echo"), "status line");
type("#showme plain again");
assert.equal(lastCall("echo"), "plain again");

// #prompt without a row rewrites the prompt in place; the trigger is a
// prompt trigger.
type("#prompt {%1hp>} {[%1 hp]}");
const promptTrigger = created.filter((handle) => handle.kind === "trigger").at(-1);
assert.equal(promptTrigger.options.prompt, true);
stub.line.text = "100hp>";
promptTrigger.script({ 0: "100hp>", 1: "100" });
assert.deepEqual(stub.line.edits.at(-1), ["replaceAt", "[100 hp]", 0, 6]);

// #prompt with a row parks the prompt in the split pane and gags the line.
type("#prompt {%1sp>} {[%1 sp]} {1}");
const rowPrompt = created.filter((handle) => handle.kind === "trigger").at(-1);
stub.line.text = "50sp>";
stub.line.gagged = false;
rowPrompt.script({ 0: "50sp>", 1: "50" });
assert.equal(splitPane.echoes.at(-1), "[50 sp]");
assert.equal(stub.line.gagged, true);
type("#unsplit");
assert.equal(splitPane.closed, true);

// #pathdir defaults + custom entries; recording via the sys send event.
const pumpSends = (...commands) => {
  for (const command of commands) {
    for (const handler of stub.eventHandlers.send ?? []) handler({ command });
  }
};
type("#path new");
calls.length = 0;
speedwalkAlias.script({ 0: "2ne" });
type("#path describe");
assert.ok(String(lastCall("echo")).includes("2n;e"), "speedwalk directions are recorded in #PATH");
assert.equal(
  calls.filter(([kind]) => kind === "sendRaw").length,
  3,
  "recorded speedwalk directions still reach the MUD",
);
type("#pathdir {enter} {out}");
type("#path new");
pumpSends("n", "n", "e", "look", "enter");
type("#path describe");
assert.ok(String(lastCall("echo")).includes("2n;e;enter"));
calls.length = 0;
type("#path undo");
assert.equal(lastCall("send"), "out");
type("#path goto {start}");
type("#path run");
assert.deepEqual(calls.filter(([k]) => k === "send").map(([, v]) => v), ["out", "n", "n", "e"]);
// The client delivers sys:send AFTER send() returns; the emulator's own
// replay sends must not be re-recorded even though recording is still on.
pumpSends("out", "n", "n", "e");
type("#path describe");
assert.ok(String(lastCall("echo")).includes("2n;e"), "route unchanged after replay events");
assert.ok(String(lastCall("echo")).includes("3 steps"), "route did not double");
type("#path save {forward} {route}");
assert.equal(stub.vars.route, "n;n;e");
type("#path save {backward} {retreat}");
assert.equal(stub.vars.retreat, "w;s;s");
type("#path unzip {3s;2w}");
type("#path describe");
assert.ok(String(lastCall("echo")).includes("3s;2w"));

// Unknown names load as literal steps (TinTin inserts them verbatim).
type("#path unzip {open;2n}");
assert.ok(String(lastCall("echo")).includes("3 steps (1 literal command)"));
type("#path describe");
assert.ok(String(lastCall("echo")).includes("open"));

// Cursor-aware path operations: map/show, walk, move, get, swap, and
// save/load of TinTin's forward/backward/delay triples.
type("#path map");
assert.ok(String(lastCall("echo")).includes("[open]"));
calls.length = 0;
type("#path walk {forward}");
assert.equal(lastCall("send"), "open");
type("#path get {position} {pathpos}");
assert.equal(stub.vars.pathpos, 2);
type("#path move {1}");
type("#path get {info} {pathinfo}");
assert.deepEqual(stub.vars.pathinfo, { length: 3, position: 3, mapping: 0, running: 0 });
type("#path save {both} {routeboth}");
assert.equal(stub.vars.routeboth, ";{open}{open}{0};{n}{s}{0};{n}{s}{0}");
type("#path load {routeboth}");
type("#path get {length} {pathlen}");
assert.equal(stub.vars.pathlen, 3);
type("#path goto {end}");
type("#path swap");
type("#path save {forward} {swapped}");
assert.equal(stub.vars.swapped, "s;s;open");

// #path create clears AND starts tracking.
type("#path destroy");
type("#path create");
pumpSends("n");
type("#path describe");
assert.ok(String(lastCall("echo")).includes("1 steps"));

// Timer-paced runs are cancellable: destroy deletes the scheduled timers.
type("#path unzip {3n}");
type("#path run {5}");
const pacedTimers = created.filter((handle) => handle.kind === "timer" && String(handle.patterns?.name ?? "").startsWith("#path run"));
assert.equal(pacedTimers.length, 3);
pacedTimers[0].script();
assert.equal(lastCall("send"), "n");
type("#path get {position} {pacedpos}");
assert.equal(stub.vars.pacedpos, 2);
type("#path stop");
const beforeResume = created.length;
type("#path run {5}");
const resumedTimers = created.slice(beforeResume)
  .filter((handle) => handle.kind === "timer" && String(handle.patterns?.name ?? "").startsWith("#path run"));
assert.equal(resumedTimers.length, 2, "paced run resumes from its cursor");
type("#path destroy");
assert.ok(pacedTimers.every((timer) => timer.deleted), "paced run cancelled by destroy");
assert.ok(resumedTimers.every((timer) => timer.deleted), "resumed run cancelled by destroy");

// Arbitrary forward/backward command pairs are valid path nodes.
type("#path destroy");
type("#path insert {toString}");
type("#path describe");
assert.ok(String(lastCall("echo")).includes("toString"));
// __proto__ is still not a storable pathdir name.
type("#pathdir {__proto__} {x}");
assert.ok(String(lastCall("echo")).includes("cannot be a pathdir name"));
type("#path stop");

// #read loads a .tin file from $DATA with its own command character.
fakeFiles.set("C:/fake-data/setup.tin", "#alias {hh} {say hello from file}\n#variable {fileread} {yes}\n");
type("#read setup.tin");
assert.equal(stub.vars.fileread, "yes");
assert.ok(stub.vars.__tintin_definitions.aliases.hh, "alias defined from file");
assert.ok(String(lastCall("echo")).includes("READ {setup.tin}"));

// #read cycles are refused, not infinite.
fakeFiles.set("C:/fake-data/loop.tin", "#read loop.tin\n");
calls.length = 0;
type("#read loop.tin");
assert.ok(calls.some(([kind, value]) => kind === "echo" && String(value).includes("cycle")));

// Escapes out of $DATA are refused.
type("#read ../outside.tin");
assert.ok(String(lastCall("echo")).includes("data directory"));

// #write exports the definition tables as a TinTin file.
type("#write out.tin");
const written = fakeFiles.get("C:/fake-data/out.tin");
assert.ok(written.startsWith("#nop"), "file starts with the command char");
assert.ok(written.includes("#alias {hh} {say hello from file}"));
assert.ok(written.includes("#pathdir {enter} {out}"));

// #all fans out to every session; #<name> targets one.
stub.sessions = [
  { profile: { name: "Scout" }, sent: [], send(command) { this.sent.push(command); } },
  { profile: { name: "Healer" }, sent: [], send(command) { this.sent.push(command); } },
];
type("#all {say hi all}");
assert.deepEqual(stub.sessions.map((target) => target.sent), [["say hi all"], ["say hi all"]]);
type("#Scout {kill orc}");
assert.deepEqual(stub.sessions[0].sent.at(-1), "kill orc");
type("#session");
assert.ok(String(lastCall("echo")).includes("Healer"));

// #bell is visual.
type("#bell");
assert.equal(lastCall("echo"), "*bell*");

// ---- Round-2 review regressions --------------------------------------------

// Unbraced #showme text ending in a number stays in the main view; only the
// braced-text-plus-row form routes to the split. Resizing recreates the pane.
type("#split {2}");
const firstSplit = stub.panes.at(-1);
assert.equal(firstSplit.options.height, 40);
type("#split {10}");
const resized = stub.panes.at(-1);
assert.equal(firstSplit.closed, true, "resize recreates the pane");
assert.equal(resized.options.height, 200);
type("#showme hp 100");
assert.equal(lastCall("echo"), "hp 100");
assert.ok(!resized.echoes.includes("hp 100"), "unbraced text never routes to the split");
type("#showme {in the pane} {1}");
assert.equal(resized.echoes.at(-1), "in the pane");
type("#unsplit");

// A file opening with a comment still reads with # as its command char.
fakeFiles.set("C:/fake-data/hdr.tin", "/* my settings */\n#variable {ch} {1}\n");
calls.length = 0;
type("#read hdr.tin");
assert.equal(stub.vars.ch, 1);
assert.ok(!calls.some(([kind, value]) => kind === "send" && String(value).startsWith("#")), "no commands leaked to the MUD");

// #prompt with one argument shows, never defines.
const triggersBefore = created.filter((handle) => handle.kind === "trigger").length;
type("#prompt {nosuch>}");
assert.ok(String(lastCall("echo")).includes("IS NOT A PROMPT"));
assert.equal(created.filter((handle) => handle.kind === "trigger").length, triggersBefore);

// MUD-controlled text with unbalanced braces cannot inject commands via
// #write -> #read; the entry is skipped with a note.
stub.vars.loot = "} ;#showme PWNED-FROM-MUD";
type("#write inj.tin");
assert.ok(String(lastCall("echo")).includes("1 entry skipped"));
const injected = fakeFiles.get("C:/fake-data/inj.tin");
assert.ok(!injected.includes("PWNED"), "hostile value not written");
calls.length = 0;
type("#read inj.tin");
assert.ok(!calls.some(([kind, value]) => kind === "echo" && String(value).includes("PWNED")), "no injected command ran");
delete stub.vars.loot;

// ---- A real script: the TT_Win1 two-window port link ------------------------

// Patterns full of <>, unsupported #port lines, a #sess command, and a #main
// session target. Every line must process; nothing may abort the file.
fakeFiles.set("C:/fake-data/TT_Win1", [
  "#port init ic 3001;",
  "#alias {a} {#port send all docommand %0};",
  "#alias {b} {#main %0;#port send all docommand %0};",
  "#act {^<PORT> docommand %0} {#main %0};",
  "#port call localhost 3002;",
  "#sess main mud.arctic.org 2700 Arctic;",
].join("\n"));
stub.sessions = [
  { profile: { name: "main" }, sent: [], send(command) { this.sent.push(command); } },
];
calls.length = 0;
type("#read TT_Win1");
assert.ok(stub.vars.__tintin_definitions.aliases.a, "alias a defined");
assert.ok(stub.vars.__tintin_definitions.aliases.b, "alias b defined");
const portAction = stub.vars.__tintin_definitions.actions["^<PORT> docommand %0"];
assert.ok(portAction, "action with < in its pattern defined");
const portTrigger = created.filter((handle) => handle.kind === "trigger").at(-1);
assert.ok(!String(portTrigger.options.name).includes("<"), "automation name sanitized");
assert.ok(calls.some(([kind, value]) => kind === "echo" && String(value).includes("READ {TT_Win1}")), "file read to completion");
assert.ok(calls.some(([kind, value]) => kind === "echo" && String(value).includes("#port")), "#port warned");
// Firing alias b targets the session named main and warns about #port.
const bAlias = created.filter((handle) => handle.kind === "alias" && handle.options.name.includes("{b}")).at(-1);
bAlias.script({ 0: "b kill orc", 1: "kill orc" });
assert.equal(stub.sessions[0].sent.at(-1), "kill orc");
type("#unalias {a};#unalias {b};#unaction {^<PORT> docommand %0}");
stub.sessions = [];

// A command that throws mid-file warns and keeps going.
const realCreate = stub.create;
let threw = false;
stub.create = (kind, ...rest) => {
  if (kind === "trigger" && !threw) {
    threw = true;
    throw new TypeError("synthetic create failure");
  }
  return realCreate.call(stub, kind, ...rest);
};
fakeFiles.set("C:/fake-data/half.tin", "#action {boom} {say no};#variable {survived} {yes}\n");
calls.length = 0;
type("#read half.tin");
stub.create = realCreate;
assert.equal(stub.vars.survived, "yes", "execution continued past the throw");
assert.ok(calls.some(([kind, value]) => kind === "echo" && String(value).includes("synthetic create failure")), "throw surfaced as a warning");
type("#unvariable {survived}");

// ---- The good ol' round trip ------------------------------------------------

// Wipe everything, define one of each kind, #write, wipe again, #read, and
// the persisted store must come back deep-equal.
for (const kind of ["unalias", "unaction", "ungag", "unhighlight", "unsubstitute", "unticker", "unfunction", "unmacro", "unevent", "unprompt"]) {
  type(`#${kind} {%*}`);
}
type("#unpathdir {enter}");
for (const key of Object.keys(stub.vars)) {
  if (key !== "__tintin_definitions") delete stub.vars[key];
}

type("#variable {target} {orc}");
type("#variable {stats} {{hp}{100}{sp}{50}}");
type("#list {inv} {create} {sword;shield}");
type("#pathdir {enter} {out}");
type("#function {greet} {#return hi %1}");
type("#alias {gt} {guildtell %0} {3}");
type("#class {combat} {open}");
type("#action {%1 hits you} {say ouch %1}");
type("#class {combat} {close}");
type("#gag {^spam}");
type("#highlight {treasure} {bold yellow}");
type("#substitute {%1 miss} {%1 MISS}");
type("#prompt {%1hp>} {[%1]} {1}");
type("#ticker {sip} {drink} {30}");
type("#macro {\\e[15~} {say f5}");
type("#event {RECEIVED LINE} {#variable {last} {%0}}");

type("#write full.tin");
assert.ok(!String(lastCall("echo")).includes("skipped"), "clean export has no skips");
const storeBefore = structuredClone(stub.vars.__tintin_definitions);
const varsBefore = {
  target: structuredClone(stub.vars.target),
  stats: structuredClone(stub.vars.stats),
  inv: structuredClone(stub.vars.inv),
};

for (const kind of ["unalias", "unaction", "ungag", "unhighlight", "unsubstitute", "unticker", "unfunction", "unmacro", "unevent", "unprompt"]) {
  type(`#${kind} {%*}`);
}
type("#unpathdir {enter}");
for (const key of Object.keys(stub.vars)) {
  if (key !== "__tintin_definitions") delete stub.vars[key];
}
assert.deepEqual(stub.vars.__tintin_definitions.aliases, {}, "store wiped before re-read");

type("#read full.tin");
assert.deepEqual(stub.vars.__tintin_definitions, storeBefore, "definitions store round-trips exactly");
assert.equal(stub.vars.target, varsBefore.target);
assert.deepEqual(stub.vars.stats, varsBefore.stats);
assert.deepEqual(stub.vars.inv, varsBefore.inv);
assert.ok(Array.isArray(stub.vars.inv), "arrays come back as arrays");

console.log("ALL HARNESS CHECKS PASSED");
