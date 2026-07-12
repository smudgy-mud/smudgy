// =============================================================================
//  smudgy:core runtime implementation (the `smudgy_ops` extension entry point)
// =============================================================================
//  This file is TypeScript. It is the deno_core extension ESM entry (esm_entry_point
//  ext:smudgy_ops/smudgy.ts). deno_runtime's extension transpiler type-strips it at
//  JsRuntime init, exactly like mapper.ts / widgets.ts -- so it loads as runnable JS
//  with no build step. Two HARD constraints:
//    1. Source must be 7-bit ASCII (deno_core's extension load asserts this). Use
//       \uXXXX escapes for any non-ASCII *runtime* string; keep comments ASCII.
//    2. The only importable module is "ext:core/ops" (deno's global op module). Any
//       `import type` is erased at transpile, so it is safe, but we keep this file
//       self-contained (no cross-file imports) to stay robust.
//
//  The AUTHOR-FACING contract (what `import ... from "smudgy:core"` resolves to in an
//  editor) is the hand-authored ambient `script_typings/smudgy-core.d.ts`. This impl is
//  kept in lockstep with it by a drift-guard test (see script_typings.rs). When you change
//  the public surface here, change that .d.ts too.
//
//  The smudgy:core surface reaches scripts two ways, both via the per-creator api object
//  built by `__smudgy_make_api` (installed below as `globalThis.__smudgy_create_api`):
//    - modules/packages: the synthesized per-importer `smudgy:core` virtual module
//      (script/src/package_resolver.rs::load_core_module) re-exports the api members;
//    - inline alias/trigger bodies: run inside `with (globalThis.__smudgy_user_api) {...}`.
// =============================================================================

// @ts-ignore - ext:core/ops is a deno virtual module with no type decls
import * as __smudgy_ops from "ext:core/ops";

// Ops are an untyped FFI boundary; treat the table as `any`. Type-checking value lives in
// the public surface below (Session/Line/the api object), not in the op calls.
const {
    op_smudgy_get_current_session,
    op_smudgy_get_session_character,
    op_smudgy_get_sessions,
    op_smudgy_create_simple_alias,
    op_smudgy_create_javascript_function_alias,
    op_smudgy_create_simple_trigger,
    op_smudgy_create_javascript_function_trigger,
    op_smudgy_set_alias_enabled,
    op_smudgy_set_trigger_enabled,
    op_smudgy_remove_alias,
    op_smudgy_remove_trigger,
    op_smudgy_create_hotkey,
    op_smudgy_remove_hotkey,
    op_smudgy_get_alias,
    op_smudgy_get_trigger,
    op_smudgy_list_aliases,
    op_smudgy_list_triggers,
    op_smudgy_alias_exists,
    op_smudgy_trigger_exists,
    op_smudgy_session_echo,
    op_smudgy_session_echo_styled,
    op_smudgy_session_reload,
    op_smudgy_session_send,
    op_smudgy_session_send_raw,
    op_smudgy_insert,
    op_smudgy_replace,
    op_smudgy_highlight,
    op_smudgy_remove,
    op_smudgy_gag,
    op_smudgy_redirect,
    op_smudgy_copy,
    op_smudgy_pane_split,
    op_smudgy_pane_close,
    op_smudgy_pane_echo,
    op_smudgy_pane_echo_styled,
    op_smudgy_pane_clear,
    op_smudgy_pane_list,
    op_smudgy_pane_resolve,
    op_smudgy_get_current_line,
    op_smudgy_get_current_line_number,
    op_smudgy_get_current_line_styles,
    op_smudgy_buffer_get_text,
    op_smudgy_buffer_get_styles,
    op_smudgy_line_insert,
    op_smudgy_line_replace,
    op_smudgy_splice,
    op_smudgy_line_splice,
    op_smudgy_line_highlight,
    op_smudgy_line_remove,
    op_smudgy_capture,
    op_smudgy_param_get,
    op_smudgy_get_settings,
    op_smudgy_gmcp_enabled,
    op_smudgy_gmcp_send,
    op_smudgy_gmcp_enable_module,
    op_smudgy_gmcp_disable_module,
    op_smudgy_gmcp_merge_keys,
    op_smudgy_data_dir,
    op_smudgy_save_user_alias,
    op_smudgy_save_user_trigger,
    op_smudgy_save_user_hotkey,
    op_smudgy_delete_user_alias,
    op_smudgy_delete_user_trigger,
    op_smudgy_delete_user_hotkey,
    op_smudgy_get_user_alias,
    op_smudgy_get_user_trigger,
    op_smudgy_get_user_hotkey,
    op_smudgy_list_user_aliases,
    op_smudgy_list_user_triggers,
    op_smudgy_list_user_hotkeys,
    op_smudgy_validate_name,
    op_smudgy_on,
    op_smudgy_off,
    op_smudgy_emit,
    op_smudgy_interop_resolve_creator,
    op_smudgy_interop_resolve_producer_root,
    op_smudgy_interop_resolve_consumer_root,
    op_smudgy_interop_resolve_previous_root,
    op_smudgy_interop_resolve_event,
    op_smudgy_store_set,
    op_smudgy_store_get,
    op_smudgy_store_get_tagged,
    op_smudgy_store_keys,
    op_smudgy_store_previous_get,
    op_smudgy_store_previous_get_tagged,
    op_smudgy_store_previous_keys,
    op_smudgy_store_watch,
    op_smudgy_store_unwatch,
    op_smudgy_store_bind,
    op_smudgy_procedure_on,
    op_smudgy_procedure_post,
    op_smudgy_interop_declare,
} = __smudgy_ops as any;

// ---- Shared types (mirrored by script_typings/smudgy-core.d.ts) -------------

/** A session's profile (name + subtext). */
interface Profile {
    name?: string;
    subtext?: string;
}

/** The resolved terminal color scheme as `#rrggbb` hex strings (theme + tweaks applied). */
interface Palette {
    /** The 16 ANSI colors, indexed [normal 8, bright 8] (black, red, green, yellow, blue,
     *  magenta, cyan, white). */
    ansi: string[];
    foreground: string;
    background: string;
    echo: string;
    warn: string;
    output: string;
    selection: string;
    inputBackground: string;
    /** The app accent color; absent when the scheme falls back to the foreground. */
    accent?: string;
}

/** The read-only app settings a script can inspect via `getSettings()`. Only display/behavior
 *  settings are exposed (never the API endpoint or any secret). `palette` is resolved by the UI,
 *  so it is absent for a brief moment at session start before the first settings push. */
interface Settings {
    /** Separates multiple commands on one input line; empty disables splitting. */
    commandSeparator: string;
    /** Lines starting with this prefix are sent verbatim; empty disables it. */
    rawLinePrefix: string;
    /** The scrollback buffer's maximum line count. */
    scrollbackLength: number;
    terminalFontFamily: string;
    /** Terminal font size in pixels (line height is size * 1.25). */
    terminalFontSize: number;
    /** Maximum terminal line length in columns; absent means wrap to pane width. */
    terminalLineLength?: number;
    /** The active color-scheme name. */
    theme: string;
    /** What the command input does with the text after a send. */
    commandInputBehavior: "selectAllClearOnBlur" | "selectAll" | "clear";
    /** The resolved terminal palette; absent until the UI has pushed it. */
    palette?: Palette;
}

/** A foreground/background color in the shape the write color API accepts. */
type Color =
    | string
    | { r: number; g: number; b: number }
    | { color: string; bold: boolean };

interface ColorOptions {
    fg?: Color;
    bg?: Color;
}

// ---- Styled text (the `style` tagged-template surface) ------------------------

/** The runtime brand every StyledText fragment carries (checked by `__is_styled_text`;
 *  `Symbol.for` so the check is robust even across realm copies of this module). */
const __STYLED_BRAND = Symbol.for("smudgy.styledText");

/** Modifier keys held when a link was clicked. */
interface LinkClick {
    shift: boolean;
    ctrl: boolean;
    alt: boolean;
}

/** A run's click action: a command to send, or a handler function (extracted into a
 *  side array before the payload crosses the op boundary -- functions don't serialize). */
type LinkSpec = { send: string } | { fn: (click: LinkClick) => void };

/** One flattened run of a fragment: its text plus the colors it has resolved so far.
 *  `null` means unset -- filled by an enclosing fragment's style, or by the delivery
 *  default (the echo role / the splice-point style). */
interface StyledRun {
    text: string;
    fg: Color | null;
    bg: Color | null;
    link: LinkSpec | null;
}

/** A piece of styled text built by the `style` tag. Opaque to authors; internally an
 *  ordered run list, already flattened (nesting resolves at construction time). */
class StyledTextImpl {
    /** Type-level brand mirrored by the contract's `StyledText`. The runtime property
     *  lives on the prototype (defined below); `declare` keeps this field type-only. */
    declare readonly __smudgyStyled: true;
    _runs: StyledRun[];

    constructor(runs: StyledRun[]) {
        this._runs = runs;
    }

    /** Interpolating a fragment into a PLAIN template degrades to its text. */
    toString(): string {
        let text = "";
        for (const run of this._runs) text += run.text;
        return text;
    }
}
Object.defineProperty(StyledTextImpl.prototype, __STYLED_BRAND, { value: true });
Object.defineProperty(StyledTextImpl.prototype, "__smudgyStyled", { value: true });

function __is_styled_text(value: unknown): value is StyledTextImpl {
    return typeof value === "object" && value !== null && (value as any)[__STYLED_BRAND] === true;
}

/** The contract-facing face of a fragment: what the published `StyledText` type
 *  declares. The echo/edit entry points accept this (so contract-typed values flow),
 *  then narrow to the real `StyledTextImpl` with the runtime brand check. */
interface StyledTextLike {
    readonly __smudgyStyled: true;
}

/** A tagged-template invocation's first argument (the cooked strings array with `raw`). */
function __is_template_strings(value: unknown): value is TemplateStringsArray {
    return Array.isArray(value) && Array.isArray((value as any).raw);
}

const __STYLED_ANSI_NAMES = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
const __STYLED_ROLE_NAMES = ["default", "echo", "output", "warn"];

/** Clamp to an integer 0-255 (the wire is strict about u8s). */
function __styled_u8(value: unknown): number {
    const n = Math.round(Number(value));
    return Number.isFinite(n) ? Math.min(255, Math.max(0, n)) : 0;
}

/** Validate + normalize a `Color` so the wire only ever sees the exact accepted shapes.
 *  Throws a TypeError naming the problem (style bugs should be loud, not silent). */
function __styled_check_color(value: Color): Color {
    if (typeof value === "string") {
        if (__STYLED_ANSI_NAMES.indexOf(value) === -1 && __STYLED_ROLE_NAMES.indexOf(value) === -1) {
            throw new TypeError(
                `Unknown color name "${value}" (expected an ANSI name or "default"/"echo"/"output"/"warn")`,
            );
        }
        return value;
    }
    if (typeof value === "object" && value !== null) {
        const v = value as any;
        if (typeof v.r === "number" && typeof v.g === "number" && typeof v.b === "number") {
            return { r: __styled_u8(v.r), g: __styled_u8(v.g), b: __styled_u8(v.b) };
        }
        if (typeof v.color === "string") {
            if (__STYLED_ANSI_NAMES.indexOf(v.color) === -1) {
                throw new TypeError(`Unknown ANSI color "${v.color}" in { color, bold }`);
            }
            return { color: v.color, bold: Boolean(v.bold) };
        }
    }
    throw new TypeError(
        "Expected a color: an ANSI/theme name, { r, g, b }, or { color, bold }",
    );
}

/** Build a fragment from one tagged-template invocation under the enclosing style
 *  `(fg, bg)` and link. Literal parts and plain interpolations take the enclosing
 *  values; interpolated fragments keep their own runs, inheriting only what they left
 *  unset (lexical inheritance -- for links that means the innermost tag wins).
 *  Adjacent runs merge when every attribute matches. */
function __styled_from_template(
    fg: Color | null,
    bg: Color | null,
    link: LinkSpec | null,
    strings: TemplateStringsArray,
    values: unknown[],
): StyledTextImpl {
    const runs: StyledRun[] = [];
    const push = (
        text: string,
        runFg: Color | null,
        runBg: Color | null,
        runLink: LinkSpec | null,
    ): void => {
        if (text === "") return;
        const last = runs.length > 0 ? runs[runs.length - 1] : undefined;
        if (last !== undefined && last.fg === runFg && last.bg === runBg && last.link === runLink) {
            last.text += text;
            return;
        }
        runs.push({ text, fg: runFg, bg: runBg, link: runLink });
    };
    for (let i = 0; i < strings.length; i++) {
        // An illegal escape in a tagged template yields an undefined cooked entry
        // (legal ES for tags); fall back to the raw text, like String.raw.
        push(strings[i] !== undefined ? strings[i] : (strings.raw[i] ?? ""), fg, bg, link);
        if (i < values.length) {
            const value = values[i];
            if (__is_styled_text(value)) {
                for (const run of value._runs) {
                    push(
                        run.text,
                        run.fg === null ? fg : run.fg,
                        run.bg === null ? bg : run.bg,
                        run.link === null ? link : run.link,
                    );
                }
            } else {
                // Plain-template semantics: every other value stringifies.
                push(String(value), fg, bg, link);
            }
        }
    }
    return new StyledTextImpl(runs);
}

/** The impl-side twin of the contract's `StyleBuilder` (see smudgy-core.d.ts). */
interface StyleBuilder {
    (text: TemplateStringsArray, ...values: unknown[]): StyledTextImpl;
    (options: ColorOptions): StyleBuilder;
    fg(color: Color): StyleBuilder;
    bg(color: Color): StyleBuilder;
    readonly black: StyleBuilder;
    readonly red: StyleBuilder;
    readonly green: StyleBuilder;
    readonly yellow: StyleBuilder;
    readonly blue: StyleBuilder;
    readonly magenta: StyleBuilder;
    readonly cyan: StyleBuilder;
    readonly white: StyleBuilder;
    readonly default: StyleBuilder;
    readonly echo: StyleBuilder;
    readonly output: StyleBuilder;
    readonly warn: StyleBuilder;
    readonly bgBlack: StyleBuilder;
    readonly bgRed: StyleBuilder;
    readonly bgGreen: StyleBuilder;
    readonly bgYellow: StyleBuilder;
    readonly bgBlue: StyleBuilder;
    readonly bgMagenta: StyleBuilder;
    readonly bgCyan: StyleBuilder;
    readonly bgWhite: StyleBuilder;
}

/** The shorthand property tables, built once: property name -> the color it sets. */
const __STYLED_FG_PROPS: [string, Color][] =
    __STYLED_ANSI_NAMES.concat(__STYLED_ROLE_NAMES).map((name) => [name, name]);
const __STYLED_BG_PROPS: [string, Color][] = __STYLED_ANSI_NAMES.map((name) => [
    "bg" + name.charAt(0).toUpperCase() + name.slice(1),
    name,
]);

/** Every step of the chain is a fresh immutable builder over `(fg, bg)`; each is both
 *  a template tag and (called with options) a refinement. Shorthand properties are
 *  memoizing getters: the derived builder is built on first touch and cached as a data
 *  property, so a chain echoed per incoming line pays its allocations once. */
function __styled_make_builder(fg: Color | null, bg: Color | null): StyleBuilder {
    const builder = ((first: TemplateStringsArray | ColorOptions, ...values: unknown[]): any => {
        if (__is_template_strings(first)) {
            return __styled_from_template(fg, bg, null, first, values);
        }
        if (typeof first !== "object" || first === null) {
            throw new TypeError("style(...) expects { fg?, bg? }, or use it as a template tag");
        }
        // `!= null` so an explicit null means unset, like the plain color options.
        return __styled_make_builder(
            first.fg != null ? __styled_check_color(first.fg) : fg,
            first.bg != null ? __styled_check_color(first.bg) : bg,
        );
    }) as any;
    builder.fg = (color: Color) => __styled_make_builder(__styled_check_color(color), bg);
    builder.bg = (color: Color) => __styled_make_builder(fg, __styled_check_color(color));
    const memoize = (prop: string, derive: () => StyleBuilder): void => {
        Object.defineProperty(builder, prop, {
            configurable: true,
            get: () => {
                const derived = derive();
                Object.defineProperty(builder, prop, { value: derived });
                return derived;
            },
        });
    };
    for (const [prop, color] of __STYLED_FG_PROPS) {
        memoize(prop, () => __styled_make_builder(color, bg));
    }
    for (const [prop, color] of __STYLED_BG_PROPS) {
        memoize(prop, () => __styled_make_builder(fg, color));
    }
    return builder as StyleBuilder;
}

/** The root style builder (`style.red`, `style.bgBlue`, `style({ fg, bg })`, ...). */
const style: StyleBuilder = __styled_make_builder(null, null);

/** The impl-side twin of the contract's `StyleTag` (see smudgy-core.d.ts). */
interface StyleTag {
    (text: TemplateStringsArray, ...values: unknown[]): StyledTextImpl;
}

/** Makes text clickable: `link("north")` sends the command when clicked (as if
 *  typed); `link(fn)` runs the handler. Returns a template tag, so it composes with
 *  `style` by nesting -- the innermost link wins on overlap. */
function link(action: string | ((click: LinkClick) => void)): StyleTag {
    let spec: LinkSpec;
    if (typeof action === "string") {
        if (action === "") {
            throw new TypeError("link() command must not be empty");
        }
        spec = { send: action };
    } else if (typeof action === "function") {
        spec = { fn: action };
    } else {
        throw new TypeError("link() expects a command string or a click handler function");
    }
    return ((strings: TemplateStringsArray, ...values: unknown[]) =>
        __styled_from_template(null, null, spec, strings, values)) as StyleTag;
}

/** A run's link in wire form: a command, or an index into the callbacks array the op
 *  receives beside the payload. */
type WireLink = { send: string } | { cb: number } | null;

/** The wire shape of one run / one styled line (see ops.rs `StyledRunWire`). */
interface WireRun {
    text: string;
    fg: Color | null;
    bg: Color | null;
    link: WireLink;
}
interface StyledLineWire {
    runs: WireRun[];
}

/** A flattened fragment ready for a styled op call: the serializable payload plus the
 *  extracted callback functions, indexed by the runs' `{ cb }` links. */
interface StyledEchoArgs {
    payload: { lines: StyledLineWire[] };
    callbacks: ((click: LinkClick) => void)[];
}

/** Build the link converter one flatten pass uses: `{ fn }` specs are deduplicated
 *  into `callbacks` and become `{ cb }` indexes; command links pass through. */
function __styled_make_wire_link(
    callbacks: ((click: LinkClick) => void)[],
): (spec: LinkSpec | null) => WireLink {
    return (spec) => {
        if (spec === null || "send" in spec) return spec;
        let index = callbacks.indexOf(spec.fn);
        if (index === -1) {
            index = callbacks.length;
            callbacks.push(spec.fn);
        }
        return { cb: index };
    };
}

/** Flatten a fragment into the wire payload: whole lines, split on `\n` (a run may
 *  span several lines; each piece keeps the run's colors and link). Callback links
 *  are extracted into the side array -- functions don't serialize. */
function __styled_echo_payload(text: StyledTextImpl): StyledEchoArgs {
    const callbacks: ((click: LinkClick) => void)[] = [];
    const wireLink = __styled_make_wire_link(callbacks);
    const lines: StyledLineWire[] = [];
    let current: StyledLineWire = { runs: [] };
    for (const run of text._runs) {
        const link = wireLink(run.link);
        // Common case: no newline in the run. A linkless run object is safe to share
        // with the payload as-is -- it is serialized synchronously by the op call.
        if (run.text.indexOf("\n") === -1) {
            if (run.text !== "") {
                current.runs.push(
                    run.link === null
                        ? (run as unknown as WireRun)
                        : { text: run.text, fg: run.fg, bg: run.bg, link },
                );
            }
            continue;
        }
        const parts = run.text.split("\n");
        for (let i = 0; i < parts.length; i++) {
            if (i > 0) {
                lines.push(current);
                current = { runs: [] };
            }
            if (parts[i] !== "") {
                current.runs.push({ text: parts[i], fg: run.fg, bg: run.bg, link });
            }
        }
    }
    lines.push(current);
    return { payload: { lines }, callbacks };
}

/** Flatten a fragment for a line splice: ONE line's wire runs (a `Line` is one line,
 *  so a newline is an error) plus the extracted callbacks. Unset run colors are
 *  filled from `options` -- the inheritance base `insert` takes; whatever is still
 *  unset inherits the style at the splice point when the edit applies. */
function __styled_splice_args(
    text: StyledTextImpl,
    options: ColorOptions,
): { runs: WireRun[]; callbacks: ((click: LinkClick) => void)[] } {
    const callbacks: ((click: LinkClick) => void)[] = [];
    const wireLink = __styled_make_wire_link(callbacks);
    // Match the plain path's tolerance: a null color option means unset.
    const baseFg = options.fg != null ? __styled_check_color(options.fg) : null;
    const baseBg = options.bg != null ? __styled_check_color(options.bg) : null;
    const runs: WireRun[] = [];
    for (const run of text._runs) {
        if (run.text.indexOf("\n") !== -1) {
            throw new TypeError("styled text spliced into a line may not contain a newline");
        }
        if (run.text === "") continue;
        runs.push({
            text: run.text,
            fg: run.fg === null ? baseFg : run.fg,
            bg: run.bg === null ? baseBg : run.bg,
            link: wireLink(run.link),
        });
    }
    return { runs, callbacks };
}

/** The two call shapes every echo mirror accepts: a value (string or fragment), or a
 *  direct tagged-template use. Returns what should be delivered. */
function __styled_echo_arg(
    first: string | StyledTextLike | TemplateStringsArray,
    values: unknown[],
): string | StyledTextImpl {
    if (__is_template_strings(first)) {
        return __styled_from_template(null, null, null, first, values);
    }
    return first as string | StyledTextImpl;
}

/**
 * The capture object passed to a trigger/alias handler. Keyed by group NUMBER
 * (`matches[0]` whole match, `matches[1..]` groups) and, for a named group, also by NAME.
 * A plain object: named groups are own data properties keyed by group name.
 */
type Matches = Record<number | string, string>;

/** A string trigger/alias body uses bash-style `$N` / `${name}` / `$$` substitution. */
type InlineTemplate = string;

/** A function trigger/alias body receives the {@link Matches} object. */
type AutomationHandler = (matches: Matches) => string | void;

/** Either body form a trigger/alias accepts. */
type AutomationScript = InlineTemplate | AutomationHandler;

type Pattern = string | RegExp;

interface TriggerPatterns {
    patterns?: Pattern[];
    rawPatterns?: Pattern[];
    antiPatterns?: Pattern[];
}

/** The language of a persisted automation's script body. A `script` is sent as a literal
 *  command template unless this is `"js"`/`"ts"`. Defaults to `"plaintext"`. */
type ScriptLang = "plaintext" | "js" | "ts";

/** A persisted user-side alias (the shape saved in `aliases.json`). */
interface SavedAlias {
    /** Regex matched against the input line. */
    pattern: string;
    /** Inline script body (a command template, or code when `language` is js/ts). */
    script?: string;
    /** Defaults to true. */
    enabled?: boolean;
    /** Defaults to "plaintext". */
    language?: ScriptLang;
    /** Optional package-folder grouping in the automations window. */
    package?: string;
}

/** A persisted user-side trigger (the shape saved in `triggers.json`). */
interface SavedTrigger {
    /** Regexes matched against each incoming line's displayed text. */
    patterns?: string[];
    /** Regexes matched against the raw incoming line, before ANSI codes are stripped. */
    rawPatterns?: string[];
    /** Patterns that must NOT match for the trigger to fire. */
    antiPatterns?: string[];
    script?: string;
    /** Defaults to true. */
    enabled?: boolean;
    /** Also fire on prompts. Defaults to false. */
    prompt?: boolean;
    /** Defaults to "plaintext". */
    language?: ScriptLang;
    package?: string;
}

/** A persisted user-side hotkey (the shape saved in `hotkeys.json`). */
interface SavedHotkey {
    /** The primary key (e.g. "A", "F1", "Space"). */
    key: string;
    /** Modifier keys (e.g. ["Control", "Shift"]). */
    modifiers?: string[];
    script?: string;
    /** Defaults to true. */
    enabled?: boolean;
    /** Defaults to "plaintext". */
    language?: ScriptLang;
    package?: string;
}

/**
 * A handle to one persisted automation (returned by a registry's `save`/`get`). Reads are a
 * snapshot taken when the handle was obtained: `def()` returns it and `refresh()` re-reads disk.
 * Writes are EXPLICIT: `update()`/`delete()` each persist to disk and reload the server's other
 * sessions (there are no property setters that hide that cost).
 */
interface SavedAutomationHandle<Def> {
    /** The automation's name (its key in the saved set). */
    readonly name: string;
    /** The saved definition as last read into this handle. */
    def(): Def;
    /** Re-read the saved definition from disk into this handle. Returns false if it no longer exists. */
    refresh(): boolean;
    /**
     * Persist a partial change: merges `patch` onto the CURRENT saved definition (re-read from
     * disk, so concurrent edits aren't clobbered) and writes it back. Each call is a file write
     * plus a reload of the server's other sessions.
     */
    update(patch: Partial<Def>): boolean;
    /** Remove the saved automation. */
    delete(): boolean;
}

type SavedAliasHandle = SavedAutomationHandle<SavedAlias>;
type SavedTriggerHandle = SavedAutomationHandle<SavedTrigger>;
type SavedHotkeyHandle = SavedAutomationHandle<SavedHotkey>;

/**
 * CRUD over one kind of persisted user automation (the saved aliases/triggers/hotkeys shown in
 * the automations window), shaped like the live `aliases`/`triggers` registries but disk-backed.
 * `save` upserts and returns a handle; `get` returns a handle for an existing name; `list`/
 * `exists` introspect. Every write persists and reloads the server's other sessions.
 */
interface SavedAutomationRegistry<Def, Handle> {
    save(name: string, def: Def): Handle;
    get(name: string): Handle | undefined;
    list(): string[];
    exists(name: string): boolean;
    delete(name: string): boolean;
}

/** The persisted, UI-visible user automations, grouped by kind. See {@link SavedAutomationRegistry}. */
interface UserAutomations {
    aliases: SavedAutomationRegistry<SavedAlias, SavedAliasHandle>;
    triggers: SavedAutomationRegistry<SavedTrigger, SavedTriggerHandle>;
    hotkeys: SavedAutomationRegistry<SavedHotkey, SavedHotkeyHandle>;
}

interface AliasOptions {
    /** Explicit identity/display name; defaults to the pattern source(s). */
    name?: string;
    /** Register only if no automation with the same singleton identity exists session-wide. */
    singleton?: boolean;
    /** Auto-remove after this many fires (`1` = one-shot). */
    fireLimit?: number;
}

interface TriggerOptions {
    /** Explicit identity/display name; defaults to the pattern source(s). */
    name?: string;
    /** Also fire when a prompt is received, not only on new lines. */
    prompt?: boolean;
    /** Enabled by default (default: true). */
    enabled?: boolean;
    /** Register only if no automation with the same singleton identity exists session-wide. */
    singleton?: boolean;
    /** Auto-remove after this many fires (`1` = one-shot). */
    fireLimit?: number;
    /** Auto-remove after this many tested incoming lines (trigger-only). */
    lineLimit?: number;
}

interface TriggerDef extends TriggerPatterns {
    script: AutomationScript;
    prompt?: boolean;
    enabled?: boolean;
    singleton?: boolean;
    fireLimit?: number;
    lineLimit?: number;
}

interface TimerOptions {
    /** Explicit identity/display name; defaults to the interval + handler source. */
    name?: string;
    /** Delay between fires, in milliseconds (required). */
    intervalMs: number;
    /** Fire repeatedly (default: false -- fire once then auto-remove). */
    repeat?: boolean;
    /** Auto-remove after this many fires. */
    fireLimit?: number;
}

interface KeySpec {
    key: string;
    modifiers?: string[];
}

interface HotkeyOptions {
    /** Explicit identity/display name; defaults to the key combination. */
    name?: string;
}

/** One style span read back from a line; `begin`/`end` are byte offsets. */
interface StyleSpan {
    begin: number;
    end: number;
    fg: Color;
    bg: Color;
}

// ---- Panes --------------------------------------------------------------

/** Which side of the reference pane a split places the new pane on. */
type SplitDirection = "left" | "right" | "top" | "bottom";

/** When a pane's title bar (header/drag handle) shows: 'normal' follows the
 *  global hide-unless-toolbar rule; 'always-show' pins the header on. */
type TitleBarSpec = "normal" | "always-show";

/** The direction-independent half of the spec for `pane.split()`.
 *  `terminal: false` creates a widgets-only pane (no terminal scrollback);
 *  every pane hosts widgets either way. */
interface PaneSpecBase {
    name: string;
    terminal?: boolean;
    titleBar?: TitleBarSpec;
}

/** The spec for `pane.split()`. The initial pixel size is keyed to the split
 *  axis -- `width` on left/right splits, `height` on top/bottom -- and the
 *  off-axis dimension is a type error (the runtime ignores it regardless). */
type PaneSpec<D extends SplitDirection> = PaneSpecBase &
    (D extends "left" | "right"
        ? { width?: number; height?: never }
        : { height?: number; width?: never });

/** The wire shape a pane op returns for one pane. */
interface PaneInfoWire {
    name: string;
    kind: "terminal" | "widgets";
    isMain: boolean;
    /** The interned per-session name identity (the per-line routing fast
     *  path); null on a cross-session optimistic handle. */
    nameId: number | null;
    created: boolean;
}

/**
 * A handle to one session pane. Get-or-create via `split()` (an existing name
 * returns the existing pane); a pane is removed by `close()`, session end, or
 * the reload sweep (a reload closes panes no script re-claimed via `split()`).
 * Handles carry their owning session id -- passing another session's `Pane` to
 * a line-routing op throws.
 */
class Pane {
    _sessionId: number;
    _name: string;
    _kind: "terminal" | "widgets";
    _isMain: boolean;
    _nameId: number | null;
    /** false when `split()` returned an already-existing pane. */
    created?: boolean;

    constructor(sessionId: number, info: PaneInfoWire) {
        this._sessionId = sessionId;
        this._name = info.name;
        this._kind = info.kind;
        this._isMain = info.isMain;
        this._nameId = info.nameId;
        this.created = info.created;
    }

    /** The pane's name in its display case (identity is case-insensitive). */
    get name(): string {
        return this._name;
    }

    get kind(): "terminal" | "widgets" {
        return this._kind;
    }

    get isMain(): boolean {
        return this._isMain;
    }

    /** Write whole lines into this pane's terminal (throws on widgets-only panes).
     *  Accepts a string or a `style`/`link` fragment, and is directly usable as a
     *  template tag. */
    echo(text: string | StyledTextLike | TemplateStringsArray, ...values: unknown[]): void {
        const arg = __styled_echo_arg(text, values);
        if (__is_styled_text(arg)) {
            const { payload, callbacks } = __styled_echo_payload(arg);
            op_smudgy_pane_echo_styled(this._sessionId, this._name, payload, callbacks);
        } else {
            op_smudgy_pane_echo(this._sessionId, this._name, String(arg));
        }
    }

    /** Clear this pane's terminal scrollback (works on main; throws on widgets-only panes). */
    clear(): void {
        op_smudgy_pane_clear(this._sessionId, this._name);
    }

    /** Close this pane (throws on the main pane; idempotent otherwise). */
    close(): void {
        op_smudgy_pane_close(this._sessionId, this._name);
    }

    /** Split a new pane off this one. Get-or-create by (folded) name; an
     *  explicit `titleBar` also re-policies an existing pane (incl. main). */
    split<D extends SplitDirection>(direction: D, spec: PaneSpec<D>): Pane {
        return __smudgy_pane_split(this._sessionId, this._name, direction, spec);
    }
}

function __smudgy_pane_split(
    sessionId: number,
    refName: string,
    direction: SplitDirection,
    spec: PaneSpec<SplitDirection>,
): Pane {
    if (direction !== "left" && direction !== "right" && direction !== "top" && direction !== "bottom") {
        throw new TypeError('direction must be one of "left" | "right" | "top" | "bottom"');
    }
    if (typeof spec !== "object" || spec === null || typeof spec.name !== "string") {
        throw new TypeError(
            "spec must be an object of the form { name, width?, height?, terminal?, titleBar? }",
        );
    }
    const info = op_smudgy_pane_split(sessionId, refName, direction, {
        name: spec.name,
        width: typeof spec.width === "number" ? spec.width : null,
        height: typeof spec.height === "number" ? spec.height : null,
        terminal: spec.terminal !== undefined ? Boolean(spec.terminal) : null,
        titleBar: typeof spec.titleBar === "string" ? spec.titleBar : null,
    });
    return new Pane(sessionId, info);
}

/** The methods half of a session's pane registry; the proxy below adds
 *  dot-access (`session.panes.chat`). */
interface PaneRegistryMethods {
    get(name: string): Pane | undefined;
    list(): Pane[];
    exists(name: string): boolean;
}

type PaneRegistry = PaneRegistryMethods & { readonly [name: string]: Pane | undefined };

/**
 * Build a session's `panes` registry: `get`/`list`/`exists` plus dot-access
 * for any other string key. `get`/`list`/`exists`/`then` are reserved names
 * (`then` keeps the proxy non-thenable under `await`); reserved-name panes
 * are reachable via `get()` only in the sense that they cannot exist at all --
 * `split()` refuses to create them.
 */
function __smudgy_make_pane_registry(sessionId: number): PaneRegistry {
    const methods: PaneRegistryMethods = {
        get(name: string): Pane | undefined {
            const info = op_smudgy_pane_resolve(sessionId, String(name));
            return info === null || info === undefined ? undefined : new Pane(sessionId, info);
        },
        list(): Pane[] {
            return op_smudgy_pane_list(sessionId).map((info: PaneInfoWire) => new Pane(sessionId, info));
        },
        exists(name: string): boolean {
            const info = op_smudgy_pane_resolve(sessionId, String(name));
            return info !== null && info !== undefined;
        },
    };
    return new Proxy(methods, {
        get(target, key) {
            if (typeof key !== "string") return undefined;
            if (key === "then") return undefined;
            // Own-property check only: `key in target` would also match
            // inherited Object.prototype members, so a pane named "toString"
            // or "valueOf" would be shadowed by the prototype function on
            // dot/index access instead of resolving to the pane.
            if (Object.hasOwn(target, key)) return (target as any)[key];
            return methods.get(key);
        },
    }) as PaneRegistry;
}

/** Normalize a line-routing target to the `(nameId, name)` op arguments. A
 *  `Pane` handle passes its interned name id (`-1` = resolve by name); a
 *  handle owned by another session throws -- a name id is never applied to a
 *  foreign registry. */
function __smudgy_pane_route_arg(pane: Pane | string): [number, string] {
    if (pane instanceof Pane) {
        if (pane._sessionId !== op_smudgy_get_current_session()) {
            throw new TypeError(
                "a Pane belonging to another session cannot be used to route this session's lines",
            );
        }
        return [pane._nameId === null ? -1 : pane._nameId, pane._name];
    }
    if (typeof pane === "string") {
        return [-1, pane];
    }
    throw new TypeError("expected a Pane or a pane name");
}

// ---- Session ----------------------------------------------------------------

/**
 * Represents a session in the Smudgy MUD client.
 *
 * A handle: every member routes to the session the handle names, own or foreign.
 * Cross-session calls are gated by the `reach-others` capability for sandboxed
 * packages, and pane introspection (`panes.get`/`list`/`exists`) is own-session
 * only (see `__smudgy_make_pane_registry`); everything else routes by id.
 */
class Session {
    _id: number;

    constructor(id: number) {
        this._id = id;
    }

    /** The ID of the session. */
    get id(): number {
        return this._id;
    }

    /** Echo a line of text to this session's terminal (local; not sent to the MUD).
     *  Accepts a string or a `style`/`link` fragment, and is directly usable as a
     *  template tag. */
    echo(line: string | StyledTextLike | TemplateStringsArray, ...values: unknown[]): void {
        const arg = __styled_echo_arg(line, values);
        if (__is_styled_text(arg)) {
            const { payload, callbacks } = __styled_echo_payload(arg);
            op_smudgy_session_echo_styled(this.id, payload, callbacks);
        } else {
            op_smudgy_session_echo(this.id, String(arg));
        }
    }

    /** Reload this session's scripts (rebuilds its engine). */
    reload(): void {
        op_smudgy_session_reload(this.id);
    }

    /** Send a line to the MUD, processed exactly as if typed by the user. */
    send(line: string): void {
        op_smudgy_session_send(this.id, line);
    }

    /** Send a raw line to the MUD with no processing. */
    sendRaw(line: string): void {
        op_smudgy_session_send_raw(this.id, line);
    }

    /** The profile (name + subtext) associated with this session. */
    get profile(): Profile {
        return op_smudgy_get_session_character(this.id);
    }

    /** This session's main (output + input) pane. */
    get mainPane(): Pane {
        return new Pane(this.id, {
            name: "main",
            kind: "terminal",
            isMain: true,
            // The main pane's name id is 0 in every session's registry.
            nameId: 0,
            created: false,
        });
    }

    /** This session's pane registry (`get`/`list`/`exists` + dot access).
     *  Introspection is own-session only; a foreign session's `get`/`list`/
     *  `exists` throw (pane mutations still route by name). */
    get panes(): PaneRegistry {
        return __smudgy_make_pane_registry(this.id);
    }

    toString(): string {
        return `Session(${this.id})`;
    }
}

/**
 * A handle to a script-created alias. `enabled` is a live get+set, `pattern` reads back
 * the first pattern's source, and `delete()` removes it. All three are origin-scoped.
 */
class Alias {
    name: string;
    _creatorId: number;
    created?: boolean;

    constructor(name: string, creatorId: number) {
        this.name = name;
        this._creatorId = creatorId;
    }

    /** Whether the alias is currently enabled (reads the live registry). */
    get enabled(): boolean {
        const view = op_smudgy_get_alias(this._creatorId, this.name);
        return view ? view.enabled : false;
    }

    set enabled(value: boolean) {
        op_smudgy_set_alias_enabled(this._creatorId, this.name, value);
    }

    /** The first pattern's source string, or "" if the alias no longer exists. */
    get pattern(): string {
        const view = op_smudgy_get_alias(this._creatorId, this.name);
        return view ? view.pattern : "";
    }

    /** Remove the alias. Idempotent. */
    delete(): void {
        op_smudgy_remove_alias(this._creatorId, this.name);
    }
}

/** A handle to a script-created trigger. Mirrors {@link Alias}. */
class Trigger {
    name: string;
    _creatorId: number;
    created?: boolean;

    constructor(name: string, creatorId: number) {
        this.name = name;
        this._creatorId = creatorId;
    }

    /** Whether the trigger is currently enabled (reads the live registry). */
    get enabled(): boolean {
        const view = op_smudgy_get_trigger(this._creatorId, this.name);
        return view ? view.enabled : false;
    }

    set enabled(value: boolean) {
        op_smudgy_set_trigger_enabled(this._creatorId, this.name, value);
    }

    /** The first pattern's source string, or "" if the trigger no longer exists. */
    get pattern(): string {
        const view = op_smudgy_get_trigger(this._creatorId, this.name);
        return view ? view.pattern : "";
    }

    /** Remove the trigger. Idempotent. */
    delete(): void {
        op_smudgy_remove_trigger(this._creatorId, this.name);
    }
}

/** The current Session (the one whose script is running). The session id is constant for
 *  the life of a runtime, so the returned handle is stable. */
const getCurrentSession = (): Session => new Session(op_smudgy_get_current_session());

/** All of the user's connected sessions. The set changes as sessions connect/disconnect,
 *  so this is a function (read live), not a snapshot value. For sandboxed packages the
 *  enumeration itself is the `reach-others` capability (see the Session class doc). */
const getSessions = (): Session[] =>
    op_smudgy_get_sessions().map((id: number) => new Session(id));

/** The current session's profile (name + subtext), read live. */
const getProfile = (): Profile => getCurrentSession().profile;

/** The current app settings (command separator, fonts, theme, resolved palette, ...), read
 *  live. Read-only: a script can inspect settings but not change them. */
const getSettings = (): Settings => op_smudgy_get_settings();
/** This package/isolate's own data dir (`$DATA`) as an absolute path -- where its `read`/`write`
 *  grants resolve. Build file paths from it (relative paths resolve against the process cwd, not
 *  here). Ungated. */
const getDataDir = (): string => op_smudgy_data_dir();

/** Look a session up by its profile name. Returns the first match, or undefined. */
const byName = (name: string): Session | undefined =>
    getSessions().find((s) => {
        const profile = s.profile;
        return profile !== undefined && profile !== null && profile.name === name;
    });

/** Send a line of text to the current session. */
const send = (line: string): void => getCurrentSession().send(line);

/** Send a line of text to the current session without any processing. */
const sendRaw = (line: string): void => getCurrentSession().sendRaw(line);

/** Echo a line of text to the current session's output. Accepts a string or a
 *  `style`/`link` fragment, and is directly usable as a template tag:
 *  `` echo`hi ${style.red`there`}` ``. */
const echo = (
    line: string | StyledTextLike | TemplateStringsArray,
    ...values: unknown[]
): void => getCurrentSession().echo(line, ...values);

/** Reload the current session's scripts (rebuilds its engine). */
const reload = (): void => getCurrentSession().reload();

// ---- Automation creation ----------------------------------------------------

/**
 * Validate an optional positive-integer self-limit option (`fireLimit`/`lineLimit`) and
 * normalize it to the `0`-means-unbounded wire integer the create ops expect.
 */
function normalizeSelfLimit(options: Record<string, any>, key: string): number {
    if (!(key in options) || options[key] === undefined) {
        return 0;
    }
    const value = options[key];
    if (typeof value !== "number" || !Number.isInteger(value) || value < 1) {
        throw new TypeError(`Option "${key}" must be a positive integer`);
    }
    return value;
}

/**
 * Validate an automation name with the host's `naming::validate_name` (the single source of
 * truth, via {@link op_smudgy_validate_name}).
 */
function validateAutomationName(name: unknown): void {
    if (typeof name !== "string") {
        throw new TypeError(`Name must be a string. You provided: ${typeof name}`);
    }
    const error = op_smudgy_validate_name(name);
    if (error) {
        throw new TypeError(error);
    }
}

/**
 * Resolve an automation's identity name: the explicit `options.name` when given (validated
 * with the host's naming rule, like a name typed into the automations editor), else the
 * derived self-description (`derive()`). Derived names skip `validate_name` on purpose --
 * they are pattern/key/handler text, full of characters the (filename-safe) rule rejects,
 * and ephemeral automations never touch disk.
 */
function resolveAutomationName(explicit: unknown, derive: () => string): string {
    if (explicit === undefined) {
        return derive();
    }
    validateAutomationName(explicit);
    return explicit as string;
}

/** The derived identity of a pattern-matched automation: its pattern source(s). */
function derivePatternName(patternList: string[]): string {
    return patternList.join(" | ");
}

// --- Name-first deprecation shim (DEPRECATED-NAME-FIRST -- remove in 0.5) -------------
// Before 0.4 every create* took the automation name as its first argument. Installed
// packages are versioned artifacts that keep making such calls until re-published, so the
// creation entry points honor the old shape through the 0.4 line: the positional name
// moves into options.name (identical identity semantics: replace key, singleton identity,
// registry key) and a notice is echoed once per script and function. Detection is exact,
// never a heuristic: a new-form createAlias/createTrigger third argument can only be an
// options object (a string or function there is the old form's script slot), and
// TimerOptions/KeySpec are never strings. A version tripwire
// (script_typings.rs::name_first_shim_is_removed_by_0_5) fails the build if this section
// survives into 0.5.

/** Build the once-per-function deprecation notifier for one creator. */
function __smudgy_make_deprecation_warner(
    creator: { kind: string; owner?: string; name?: string },
): (fn: string, newForm: string) => void {
    const warned = new Set<string>();
    return (fn, newForm) => {
        if (warned.has(fn)) return;
        warned.add(fn);
        echo(
            `[deprecated] ${fn} was called name-first (from ${__smudgy_producer_spec(creator)}): ` +
                `the name argument moved into options -- ${newForm}. ` +
                `The old form stops working in smudgy 0.5.`,
        );
    };
}

/** Fold an old-form positional name into the (optional) trailing options bag. */
function __smudgy_adopt_positional_name<T extends { name?: string }>(
    fn: string,
    name: unknown,
    options: unknown,
): T {
    if (options !== undefined && (typeof options !== "object" || options === null)) {
        throw new TypeError("Options must be an object");
    }
    const bag = (options ?? {}) as { name?: string };
    if (bag.name !== undefined) {
        throw new TypeError(`${fn}: a positional name and options.name cannot both be given`);
    }
    return { ...bag, name } as T;
}

/** Creates an alias that matches input patterns and executes a script. */
function createAlias(
    creatorId: number,
    patterns: Pattern | Pattern[],
    script: AutomationScript,
    options: AliasOptions = {},
): Alias {
    if (typeof options !== "object" || options === null) {
        throw new TypeError("Options must be an object");
    }
    if ("singleton" in options && typeof options.singleton !== "boolean") {
        throw new TypeError('Option "singleton" must be a boolean');
    }
    if (typeof script !== "string" && typeof script !== "function") {
        throw new TypeError("Script must be a string or function");
    }
    // Reject unknown keys (parity with createTrigger), so a fat-fingered opt-in like
    // {singletonn:true} fails loudly instead of silently registering a non-singleton alias.
    const unexpectedOptions = Object.keys(options).filter(
        (key) => key !== "name" && key !== "singleton" && key !== "fireLimit",
    );
    if (unexpectedOptions.length > 0) {
        throw new TypeError(`Unexpected option(s): ${unexpectedOptions.join(", ")}`);
    }
    const fireLimit = normalizeSelfLimit(options, "fireLimit");

    const patternList = (Array.isArray(patterns) ? patterns : [patterns]).map((p) =>
        p instanceof RegExp ? p.source : p,
    );
    const name = resolveAutomationName(options.name, () => derivePatternName(patternList));

    const singleton = options.singleton ?? false;
    let created: boolean;
    if (script instanceof Function) {
        // Pass the handler's source (`toString()`) in good faith so the automations window can
        // show it read-only; the host treats it as display-only and never executes it.
        created = op_smudgy_create_javascript_function_alias(creatorId, name, patternList, script, singleton, fireLimit, script.toString());
    } else {
        created = op_smudgy_create_simple_alias(creatorId, name, patternList, script, singleton, fireLimit);
    }

    const alias = new Alias(name, creatorId);
    alias.created = created;
    return alias;
}

/** Creates multiple triggers from an object of trigger definitions; the keys become the
 *  triggers' (explicit) names -- which is the point of the batch form: the returned handle
 *  map addresses stage chains by key, and multi-pattern triggers get a readable label. */
function createTriggers(
    creatorId: number,
    triggers: Record<string, TriggerDef>,
): Record<string, Trigger> {
    return Object.fromEntries(
        Object.entries(triggers).map(([name, triggerDef]) => {
            const {
                script,
                patterns,
                rawPatterns,
                antiPatterns,
                prompt,
                enabled,
                singleton,
                fireLimit,
                lineLimit,
            } = triggerDef;

            const validPatterns: TriggerPatterns = {
                ...(patterns && { patterns }),
                ...(rawPatterns && { rawPatterns }),
                ...(antiPatterns && { antiPatterns }),
            };

            const options: TriggerOptions = {
                name,
                ...(prompt !== undefined && { prompt }),
                ...(enabled !== undefined && { enabled }),
                ...(singleton !== undefined && { singleton }),
                ...(fireLimit !== undefined && { fireLimit }),
                ...(lineLimit !== undefined && { lineLimit }),
            };

            return [name, createTrigger(creatorId, validPatterns, script, options)] as [
                string,
                Trigger,
            ];
        }),
    );
}

/** Creates a new trigger. */
function createTrigger(
    creatorId: number,
    patterns: Pattern | TriggerPatterns,
    script: AutomationScript,
    options: TriggerOptions = {},
): Trigger {
    const params = validateCreateTriggerParams(patterns, script, options);

    const singleton = options.singleton ?? false;
    const fireLimit = normalizeSelfLimit(options, "fireLimit");
    const lineLimit = normalizeSelfLimit(options, "lineLimit");
    let created: boolean;
    if (typeof script === "function") {
        created = op_smudgy_create_javascript_function_trigger(
            creatorId,
            params.name,
            params.normalizedPatterns.patterns,
            params.normalizedPatterns.rawPatterns,
            params.normalizedPatterns.antiPatterns,
            script,
            options.prompt ?? false,
            options.enabled ?? true,
            singleton,
            fireLimit,
            lineLimit,
            // Pass the handler's source (`toString()`) in good faith for the read-only detail
            // pane; the host treats it as display-only and never executes it.
            script.toString(),
        );
    } else {
        created = op_smudgy_create_simple_trigger(
            creatorId,
            params.name,
            params.normalizedPatterns.patterns,
            params.normalizedPatterns.rawPatterns,
            params.normalizedPatterns.antiPatterns,
            script,
            options.prompt ?? false,
            options.enabled ?? true,
            singleton,
            fireLimit,
            lineLimit,
        );
    }

    const trigger = new Trigger(params.name, creatorId);
    trigger.created = created;
    return trigger;
}

interface NormalizedPatterns {
    patterns: string[];
    rawPatterns: string[];
    antiPatterns: string[];
}

interface NormalizedTriggerParams {
    name: string;
    normalizedPatterns: NormalizedPatterns;
    script: AutomationScript;
}

/** Validates and normalizes parameters for creating a trigger. */
function validateCreateTriggerParams(
    patterns: Pattern | TriggerPatterns,
    script: AutomationScript,
    options: TriggerOptions,
): NormalizedTriggerParams {
    if (typeof options !== "object" || options === null) {
        throw new TypeError("Options must be an object");
    }

    if ("prompt" in options && typeof options.prompt !== "boolean") {
        throw new TypeError('Option "prompt" must be a boolean');
    }

    if ("enabled" in options && typeof options.enabled !== "boolean") {
        throw new TypeError('Option "enabled" must be a boolean');
    }

    if ("singleton" in options && typeof options.singleton !== "boolean") {
        throw new TypeError('Option "singleton" must be a boolean');
    }

    // Check for unexpected options
    const validOptions = ["name", "prompt", "enabled", "singleton", "fireLimit", "lineLimit"];
    const unexpectedOptions = Object.keys(options).filter((key) => !validOptions.includes(key));
    if (unexpectedOptions.length > 0) {
        throw new TypeError(`Unexpected option(s): ${unexpectedOptions.join(", ")}`);
    }

    if (
        typeof patterns !== "string" && !(patterns instanceof RegExp) &&
        typeof patterns !== "object"
    ) {
        throw new TypeError(
            "Patterns must be a string, RegExp, or an object with pattern properties",
        );
    }

    if (typeof script !== "string" && typeof script !== "function") {
        throw new TypeError("Script must be a string or function");
    }

    const normalizedPatterns = normalizePatterns(patterns);
    if (
        normalizedPatterns.patterns.length === 0 &&
        normalizedPatterns.rawPatterns.length === 0
    ) {
        throw new TypeError("At least one pattern or raw pattern must be provided");
    }

    const name = resolveAutomationName(options.name, () =>
        derivePatternName([...normalizedPatterns.patterns, ...normalizedPatterns.rawPatterns]),
    );

    return { name, normalizedPatterns, script };
}

/** Normalizes the patterns for a trigger. */
function normalizePatterns(patterns: Pattern | TriggerPatterns): {
    patterns: string[];
    rawPatterns: string[];
    antiPatterns: string[];
} {
    const normalized = {
        patterns: [] as string[],
        rawPatterns: [] as string[],
        antiPatterns: [] as string[],
    };

    if (typeof patterns === "string" || patterns instanceof RegExp) {
        normalized.patterns = [patterns instanceof RegExp ? patterns.source : patterns];
    } else if (typeof patterns === "object") {
        normalized.patterns = (patterns.patterns || []).map((p) =>
            p instanceof RegExp ? p.source : p,
        );
        normalized.rawPatterns = (patterns.rawPatterns || []).map((p) =>
            p instanceof RegExp ? p.source : p,
        );
        normalized.antiPatterns = (patterns.antiPatterns || []).map((p) =>
            p instanceof RegExp ? p.source : p,
        );
    }

    return normalized;
}

/** A creator-bound introspection registry: `.get`/`.list`/`.exists` over owned names. */
interface AutomationRegistry<H> {
    get(name: string): H | undefined;
    list(): string[];
    exists(name: string): boolean;
}

/**
 * Build the creator-bound `triggers` / `aliases` introspection registries. Each registry
 * is origin-scoped to its creator and reads the runtime's live introspection mirror.
 */
function __smudgy_make_registries(creatorId: number): {
    triggers: AutomationRegistry<Trigger>;
    aliases: AutomationRegistry<Alias>;
} {
    const triggers: AutomationRegistry<Trigger> = Object.freeze({
        get(name: string) {
            return op_smudgy_trigger_exists(creatorId, name)
                ? new Trigger(name, creatorId)
                : undefined;
        },
        list() {
            return op_smudgy_list_triggers(creatorId);
        },
        exists(name: string) {
            return op_smudgy_trigger_exists(creatorId, name);
        },
    });
    const aliases: AutomationRegistry<Alias> = Object.freeze({
        get(name: string) {
            return op_smudgy_alias_exists(creatorId, name)
                ? new Alias(name, creatorId)
                : undefined;
        },
        list() {
            return op_smudgy_list_aliases(creatorId);
        },
        exists(name: string) {
            return op_smudgy_alias_exists(creatorId, name);
        },
    });
    return { triggers, aliases };
}

// ---- Timers / hotkeys -------------------------------------------------------

/**
 * A handle to a script-created managed timer. Named, tracked in the `timers` registry, and
 * cleared automatically on session reload (timers do NOT survive reload). `enabled` is a
 * live get+set; `delete()` stops and unregisters it.
 */
class Timer {
    name: string;
    _intervalMs: number;
    _repeat: boolean;
    _fireLimit: number;
    _handler: () => void;
    _registry: Map<string, Timer>;
    _fires: number;
    _handle: any;
    _enabled: boolean;

    constructor(name: string, options: TimerOptions, handler: () => void, registry: Map<string, Timer>) {
        this.name = name;
        this._intervalMs = options.intervalMs;
        this._repeat = options.repeat ?? false;
        this._fireLimit = options.fireLimit ?? 0;
        this._handler = handler;
        this._registry = registry;
        this._fires = 0;
        this._handle = null;
        this._enabled = false;
        this.start();
    }

    /** One tick: run the handler, count the fire, and either rearm (repeat) or self-remove. */
    _tick(): void {
        this._fires++;
        try {
            this._handler();
        } finally {
            const hitLimit = this._fireLimit > 0 && this._fires >= this._fireLimit;
            if (!this._repeat || hitLimit) {
                this.delete();
            }
        }
    }

    /** (Re)start the timer if it isn't already running. */
    start(): void {
        if (this._enabled) return;
        this._enabled = true;
        if (this._repeat) {
            this._handle = setInterval(() => this._tick(), this._intervalMs);
        } else {
            this._handle = setTimeout(() => this._tick(), this._intervalMs);
        }
    }

    /** Pause the timer without unregistering it (a later `enabled = true` resumes it). */
    stop(): void {
        if (!this._enabled) return;
        this._enabled = false;
        if (this._repeat) {
            clearInterval(this._handle);
        } else {
            clearTimeout(this._handle);
        }
        this._handle = null;
    }

    /** Whether the timer is currently running. */
    get enabled(): boolean {
        return this._enabled;
    }

    set enabled(value: boolean) {
        if (value) {
            this.start();
        } else {
            this.stop();
        }
    }

    /** Stop the timer and remove it from its registry. Idempotent. */
    delete(): void {
        this.stop();
        if (this._registry.get(this.name) === this) {
            this._registry.delete(this.name);
        }
    }
}

/**
 * A handle to a script-created hotkey. `enabled` is JS-tracked (get+set); setting it
 * `false` unbinds the key (a later `true` rebinds), and `delete()` unbinds + unregisters.
 * Cleared on reload (the engine rebuild drops all hotkeys).
 */
class Hotkey {
    name: string;
    _keySpec: KeySpec;
    _handler: () => void;
    _creatorId: number;
    _registry: Map<string, Hotkey>;
    _enabled: boolean;

    constructor(name: string, keySpec: KeySpec, handler: () => void, creatorId: number, registry: Map<string, Hotkey>) {
        this.name = name;
        this._keySpec = keySpec;
        this._handler = handler;
        this._creatorId = creatorId;
        this._registry = registry;
        this._enabled = false;
        this.enabled = true;
    }

    /** Whether the key is currently bound. */
    get enabled(): boolean {
        return this._enabled;
    }

    set enabled(value: boolean) {
        if (value === this._enabled) return;
        this._enabled = value;
        if (value) {
            op_smudgy_create_hotkey(
                this._creatorId,
                this.name,
                this._keySpec.key,
                this._keySpec.modifiers ?? [],
                this._handler,
            );
        } else {
            op_smudgy_remove_hotkey(this._creatorId, this.name);
        }
    }

    /** Unbind and unregister the hotkey. Idempotent. */
    delete(): void {
        if (this._enabled) {
            op_smudgy_remove_hotkey(this._creatorId, this.name);
            this._enabled = false;
        }
        if (this._registry.get(this.name) === this) {
            this._registry.delete(this.name);
        }
    }
}

/**
 * Build the creator-bound `timers` and `hotkeys` registries plus their
 * `createTimer`/`createHotkey` factories.
 */
function __smudgy_make_timer_hotkey_api(
    creatorId: number,
    warnNameFirst: (fn: string, newForm: string) => void,
) {
    const timerMap = new Map<string, Timer>();
    const hotkeyMap = new Map<string, Hotkey>();

    const createTimerImpl = (options: TimerOptions, handler: () => void): Timer => {
        if (typeof options !== "object" || options === null) {
            throw new TypeError("Options must be an object");
        }
        if (typeof options.intervalMs !== "number" || !(options.intervalMs >= 0)) {
            throw new TypeError('Option "intervalMs" must be a non-negative number');
        }
        if (typeof handler !== "function") {
            throw new TypeError("Handler must be a function");
        }
        // A timer has no self-describing key the way a trigger has its pattern, so the
        // derived identity folds in the handler source: same interval + same code is the
        // same timer (idempotent re-create), different code stays distinct.
        const name = resolveAutomationName(
            options.name,
            () => `${options.intervalMs}ms: ${handler.toString().replace(/\s+/g, " ")}`,
        );
        // Upsert: a same-named timer is replaced (its old interval is cleared first).
        const existing = timerMap.get(name);
        if (existing !== undefined) existing.delete();
        const timer = new Timer(name, options, handler, timerMap);
        timerMap.set(name, timer);
        return timer;
    };

    const createHotkeyImpl = (keySpec: KeySpec, handler: () => void, options: HotkeyOptions = {}): Hotkey => {
        if (typeof keySpec !== "object" || keySpec === null || typeof keySpec.key !== "string") {
            throw new TypeError('keySpec must be an object of the form { key, modifiers? }');
        }
        if (typeof handler !== "function") {
            throw new TypeError("Handler must be a function");
        }
        if (typeof options !== "object" || options === null) {
            throw new TypeError("Options must be an object");
        }
        const unexpectedOptions = Object.keys(options).filter((key) => key !== "name");
        if (unexpectedOptions.length > 0) {
            throw new TypeError(`Unexpected option(s): ${unexpectedOptions.join(", ")}`);
        }
        // Derived identity: the canonicalized key combination ("ctrl+shift+h") -- one
        // binding per combination, which is also how the host resolves a pressed key.
        const name = resolveAutomationName(options.name, () =>
            [...(keySpec.modifiers ?? []).map((m) => String(m).toLowerCase()).sort(), keySpec.key]
                .join("+"),
        );
        // Upsert: replace a same-named hotkey (its old binding is removed first).
        const existing = hotkeyMap.get(name);
        if (existing !== undefined) existing.delete();
        const hotkey = new Hotkey(name, keySpec, handler, creatorId, hotkeyMap);
        hotkeyMap.set(name, hotkey);
        return hotkey;
    };

    // DEPRECATED-NAME-FIRST entry shims (see the shim section above; remove in 0.5). The
    // new form never has a string first argument, so the branch is exact.
    const createTimer = ((...args: any[]) => {
        if (typeof args[0] === "string") {
            warnNameFirst("createTimer", 'createTimer({ ...options, name: "..." }, handler)');
            return createTimerImpl(
                __smudgy_adopt_positional_name<TimerOptions>("createTimer", args[0], args[1]),
                args[2],
            );
        }
        return createTimerImpl(args[0], args[1]);
    }) as (options: TimerOptions, handler: () => void) => Timer;

    const createHotkey = ((...args: any[]) => {
        if (typeof args[0] === "string") {
            warnNameFirst("createHotkey", 'createHotkey(keySpec, handler, { name: "..." })');
            return createHotkeyImpl(args[1], args[2], { name: args[0] });
        }
        return createHotkeyImpl(args[0], args[1], args[2]);
    }) as (keySpec: KeySpec, handler: () => void, options?: HotkeyOptions) => Hotkey;

    const timers: AutomationRegistry<Timer> = Object.freeze({
        get(name: string) {
            return timerMap.get(name);
        },
        list() {
            return [...timerMap.keys()];
        },
        exists(name: string) {
            return timerMap.has(name);
        },
    });

    const hotkeys: AutomationRegistry<Hotkey> = Object.freeze({
        get(name: string) {
            return hotkeyMap.get(name);
        },
        list() {
            return [...hotkeyMap.keys()];
        },
        exists(name: string) {
            return hotkeyMap.has(name);
        },
    });

    return { createTimer, createHotkey, timers, hotkeys };
}

// ---- Line / buffer ----------------------------------------------------------

/**
 * UTF-8 byte length of `s`. The line-editing ops address text by **byte** offset into the
 * line's UTF-8 storage, but JS string indices (`indexOf`, `.length`) count UTF-16 code
 * units. The string-based `replace`/`highlight`/`remove` helpers use this to convert a
 * matched substring's code-unit span to the byte span the ops expect, so they stay correct
 * for non-ASCII lines (accents, emoji, ...) instead of mis-slicing or panicking. Counts an
 * allocation-free single pass over the string.
 */
function __utf8ByteLength(s: string): number {
    let bytes = 0;
    for (let i = 0; i < s.length; i++) {
        const code = s.charCodeAt(i);
        if (code < 0x80) {
            bytes += 1;
        } else if (code < 0x800) {
            bytes += 2;
        } else if (code >= 0xd800 && code <= 0xdbff) {
            // High surrogate: a supplementary code point encoded as a 4-byte UTF-8
            // sequence; consume its trailing low surrogate so it isn't counted twice.
            bytes += 4;
            i++;
        } else {
            bytes += 3;
        }
    }
    return bytes;
}

/**
 * A line you can read and edit. ONE `Line` type for both targets:
 *   - `line` is bound to the CURRENT in-flight incoming line.
 *   - `buffer.line(n)` is bound to the already-emitted line number `n`.
 * The line number is captured in the handle, never passed as a leading argument. All
 * `Line` access is current-session-only.
 */
class Line {
    _lineNumber: number | null;

    constructor(lineNumber: number | null) {
        this._lineNumber = lineNumber;
    }

    /** Whether this handle targets the current in-flight line. */
    get _isCurrent(): boolean {
        return this._lineNumber === null;
    }

    /** Route a styled splice to the right op for this handle's target. */
    _splice(text: StyledTextImpl, begin: number, end: number, options: ColorOptions): void {
        const { runs, callbacks } = __styled_splice_args(text, options);
        if (this._isCurrent) {
            op_smudgy_splice(runs, begin, end, callbacks);
        } else {
            op_smudgy_line_splice(this._lineNumber, runs, begin, end, callbacks);
        }
    }

    /** Inserts text at the specified position with optional styling. Styled text
     *  splices with its own colors and links; `options` is then the base its unset
     *  colors inherit from. */
    insert(
        text: string | StyledTextLike,
        begin: number,
        end: number = begin,
        options: ColorOptions = {},
    ): void {
        if (__is_styled_text(text)) {
            this._splice(text, begin, end, options);
            return;
        }
        if (this._isCurrent) {
            op_smudgy_insert(text as string, begin, end, options.fg || null, options.bg || null);
        } else {
            op_smudgy_line_insert(this._lineNumber, text as string, begin, end, options.fg || null, options.bg || null);
        }
    }

    /** Replaces text in the specified byte range. Styled text splices with its own
     *  colors and links; its unstyled parts inherit the style at the splice point. */
    replaceAt(text: string | StyledTextLike, begin: number, end: number): void {
        if (__is_styled_text(text)) {
            this._splice(text, begin, end, {});
            return;
        }
        if (this._isCurrent) {
            op_smudgy_replace(text as string, begin, end);
        } else {
            op_smudgy_line_replace(this._lineNumber, text as string, begin, end);
        }
    }

    /** Highlights text in the specified byte range with the given colors. */
    highlightAt(begin: number, end: number, options: ColorOptions = {}): void {
        if (this._isCurrent) {
            op_smudgy_highlight(begin, end, options.fg || null, options.bg || null);
        } else {
            op_smudgy_line_highlight(this._lineNumber, begin, end, options.fg || null, options.bg || null);
        }
    }

    /** Removes text in the specified byte range. */
    removeAt(begin: number, end: number): void {
        if (this._isCurrent) {
            op_smudgy_remove(begin, end);
        } else {
            op_smudgy_line_remove(this._lineNumber, begin, end);
        }
    }

    /** Replaces the first occurrence of `oldStr` with `newStr` (plain or styled).
     *  The search side is always plain text. Returns whether it matched. */
    replace(oldStr: string, newStr: string | StyledTextLike): boolean {
        const currentText = this.text;
        const index = currentText.indexOf(oldStr);
        if (index === -1) {
            return false;
        }
        const begin = __utf8ByteLength(currentText.slice(0, index));
        this.replaceAt(newStr, begin, begin + __utf8ByteLength(oldStr));
        return true;
    }

    /** Highlights the first occurrence of `str`. Returns whether it matched. */
    highlight(str: string, options: ColorOptions = {}): boolean {
        const currentText = this.text;
        const index = currentText.indexOf(str);
        if (index === -1) {
            return false;
        }
        const begin = __utf8ByteLength(currentText.slice(0, index));
        this.highlightAt(begin, begin + __utf8ByteLength(str), options);
        return true;
    }

    /** Removes the first occurrence of `str`. Returns whether it matched. */
    remove(str: string): boolean {
        const currentText = this.text;
        const index = currentText.indexOf(str);
        if (index === -1) {
            return false;
        }
        const begin = __utf8ByteLength(currentText.slice(0, index));
        this.removeAt(begin, begin + __utf8ByteLength(str));
        return true;
    }

    /** Prevents the current line from being displayed (gags it). No-op on a buffer line. */
    gag(): void {
        if (this._isCurrent) {
            op_smudgy_gag();
        }
    }

    /** Gag the current line from main and deliver it to `pane` instead
     *  (repeated calls: last wins). Styling is retained; transforms still
     *  apply. One-shot, current-line only (no-op on a buffer line). */
    redirect(pane: Pane | string): void {
        if (this._isCurrent) {
            const [nameId, name] = __smudgy_pane_route_arg(pane);
            op_smudgy_redirect(nameId, name);
        }
    }

    /** Additionally deliver the current line to `pane` (sinks are a
     *  deduplicated set). One-shot, current-line only (no-op on a buffer line). */
    copy(pane: Pane | string): void {
        if (this._isCurrent) {
            const [nameId, name] = __smudgy_pane_route_arg(pane);
            op_smudgy_copy(nameId, name);
        }
    }

    /** The line's text. For a `buffer.line(n)` outside the recent-lines window this is "". */
    get text(): string {
        if (this._isCurrent) {
            return op_smudgy_get_current_line();
        }
        const text = op_smudgy_buffer_get_text(this._lineNumber);
        return text === null ? "" : text;
    }

    /** The line's style spans. For a `buffer.line(n)` outside the window this is `undefined`. */
    get styles(): StyleSpan[] | undefined {
        if (this._isCurrent) {
            return op_smudgy_get_current_line_styles();
        }
        const styles = op_smudgy_buffer_get_styles(this._lineNumber);
        return styles === null ? undefined : styles;
    }

    /** The line's number. The current line reports the number it will be assigned on emit. */
    get number(): number {
        if (this._isCurrent) {
            return op_smudgy_get_current_line_number();
        }
        return this._lineNumber as number;
    }
}

/** The current in-flight incoming line. */
const line = new Line(null);

/** Access to already-emitted lines by number. Only the most recent `RECENT_LINES` (currently
 *  1000) lines are readable. Current-session-only. */
const buffer = {
    line(line_number: number): Line {
        return new Line(line_number);
    },
};

/**
 * From an **alias** handler, controls whether the original command you typed (the one that
 * matched this alias) is still sent to the game.
 *
 * By default an alias *replaces* the command that triggered it: the typed line is captured
 * (consumed) and only what your handler sends reaches the game. Call `capture(false)` to let
 * the original line pass through to the game as well, so the alias augments the command
 * instead of replacing it. `capture(true)` restores the default within the same handler.
 *
 * Capturing is one-way for a given input line: once this alias (or any other alias matching
 * the same line, or an inline send/template alias) has captured it, a later `capture(false)`
 * cannot bring the line back.
 *
 * No effect in a **trigger** (incoming-line) handler: an incoming line is always displayed
 * regardless of `capture()`. To hide an incoming line, use `line.gag()` instead.
 */
const capture: (value: boolean) => void = op_smudgy_capture;

// ---- vars (server-scoped persistent store) ----------------------------------

const VARS_PREFIX = "smudgy.vars/";

function __vars_try_parse(raw: string): any {
    try {
        return JSON.parse(raw);
    } catch (_e) {
        return undefined;
    }
}

/**
 * Wrap a parsed value so that deep mutations write the whole value back to its
 * `localStorage` key. Children are wrapped lazily on access.
 */
function __vars_persist_proxy(root: object, persist: () => void): any {
    const wrap = (obj: object): any =>
        new Proxy(obj, {
            get(target, key) {
                const value = Reflect.get(target, key);
                return value !== null && typeof value === "object" ? wrap(value) : value;
            },
            set(target, key, value) {
                Reflect.set(target, key, value);
                persist();
                return true;
            },
            deleteProperty(target, key) {
                Reflect.deleteProperty(target, key);
                persist();
                return true;
            },
        });
    return wrap(root);
}

const vars: Record<string, any> = new Proxy(Object.create(null), {
    get(_target, key) {
        if (typeof key === "symbol") return undefined;
        const full = VARS_PREFIX + key;
        const raw = localStorage.getItem(full);
        if (raw === null) return undefined;
        const value = __vars_try_parse(raw);
        if (value !== null && typeof value === "object") {
            return __vars_persist_proxy(value, () =>
                localStorage.setItem(full, JSON.stringify(value)),
            );
        }
        return value;
    },
    set(_target, key, value) {
        if (typeof key === "symbol") return false;
        const full = VARS_PREFIX + key;
        if (value === undefined) {
            localStorage.removeItem(full);
        } else {
            localStorage.setItem(full, JSON.stringify(value));
        }
        return true;
    },
    has(_target, key) {
        if (typeof key === "symbol") return false;
        return localStorage.getItem(VARS_PREFIX + key) !== null;
    },
    deleteProperty(_target, key) {
        if (typeof key !== "symbol") localStorage.removeItem(VARS_PREFIX + key);
        return true;
    },
    ownKeys(_target) {
        const out: string[] = [];
        for (let i = 0; i < localStorage.length; i++) {
            const k = localStorage.key(i);
            if (k !== null && k.startsWith(VARS_PREFIX)) {
                out.push(k.slice(VARS_PREFIX.length));
            }
        }
        return out;
    },
    getOwnPropertyDescriptor(_target, key) {
        if (typeof key === "symbol") return undefined;
        const raw = localStorage.getItem(VARS_PREFIX + key);
        if (raw === null) return undefined;
        return {
            value: __vars_try_parse(raw),
            enumerable: true,
            configurable: true,
            writable: true,
        };
    },
});

// ---- The per-creator api object ---------------------------------------------

// ---- Persisted user-side automations ----------------------------------------
// Create/edit the REGULAR, persisted user automations (the saved aliases/triggers/hotkeys shown in
// the automations window), as opposed to the ephemeral createAlias/createTrigger/createHotkey
// runtime ones. Exposed as `userAutomations.{aliases,triggers,hotkeys}` -- a registry per kind
// shaped like the live `aliases`/`triggers` registries (save/get/list/exists/delete) whose handles
// carry a snapshot for reads and persist on explicit update/delete. Main-isolate only; a sandboxed
// package's call throws.

// The author-facing language tag <-> the persisted ScriptLang serde form. Translating in JS keeps
// the on-disk format ("JS"/"TS"/"Plaintext") unchanged while authors use lowercase tags.
const LANG_TO_WIRE: { [k: string]: string } = { plaintext: "Plaintext", js: "JS", ts: "TS" };
const LANG_FROM_WIRE: { [k: string]: ScriptLang } = { Plaintext: "plaintext", JS: "js", TS: "ts" };
const langToWire = (lang: ScriptLang | undefined): string => LANG_TO_WIRE[lang ?? "plaintext"] ?? "Plaintext";
const langFromWire = (wire: string | undefined): ScriptLang => LANG_FROM_WIRE[wire ?? "Plaintext"] ?? "plaintext";

// A trigger's `pattern`/`patterns` arrives single-or-array (the wire form collapses a 1-element
// list to the singular key); normalize either to an array, or undefined when neither is present.
const patternArray = (single: any, multi: any): string[] | undefined => {
    if (multi !== undefined && multi !== null) return multi;
    if (single !== undefined && single !== null) return [single];
    return undefined;
};

// camelCase author def -> the wire object each save op deserializes.
const aliasToWire = (def: SavedAlias) => ({
    pattern: String(def.pattern),
    enabled: def.enabled ?? true,
    language: langToWire(def.language),
    ...(def.script !== undefined ? { script: String(def.script) } : {}),
    ...(def.package !== undefined ? { package: String(def.package) } : {}),
});
const triggerToWire = (def: SavedTrigger) => ({
    enabled: def.enabled ?? true,
    prompt: def.prompt ?? false,
    language: langToWire(def.language),
    ...(def.patterns !== undefined ? { patterns: def.patterns } : {}),
    ...(def.rawPatterns !== undefined ? { raw_patterns: def.rawPatterns } : {}),
    ...(def.antiPatterns !== undefined ? { anti_patterns: def.antiPatterns } : {}),
    ...(def.script !== undefined ? { script: String(def.script) } : {}),
    ...(def.package !== undefined ? { package: String(def.package) } : {}),
});
const hotkeyToWire = (def: SavedHotkey) => ({
    key: String(def.key),
    modifiers: def.modifiers ?? [],
    enabled: def.enabled ?? true,
    language: langToWire(def.language),
    ...(def.script !== undefined ? { script: String(def.script) } : {}),
    ...(def.package !== undefined ? { package: String(def.package) } : {}),
});

// The wire def a get op returns -> the camelCase author shape.
const aliasFromWire = (w: any): SavedAlias => ({
    pattern: w.pattern,
    script: w.script ?? undefined,
    enabled: w.enabled,
    language: langFromWire(w.language),
    package: w.package ?? undefined,
});
const triggerFromWire = (w: any): SavedTrigger => ({
    patterns: patternArray(w.pattern, w.patterns),
    rawPatterns: patternArray(w.raw_pattern, w.raw_patterns),
    antiPatterns: patternArray(w.anti_pattern, w.anti_patterns),
    script: w.script ?? undefined,
    enabled: w.enabled,
    prompt: w.prompt,
    language: langFromWire(w.language),
    package: w.package ?? undefined,
});
const hotkeyFromWire = (w: any): SavedHotkey => ({
    key: w.key,
    modifiers: w.modifiers,
    script: w.script ?? undefined,
    enabled: w.enabled,
    language: langFromWire(w.language),
    package: w.package ?? undefined,
});

// One kind's bridge to its ops + translation. `read` returns the camelCase def or undefined.
interface SavedAutomationSpec<Def> {
    save(name: string, def: Def): boolean;
    remove(name: string): boolean;
    read(name: string): Def | undefined;
    list(): string[];
}

const aliasSpec: SavedAutomationSpec<SavedAlias> = {
    save: (name, def) => op_smudgy_save_user_alias(name, aliasToWire(def)),
    remove: (name) => op_smudgy_delete_user_alias(name),
    read: (name) => { const w = op_smudgy_get_user_alias(name); return w == null ? undefined : aliasFromWire(w); },
    list: () => op_smudgy_list_user_aliases(),
};
const triggerSpec: SavedAutomationSpec<SavedTrigger> = {
    save: (name, def) => op_smudgy_save_user_trigger(name, triggerToWire(def)),
    remove: (name) => op_smudgy_delete_user_trigger(name),
    read: (name) => { const w = op_smudgy_get_user_trigger(name); return w == null ? undefined : triggerFromWire(w); },
    list: () => op_smudgy_list_user_triggers(),
};
const hotkeySpec: SavedAutomationSpec<SavedHotkey> = {
    save: (name, def) => op_smudgy_save_user_hotkey(name, hotkeyToWire(def)),
    remove: (name) => op_smudgy_delete_user_hotkey(name),
    read: (name) => { const w = op_smudgy_get_user_hotkey(name); return w == null ? undefined : hotkeyFromWire(w); },
    list: () => op_smudgy_list_user_hotkeys(),
};

// A disk-backed handle: a stable reference holding a read snapshot, with explicit writes.
function __smudgy_make_saved_handle<Def>(
    spec: SavedAutomationSpec<Def>,
    name: string,
    initial: Def,
): SavedAutomationHandle<Def> {
    let snap = initial;
    return Object.freeze({
        get name() { return name; },
        def(): Def { return { ...snap }; },
        refresh(): boolean {
            const d = spec.read(name);
            if (d === undefined) return false;
            snap = d;
            return true;
        },
        update(patch: Partial<Def>): boolean {
            // Merge onto the CURRENT saved def (re-read), so a concurrent edit isn't clobbered.
            const fresh = spec.read(name) ?? snap;
            const merged = { ...fresh, ...patch };
            const changed = spec.save(name, merged);
            snap = spec.read(name) ?? merged;
            return changed;
        },
        delete(): boolean { return spec.remove(name); },
    });
}

function __smudgy_make_saved_registry<Def>(
    spec: SavedAutomationSpec<Def>,
): SavedAutomationRegistry<Def, SavedAutomationHandle<Def>> {
    return Object.freeze({
        save(name: string, def: Def) {
            const key = String(name);
            spec.save(key, def);
            return __smudgy_make_saved_handle(spec, key, spec.read(key) ?? def);
        },
        get(name: string) {
            const key = String(name);
            const d = spec.read(key);
            return d === undefined ? undefined : __smudgy_make_saved_handle(spec, key, d);
        },
        list(): string[] { return spec.list(); },
        exists(name: string): boolean { return spec.read(String(name)) !== undefined; },
        delete(name: string): boolean { return spec.remove(String(name)); },
    });
}

const userAutomations: UserAutomations = {
    aliases: __smudgy_make_saved_registry(aliasSpec),
    triggers: __smudgy_make_saved_registry(triggerSpec),
    hotkeys: __smudgy_make_saved_registry(hotkeySpec),
};

// ---- Interop handles (docs/interop.md) ---------------------------
// createState()/createEvent() producer handles + the consumer stubs the smudgy:state/ and
// smudgy:events/ scheme modules build on. Producer and consumer handles share an
// identity but not a surface (plan 4c): producers carry set/value/previousValue or emit;
// consumers carry value/previousValue/watch or on/once. Authority is host-side (the home
// gate in the store/emit ops); the narrowed surfaces are DX, not the security boundary.

/** A subscription's disposal handle (also returned by state watch). */
interface EventSubscription {
    off(): void;
}

/** A widget-binding token (plan 7): opaque data addressing a bound store path via a
 *  host-minted id. Widget props accept it anywhere a value goes; the host repaints the
 *  widget on store flushes with no JS in the update path. `fallback` is pre-serialized
 *  JSON; `format` is a display template with one `{}` placeholder. */
interface Binding<T = unknown> {
    readonly __smudgyStoreBinding: number;
    readonly fallback?: string;
    readonly format?: string;
}

interface StateHandle<T> {
    /** Mutation proxy (plan 4a): each assignment publishes a set-at-path at exactly the
     *  assigned path. Reads go through live snapshots; `.set()` stays the hot-path/bulk
     *  spelling. Assigning `.value` itself replaces the whole published value. */
    value: T;
    /** Read-only view of the state before the newest write batch (plan 5): the open
     *  batch's base while this isolate has published this turn, else the generation the
     *  last committing flush retained. Undefined before the first commit. */
    readonly previousValue: Readonly<T> | undefined;
    set(value: T): void;
    set(path: string, value: unknown): void;
    bind(path?: string, opts?: { fallback?: unknown; format?: string }): Binding<any>;
}

interface EventHandle<T> {
    emit(payload: T): void;
}

/** A procedure handle you own (interop.md 6). The implementation IS the constructor
 *  argument -- receipt is home-gated and there is exactly one implementer, so the handle
 *  itself carries no verbs. The phantom member exists only to carry the args/return types
 *  to the typings pipeline; no runtime member exists. */
interface ProcedureHandle<A, R> {
    readonly __smudgyProcedure?: (args: A) => R;
}

interface StateConsumer<T> {
    /** Read-only live view -- the same per-hop proxy as the producer's `.value`, minus the
     *  write-through: mutation traps throw (publishing is the producer's seat). The root
     *  hop is honest about non-object roots: a scalar or array root reads as the frozen
     *  materialized value, an absent (or uninstalled) producer as `undefined`. */
    readonly value: Readonly<T> | undefined;
    /** The producer's state before its newest write batch (plan 5) -- the same read-only
     *  per-hop view, anchored to the retained generation (a producer's open journal is
     *  invisible to every other isolate, so it never moves a consumer's anchor). Undefined
     *  before the producer's first commit. */
    readonly previousValue: Readonly<T> | undefined;
    watch(handler: (snapshot: Readonly<T> | undefined) => void): EventSubscription;
    watch(path: string, handler: (snapshot: unknown) => void): EventSubscription;
    onWrite(handler: (path: string, snapshot: unknown) => void): EventSubscription;
    onWrite(path: string, handler: (path: string, snapshot: unknown) => void): EventSubscription;
    bind(path?: string, opts?: { fallback?: unknown; format?: string }): Binding<any>;
}

interface EventConsumer<T> {
    on(handler: (payload: Readonly<T>) => void): EventSubscription;
    once(): Promise<Readonly<T>>;
    once(handler: (payload: Readonly<T>) => void): EventSubscription;
}

/** The consumer's view of a procedure: post (fire-and-forget), never implement. The
 *  correlated-reply ask (`.call`) is deferred (interop.md 14); the phantom member keeps the
 *  return type ready for it. */
interface ProcedureConsumer<A, R = void> {
    post(args: A): void;
    readonly __smudgyProcedure?: (args: A) => R;
}

/** What createDerived() returns (interop.md 4b): a bindable, watch-stoppable published computation. */
interface DerivedHandle<U> {
    /** Read-only live view of the computed value (undefined before the first compute). */
    readonly value: Readonly<U> | undefined;
    bind(path?: string, opts?: { fallback?: unknown; format?: string }): Binding<any>;
    off(): void;
}

/** Uniform ASCII fold -- the same fold the host applies to every structural interop name. */
function __smudgy_fold_name(name: string): string {
    let out = "";
    for (let i = 0; i < name.length; i++) {
        const c = name.charCodeAt(i);
        out += c >= 0x41 && c <= 0x5a ? String.fromCharCode(c + 0x20) : name[i];
    }
    return out;
}

/** Spell a handle name as a store path root: bare identifiers stay bare; anything else
 *  becomes a quoted bracket key (the store path grammar's other production). */
function __smudgy_name_as_path(name: string): string {
    // The empty name is the producer ROOT (the platform state modules -- smudgy:state/gmcp --
    // address the whole subtree), not a bracket-quoted empty key.
    if (name === "") return "";
    return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(name) ? name : `[${JSON.stringify(name)}]`;
}

/** Join a relative sub-path (no leading dot, per the store grammar) onto a base path.
 *  An empty base is the root of whatever the caller addresses (a handle's interned root
 *  addresses its own subtree, so proxy walks start from ""). */
function __smudgy_join_path(root: string, sub: string): string {
    if (root === "") return sub;
    return sub.startsWith("[") ? root + sub : `${root}.${sub}`;
}

/** Respell a delivered producer-relative write path relative to a handle root: the root
 *  itself is "", a path under it drops the root prefix (fold-aware; the ASCII fold preserves
 *  byte length, so the root's length indexes the delivered spelling safely). Anything else
 *  passes through producer-relative. */
function __smudgy_relative_path(root: string, path: string): string {
    const foldedRoot = __smudgy_fold_name(root);
    const foldedPath = __smudgy_fold_name(path);
    if (foldedPath === foldedRoot) return "";
    if (foldedPath.startsWith(foldedRoot)) {
        const next = path[root.length];
        if (next === ".") return path.slice(root.length + 1);
        if (next === "[") return path.slice(root.length);
    }
    return path;
}

/** Spell a property key as a path segment (the store grammar's two productions). */
function __smudgy_key_as_segment(key: string): string {
    return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(key) ? key : `[${JSON.stringify(key)}]`;
}

/**
 * An abstract root reference the value proxies resolve every hop through
 * (docs/interop-pre-gmcp-plan.md 2): the traps know only "read a tag", "list keys",
 * "materialize a subtree", "publish" -- how a path is addressed is the root's business, so
 * roots other than a producer's live head (retained generations, host-pinned views) plug in
 * without touching the traps. `write` is the seat split: present on producer roots
 * (write-through), absent on consumer roots (mutation traps throw).
 */
interface StoreRootRef {
    /** Tagged read at `path`: `undefined` = absent (a stored `null` is a value); `"o"` =
     *  object with NO payload crossed; `"a<json>"` = whole array; `"v<json>"` = scalar. */
    read(path: string): string | undefined;
    /** Own keys of the object at `path` (first-published casing, publish order), or
     *  `undefined` when the node is absent or not an object. */
    keys(path: string): string[] | undefined;
    /** Whole-subtree snapshot (parsed, unfrozen) -- the delete trap's rewrite-minus-key
     *  inherently needs the parent materialized. */
    snapshot(path: string): unknown;
    /** Publish set-at-path as this root's producer; absent on read-only (consumer) roots. */
    write?: (path: string, value: unknown) => void;
    /** Teaching error the mutation traps throw when `write` is absent; defaults to the
     *  consumer-seat message. Previous-generation views supply their own: on the producer's
     *  own handle, "publishing is the producer's seat" is nonsense advice -- the seat is
     *  right, the target (a snapshot base) is what refuses the write. */
    denied?: () => never;
}

/** An interned root id as a root reference: every hop crosses as `(rootId, subpath)`, with
 *  subpaths relative to the root's constant path prefix (the handle's own subtree, so proxy
 *  walks start from ""). `writable` is the seat split -- producer roots write through as
 *  their interned creator; consumer roots' mutation traps throw. */
function __smudgy_root_ref(rootId: number, writable: boolean): StoreRootRef {
    const store = (globalThis as any).__smudgy_store;
    return {
        read: (path: string) => store.getTaggedAt(rootId, path),
        keys: (path: string) => store.keysAt(rootId, path),
        snapshot: (path: string) => store.getAt(rootId, path),
        write: writable
            ? (path: string, value: unknown) => store.setAt(rootId, path, value)
            : undefined,
    };
}

/** The consumer variant of {@link __smudgy_root_ref}: the root id resolves lazily (and
 *  memoized) on the first read, so a spec the store cannot address yet fails at the read --
 *  exactly where it failed before interning -- never at scheme import time. */
function __smudgy_lazy_consumer_root_ref(spec: string, rootPath: string): StoreRootRef {
    const store = (globalThis as any).__smudgy_store;
    const rootId = () => __smudgy_consumer_root_id(spec, rootPath);
    return {
        read: (path: string) => store.getTaggedAt(rootId(), path),
        keys: (path: string) => store.keysAt(rootId(), path),
        snapshot: (path: string) => store.getAt(rootId(), path),
    };
}

/** The previous-generation counterpart of {@link __smudgy_root_ref} (plan 5): a read-only
 *  root reference over the same producer/prefix, resolved against the state before the
 *  newest write batch the reading isolate can observe (the host anchors per reader). The
 *  view id resolves lazily (memoized) from the base root's id, so consumer bases keep
 *  their fail-at-the-read semantics and producers pay the resolve only if `previousValue`
 *  is ever read. Reads route through the previous-view ops (the `previous*At` glue) --
 *  the head/previous split is per op, keeping the head ops' bodies free of previous
 *  machinery on the hot proxy path. `denied` overrides the mutation traps' teaching error
 *  (the producer seat routes to the snapshot-base message; consumers keep the seat
 *  default). */
function __smudgy_previous_root_ref(baseId: () => number, denied?: () => never): StoreRootRef {
    const store = (globalThis as any).__smudgy_store;
    let id: number | undefined;
    const prevId = () => {
        if (id === undefined) {
            id = op_smudgy_interop_resolve_previous_root(baseId()) as number;
        }
        return id;
    };
    return {
        read: (path: string) => store.previousGetTaggedAt(prevId(), path),
        keys: (path: string) => store.previousKeysAt(prevId(), path),
        snapshot: (path: string) => store.previousGetAt(prevId(), path),
        denied,
    };
}

/** Resolve one proxy hop: objects come back as deeper proxies with no payload crossed;
 *  arrays and scalars parse (and deep-freeze) the payload only; absent reads `undefined`. */
function __smudgy_read_hop(root: StoreRootRef, path: string): unknown {
    const tagged = root.read(path);
    if (tagged === undefined) return undefined;
    if (tagged[0] === "o") return __smudgy_make_value_proxy(root, path);
    return __smudgy_freeze_snapshot(JSON.parse(tagged.slice(1)));
}

/** The teaching error for mutation through a read-only (consumer) view. */
function __smudgy_read_only_write(): never {
    throw new TypeError(
        "smudgy: this state view is read-only -- publishing is the producer's seat (consumers read, watch, and bind)",
    );
}

/** The teaching error for mutation through the producer's own previous-generation view:
 *  the seat is right, the target is not -- previous is the snapshot base the newest writes
 *  diff against, never a write surface. */
function __smudgy_previous_view_write(): never {
    throw new TypeError(
        "smudgy: previousValue is read-only -- it is the snapshot base from before your newest writes; publish through .value or set()",
    );
}

/** Throw a read-only root's teaching error: its own `denied` override when present
 *  (previous-generation views), else the consumer-seat default. */
function __smudgy_deny_write(root: StoreRootRef): never {
    if (root.denied !== undefined) root.denied();
    __smudgy_read_only_write();
}

/** The teaching error for shape protocols no live view honors on either seat
 *  (`Object.defineProperty`, `Object.freeze`/`seal`): store entries are plain JSON data --
 *  always enumerable and configurable -- and the view stays live. Explicit traps so these
 *  protocols fail loudly here instead of no-oping against the hidden proxy target and
 *  poisoning later reads with bare invariant TypeErrors. */
function __smudgy_live_view_reshape(): never {
    throw new TypeError(
        "smudgy: a live state view cannot be redefined or frozen -- write with assignment or set(), and copy the data (spread or JSON) when you need a value of your own",
    );
}

/**
 * The live-view proxy behind `.value` on both seats (interop.md 4a, vars-normed): property
 * reads resolve per hop through tagged gets -- kind first, payload only for leaves and
 * arrays -- so a leaf read costs what the leaf costs, never the subtree. Reads are live
 * snapshots (read-your-writes within the turn); each object access mints a fresh proxy
 * (`v.stats !== v.stats`), so subtree identity is not stable -- capture an explicit copy
 * (spread for one level, JSON for the whole shape) instead of memoizing, and diff over
 * time against `previousValue`. On writable roots each assignment publishes a set-at-path
 * at exactly the assigned path, so fine-grained change notification falls out of the trap;
 * like `vars`, writing through a missing intermediate reads `undefined` and throws; create
 * intermediates with `.set(path, value)`, which builds them. `delete` rewrites the parent
 * without the key (one whole-parent set-at-path). On read-only roots the mutation traps
 * throw the root's teaching TypeError (the consumer-seat message by default; a
 * previous-generation view on the producer's own handle teaches the snapshot-base rule
 * instead). Shape protocols (`Object.defineProperty`,
 * `Object.freeze`/`seal`) throw a teaching TypeError on both seats -- entries are plain
 * JSON data and the view stays live; assignment/`set()` write, copies capture.
 */
function __smudgy_make_value_proxy(root: StoreRootRef, path: string): any {
    return new Proxy(Object.create(null), {
        get(_t, key) {
            if (typeof key === "symbol" || key === "then") return undefined;
            return __smudgy_read_hop(root, __smudgy_join_path(path, __smudgy_key_as_segment(String(key))));
        },
        set(_t, key, value) {
            const write = root.write;
            if (write === undefined) __smudgy_deny_write(root);
            if (typeof key === "symbol") return false;
            write(__smudgy_join_path(path, __smudgy_key_as_segment(String(key))), value);
            return true;
        },
        deleteProperty(_t, key) {
            const write = root.write;
            if (write === undefined) __smudgy_deny_write(root);
            if (typeof key === "symbol") return true;
            const parent = root.snapshot(path);
            if (parent === null || typeof parent !== "object" || Array.isArray(parent)) {
                return true;
            }
            const folded = __smudgy_fold_name(String(key));
            const next: Record<string, unknown> = {};
            let removed = false;
            for (const k of Object.keys(parent as object)) {
                if (__smudgy_fold_name(k) === folded) {
                    removed = true;
                } else {
                    next[k] = (parent as Record<string, unknown>)[k];
                }
            }
            if (removed) write(path, next);
            return true;
        },
        has(_t, key) {
            if (typeof key === "symbol") return false;
            return root.read(__smudgy_join_path(path, __smudgy_key_as_segment(String(key)))) !== undefined;
        },
        ownKeys() {
            return root.keys(path) ?? [];
        },
        getOwnPropertyDescriptor(_t, key) {
            if (typeof key === "symbol") return undefined;
            const target = __smudgy_join_path(path, __smudgy_key_as_segment(String(key)));
            // Presence is one tagged read (kind only for objects, no parse or freeze ever);
            // the value rides an ACCESSOR descriptor, so enumeration protocols that call
            // [[GetOwnProperty]] per key but only inspect attributes (Object.keys, for-in)
            // stay O(keys) -- the hop runs only when a caller invokes the getter (spread and
            // JSON.stringify materialize through [[Get]] as before). Every property reports
            // configurable, as the proxy invariants require of properties absent from the
            // hidden target.
            if (root.read(target) === undefined) return undefined;
            return {
                get: () => __smudgy_read_hop(root, target),
                enumerable: true,
                configurable: true,
            };
        },
        defineProperty(_t, _key, _desc) {
            if (root.write === undefined) __smudgy_deny_write(root);
            __smudgy_live_view_reshape();
        },
        preventExtensions() {
            if (root.write === undefined) __smudgy_deny_write(root);
            __smudgy_live_view_reshape();
        },
    });
}

/** Build a widget-binding token for `(spec, root [ . path ])` (plan 7). The host mints the
 *  id (deduped per bound path) and updates the shared value cell at every store flush; the
 *  token itself is plain frozen data, so it crosses into widget props like any value.
 *  `fallback` rides pre-serialized (it crosses to the widget layer as JSON); `format` is a
 *  display template applied when the binding renders as text. */
function __smudgy_make_binding(
    spec: string,
    root: string,
    path?: string,
    opts?: { fallback?: unknown; format?: string },
): Binding<any> {
    const target =
        path === undefined || path === null || path === ""
            ? root
            : __smudgy_join_path(root, String(path));
    const store = (globalThis as any).__smudgy_store;
    const token: { __smudgyStoreBinding: number; fallback?: string; format?: string } = {
        __smudgyStoreBinding: store.bind(spec, target),
    };
    if (opts && opts.fallback !== undefined) token.fallback = JSON.stringify(opts.fallback);
    if (opts && typeof opts.format === "string") token.format = opts.format;
    return Object.freeze(token);
}

/** Deep-freeze a store snapshot so consumer-side mutation throws (strict mode) instead of
 *  silently mutating a dead copy. Snapshots are fresh JSON.parse trees, so freezing cannot
 *  reach shared state. */
function __smudgy_freeze_snapshot<T>(value: T): T {
    if (value !== null && typeof value === "object") {
        for (const key of Object.keys(value as object)) {
            __smudgy_freeze_snapshot((value as Record<string, unknown>)[key]);
        }
        Object.freeze(value);
    }
    return value;
}

/** The canonical event-registry name for `(producer spec, handle name)`: platform producers
 *  use their `sys:`/`map:` prefixes; user/package producers use the stamped `#` form. */
function __smudgy_canonical_event(spec: string, name: string): string {
    // Platform producers key their events `producer:name` -- the form the host's
    // `host_emit` registers and emits ("sys:receive", "gmcp:ready"); package events use
    // the meatball form.
    return spec === "sys" || spec === "map" || spec === "gmcp"
        ? `${spec}:${name}`
        : `${spec}#${name}`;
}

/** The store producer spec for a creator descriptor: packages publish under their own
 *  subtree; user scripts and local modules share the `user` producer. */
function __smudgy_producer_spec(creator: { kind: string; owner?: string; name?: string }): string {
    return creator.kind === "package" ? `smudgy://${creator.owner}/${creator.name}` : "user";
}

// ---- Interned interop identity ids (docs/interop-pre-gmcp-plan.md 3) ----------------
// The per-call constants of the interop ops -- the creator descriptor, producer roots,
// event stamps, the home-gate verdict -- resolve to per-isolate u32 ids once at handle/API
// construction; the per-call ops take the ids. The host table lives in this isolate's
// OpState and these closures live in the same isolate, so an engine rebuild destroys both
// sides atomically -- an id is never stale and carries no generation nonce.

const __smudgy_creator_ids = new Map<string, number>();

/** Resolve (memoized per creator JSON, per isolate) the interned creator id. The host
 *  parse is strict, so a malformed creator fails loudly HERE -- at construction, on every
 *  copy -- rather than at some later call. */
function __smudgy_creator_id(creatorJson: string): number {
    let id = __smudgy_creator_ids.get(creatorJson);
    if (id === undefined) {
        id = op_smudgy_interop_resolve_creator(creatorJson) as number;
        __smudgy_creator_ids.set(creatorJson, id);
    }
    return id;
}

const __smudgy_consumer_root_ids = new Map<string, number>();

/** Resolve (memoized) the interned consumer root id for (producer spec, root path).
 *  Called lazily from consumer reads, not at handle construction: a spec the store cannot
 *  address (an event-only platform name like "sys") must fail at the read -- where it always
 *  failed -- not at scheme import time. */
function __smudgy_consumer_root_id(spec: string, rootPath: string): number {
    // U+001F (unit separator) cannot occur in a producer spec, so the key is unambiguous.
    const key = spec + "\u001f" + rootPath;
    let id = __smudgy_consumer_root_ids.get(key);
    if (id === undefined) {
        id = op_smudgy_interop_resolve_consumer_root(spec, rootPath) as number;
        __smudgy_consumer_root_ids.set(key, id);
    }
    return id;
}

// Duplicate handle names within one producer are a boot diagnostic (plan 4 naming rules):
// the name string is the identity, so a second `state('vitals')` is a second writer to the
// same subtree -- almost always a copy/paste bug. Keyed per (producer, kind, folded name);
// the registry lives for the isolate's life, matching handle lifetime.
const __smudgy_declared_handles = new Set<string>();
function __smudgy_note_handle(creatorId: number, spec: string, kind: string, name: string): void {
    // Runtime-confirm the handle in the host catalogue (plan 10 tier 1) -- also how
    // dynamically-created handles surface there. Informational: presence grants nothing.
    op_smudgy_interop_declare(creatorId, kind, name);
    const key = `${spec} ${kind} ${__smudgy_fold_name(name)}`;
    if (__smudgy_declared_handles.has(key)) {
        echo(
            `smudgy: duplicate interop ${kind} handle name ${JSON.stringify(name)} in ${spec} -- the name string is the handle's identity, so both handles address the same ${kind}`,
        );
    } else {
        __smudgy_declared_handles.add(key);
    }
}

/** Two-arg `set(path, value)` requires a non-empty path. An empty (or blank) path resolves
 *  to the handle root, so accepting it would silently replace the WHOLE subtree -- exactly
 *  what a dynamically computed path that came up "" (an unset field, a failed lookup coerced
 *  by String()) must not do. Whole-subtree replacement is expressed deliberately: single-arg
 *  `set(value)` or assigning `.value`. */
function __smudgy_require_set_path(path: string): string {
    if (path.trim() === "") {
        throw new TypeError(
            "set(path, value) requires a non-empty path -- to replace the whole subtree, call set(value) or assign .value",
        );
    }
    return path;
}

function __smudgy_make_state_producer<T>(creatorId: number, spec: string, name: string): StateHandle<T> {
    const root = __smudgy_name_as_path(name);
    const store = (globalThis as any).__smudgy_store;
    // The (producer, root path) pair is interned once here; every read/write below crosses
    // as (rootId, subpath) with the subpath relative to the handle's own subtree.
    const rootId = op_smudgy_interop_resolve_producer_root(creatorId, root) as number;
    const head = __smudgy_root_ref(rootId, true);
    // The producer's own previous view refuses writes with the snapshot-base teaching
    // error, not the consumer-seat one: this author holds the producer seat already.
    const previous = __smudgy_previous_root_ref(() => rootId, __smudgy_previous_view_write);
    return Object.freeze({
        get value(): T {
            return __smudgy_make_value_proxy(head, "") as T;
        },
        set value(v: T) {
            store.setAt(rootId, "", v);
        },
        // The previous view resolves its root hop like any other (plan 5): an object
        // generation hands back a read-only per-hop proxy, a scalar/array generation the
        // frozen value, no retained generation yet `undefined`.
        get previousValue(): Readonly<T> | undefined {
            return __smudgy_read_hop(previous, "") as Readonly<T> | undefined;
        },
        set(pathOrValue: unknown, value?: unknown): void {
            if (arguments.length >= 2) {
                store.setAt(rootId, __smudgy_require_set_path(String(pathOrValue)), value);
            } else {
                store.setAt(rootId, "", pathOrValue);
            }
        },
        bind: (path?: string, opts?: { fallback?: unknown; format?: string }) =>
            __smudgy_make_binding(spec, root, path, opts),
    }) as StateHandle<T>;
}

function __smudgy_make_event_producer<T>(creatorId: number, name: string): EventHandle<T> {
    // The stamp, fold, and home verdict are interned once here; each emit crosses only the
    // event id and the payload.
    const eventId = op_smudgy_interop_resolve_event(creatorId, name) as number;
    return Object.freeze({
        emit: (payload: T): void =>
            op_smudgy_emit(eventId, JSON.stringify(payload ?? null)),
    });
}

/** Register a procedure's implementation at construction (interop.md 6): receipt is the
 *  producer's seat, home-gated in the op layer like `set`/`emit`. The implementation's
 *  return VALUE is ignored until `.call` (the correlated-reply ask) ships -- but a rejecting
 *  async implementation is caught and logged with the procedure's name, so an async impl's
 *  failure is attributed instead of surfacing as an anonymous unhandled rejection. */
function __smudgy_register_procedure_impl(
    creatorId: number,
    name: string,
    impl: (args: any, sender: string) => unknown,
): void {
    op_smudgy_procedure_on(
        creatorId,
        name,
        (m: { payload: string; sender: string }) => {
            let args: any = null;
            try { args = JSON.parse(m.payload); } catch { args = null; }
            const result = impl(args, m.sender);
            if (result !== null && typeof result === "object" && typeof (result as any).then === "function") {
                (result as Promise<unknown>).then(undefined, (e: unknown) => {
                    console.error(`smudgy: procedure ${JSON.stringify(name)} implementation rejected:`, e);
                });
            }
        },
    );
}

function __smudgy_make_procedure_consumer<A>(spec: string, name: string): ProcedureConsumer<A> {
    // The target root resolves lazily (memoized) at the first post, so an unaddressable
    // spec fails at the post -- where it always failed -- not at scheme import time.
    return Object.freeze({
        post: (args: A): void =>
            op_smudgy_procedure_post(
                __smudgy_consumer_root_id(spec, ""),
                name,
                JSON.stringify(args ?? null),
            ),
    });
}

/** Handle constructors receive their name from the declaration: a top-level
 *  `export const vitals = createState()` is rewritten at transpile time to
 *  `createState("vitals")` (interop.md 4). A call that reaches the runtime without a name is
 *  dynamic creation (a nested scope, a computed callee) -- there is no binding to infer from,
 *  so the author must pass the name, and this guard teaches exactly that. */
function __smudgy_require_handle_name(ctor: string, name: unknown): string {
    if (typeof name !== "string" || name.length === 0) {
        throw new TypeError(
            `${ctor}() could not infer a handle name here -- a top-level \`export const myHandle = ${ctor}(...)\` names itself after the const; anywhere else, pass the name explicitly: ${ctor}("myHandle", ...)`,
        );
    }
    return name;
}

/** Consumer verbs take exactly one argument -- the callback. The emitter-reflex call shape
 *  (`on("name", fn)`) would otherwise register the string and ignore the function, failing
 *  only when the event next fires; the guard converts that to an immediate, specific error. */
function __smudgy_require_callback(verb: string, kind: string, name: string, handler: unknown): void {
    if (typeof handler !== "function") {
        throw new TypeError(
            `${verb}() expects a callback function (got ${typeof handler}); this is already the ${JSON.stringify(name)} ${kind}, so pass only the callback`,
        );
    }
}

function __smudgy_make_state_consumer<T>(spec: string, name: string): StateConsumer<T> {
    const root = __smudgy_name_as_path(name);
    const store = (globalThis as any).__smudgy_store;
    // A read-only, lazily-resolved root reference (no writer): the same per-hop proxy as
    // the producer's `.value`, with mutation traps throwing -- fine-grained reading is not
    // publishing authority, so the seat split (interop.md 4c) is preserved.
    const head = __smudgy_lazy_consumer_root_ref(spec, root);
    const rootId = () => __smudgy_consumer_root_id(spec, root);
    // Both watch cadences take an optional leading path scoping the subscription to a
    // subpath of the handle's subtree (interop.md 2) -- the same trie that scopes bindings,
    // so an ancestor write into the scoped path still fires. Delivered onWrite paths stay
    // handle-relative regardless of scope.
    const scoped = (verb: string, pathOrHandler: unknown, maybeHandler: unknown) => {
        if (typeof pathOrHandler === "string") {
            __smudgy_require_callback(verb, "state", name, maybeHandler);
            return { target: __smudgy_join_path(root, pathOrHandler), handler: maybeHandler as Function };
        }
        __smudgy_require_callback(verb, "state", name, pathOrHandler);
        return { target: root, handler: pathOrHandler as Function };
    };
    const previous = __smudgy_previous_root_ref(rootId);
    return Object.freeze({
        // The root resolves like any other hop, so `.value` is honest about non-object
        // roots: an object root hands back the live proxy, a scalar or array root the
        // frozen materialized value, an absent root `undefined` -- a keyed view over a
        // number (truthy, un-indexable, un-iterable) would betray the `Readonly<T>` claim.
        get value(): Readonly<T> | undefined {
            return __smudgy_read_hop(head, "") as Readonly<T> | undefined;
        },
        // Same root-hop honesty over the producer's previous generation (plan 5); the
        // consumer base id keeps resolving lazily, so an unaddressable spec still fails
        // at the read, never at scheme import time.
        get previousValue(): Readonly<T> | undefined {
            return __smudgy_read_hop(previous, "") as Readonly<T> | undefined;
        },
        watch(pathOrHandler: unknown, maybeHandler?: unknown): EventSubscription {
            const { target, handler } = scoped("watch", pathOrHandler, maybeHandler);
            const sub = store.watch(spec, target, (snapshot: unknown) =>
                handler(__smudgy_freeze_snapshot(snapshot)),
            );
            return { off: () => sub.unwatch() };
        },
        onWrite(pathOrHandler: unknown, maybeHandler?: unknown): EventSubscription {
            const { target, handler } = scoped("onWrite", pathOrHandler, maybeHandler);
            const sub = store.onWrite(spec, target, (path: string, snapshot: unknown) =>
                handler(__smudgy_relative_path(root, path), __smudgy_freeze_snapshot(snapshot)),
            );
            return { off: () => sub.unwatch() };
        },
        bind: (path?: string, opts?: { fallback?: unknown; format?: string }) =>
            __smudgy_make_binding(spec, root, path, opts),
    }) as unknown as StateConsumer<T>;
}

function __smudgy_make_event_consumer<T>(canonical: string, name: string): EventConsumer<T> {
    // Payloads are delivered deep-frozen (occurrences are facts, not shared mutables) so
    // the Readonly consumer types tell the truth at runtime too (interop.md 4/11).
    const parse_frozen = (raw: string): any => {
        let payload: any = null;
        try { payload = JSON.parse(raw); } catch { payload = null; }
        return __smudgy_freeze_snapshot(payload);
    };
    const subscribe_once = (handler: (payload: Readonly<T>) => void): EventSubscription => {
        let fired = false;
        let id = -1;
        const off = () => op_smudgy_off(canonical, id);
        id = op_smudgy_on(canonical, (m: { event: string; payload: string }) => {
            if (fired) return;
            fired = true;
            off();
            handler(parse_frozen(m.payload));
        });
        return { off };
    };
    return Object.freeze({
        on(handler: (payload: Readonly<T>) => void): EventSubscription {
            __smudgy_require_callback("on", "event", name, handler);
            const id = op_smudgy_on(canonical, (m: { event: string; payload: string }) => {
                handler(parse_frozen(m.payload));
            });
            return { off: () => op_smudgy_off(canonical, id) };
        },
        // Argless once() resolves on the next occurrence -- for flows and startup
        // choreography, not for reading fast-moving game state in trigger handlers
        // (the interop.md 2 TOCTOU caveat). The promise branch keys on ARITY, not an
        // undefined check: `once(maybeMissingCallback)` where the variable is undefined is
        // the mistake the callback guard exists to catch, and must stay an immediate error
        // rather than silently becoming an ignored promise.
        once(handler?: (payload: Readonly<T>) => void): EventSubscription | Promise<Readonly<T>> {
            if (arguments.length === 0) {
                return new Promise<Readonly<T>>((resolve) => {
                    subscribe_once(resolve);
                });
            }
            __smudgy_require_callback("once", "event", name, handler);
            return subscribe_once(handler as (payload: Readonly<T>) => void);
        },
    }) as unknown as EventConsumer<T>;
}

/**
 * Build the per-creator scripting facade. Both delivery paths consume this: the synthesized
 * `smudgy:core` virtual module (one instance per importing module/package, with `__creator`
 * baked in) re-exports its members; inline alias/trigger scripts run inside
 * `with (globalThis.__smudgy_user_api) { ... }`.
 *
 * The live-state members are getters/functions, so they stay live through `with` and the
 * default export: `mapper` is a getter (the smudgy.js facade is built before mapper.ts
 * installs `globalThis.mapper`, so the lookup MUST be deferred), `getSessions()`/`getProfile()`
 * are functions (the session set + profile fields change). `session`/`id`
 * derive from a constant session id, so a snapshot is correct -- they are getters only so the
 * inline `with` object also resolves them. Only the `create*`/registry members carry
 * provenance; everything else is the shared, provenance-free set.
 *
 * None of this is installed on `globalThis`, keeping the global clean for imported jsr/npm code.
 */
function __smudgy_make_api(creator: { kind: string }) {
    // One strict host parse per API construction: the interned creator id carries the
    // origin, producer key, and home verdict that every creator-taking op addresses by id
    // (docs/interop-pre-gmcp-plan.md 3).
    const creatorId = __smudgy_creator_id(JSON.stringify(creator));
    const warnNameFirst = __smudgy_make_deprecation_warner(creator);
    const registries = __smudgy_make_registries(creatorId);
    const timerHotkey = __smudgy_make_timer_hotkey_api(creatorId, warnNameFirst);

    const api = {
        // Live/deferred accessors as object-literal getters: kept getters (not values) so the
        // inline `with` object resolves `session`/`mapper`/`id` live, and so `mapper` defers its
        // lookup until after mapper.ts installs `globalThis.mapper`. The session id is constant
        // for a runtime's life, so `session`/``id` are stable in value.
        get session(): Session {
            return getCurrentSession();
        },
        get id(): number {
            return getCurrentSession().id;
        },
        get mapper(): any {
            return (globalThis as any).mapper;
        },
        // Provenance-free value members (stable identity, safe to destructure as named exports).
        send,
        sendRaw,
        echo,
        style,
        link,
        reload,
        capture,
        line,
        buffer,
        vars,
        byName,
        // Live-state accessors as functions (read fresh on each call).
        getSessions,
        getProfile,
        getSettings,
        getDataDir,
        // Persisted, UI-visible user automations (create/edit the saved aliases/triggers/hotkeys).
        userAutomations,
        // Creator-bound creation surface. The rest-args wrappers feed the
        // DEPRECATED-NAME-FIRST shim (remove in 0.5); an old-form call is one with four
        // arguments, or whose third argument sits in the old script slot (string or
        // function) -- a new-form third argument can only be an options object. The casts
        // keep the published pattern-first signatures on the facade.
        createAlias: ((...args: any[]) => {
            if (args.length >= 4 || typeof args[2] === "string" || typeof args[2] === "function") {
                warnNameFirst("createAlias", 'createAlias(patterns, script, { name: "..." })');
                return createAlias(
                    creatorId,
                    args[1],
                    args[2],
                    __smudgy_adopt_positional_name<AliasOptions>("createAlias", args[0], args[3]),
                );
            }
            return createAlias(creatorId, args[0], args[1], args[2]);
        }) as (patterns: Pattern | Pattern[], script: AutomationScript, options?: AliasOptions) => Alias,
        createTrigger: ((...args: any[]) => {
            if (args.length >= 4 || typeof args[2] === "string" || typeof args[2] === "function") {
                warnNameFirst("createTrigger", 'createTrigger(patterns, script, { name: "..." })');
                return createTrigger(
                    creatorId,
                    args[1],
                    args[2],
                    __smudgy_adopt_positional_name<TriggerOptions>("createTrigger", args[0], args[3]),
                );
            }
            return createTrigger(creatorId, args[0], args[1], args[2]);
        }) as (patterns: Pattern | TriggerPatterns, script: AutomationScript, options?: TriggerOptions) => Trigger,
        createTriggers: (triggers: Record<string, TriggerDef>) => createTriggers(creatorId, triggers),
        createTimer: timerHotkey.createTimer,
        createHotkey: timerHotkey.createHotkey,
        triggers: registries.triggers,
        aliases: registries.aliases,
        timers: timerHotkey.timers,
        hotkeys: timerHotkey.hotkeys,
        // Interop handle constructors (docs/interop.md 4/11). Producer handles
        // are creator-bound: what you declare, you publish/emit as -- and the host gates
        // writes on the creator's origin + home isolate, so a code-imported copy reads but
        // cannot write. Consumers never call these; they import from smudgy:state/... and
        // smudgy:events/... (or use events.lookup below).
        createState: <T = unknown>(name?: string): StateHandle<T> => {
            const n = __smudgy_require_handle_name("createState", name);
            const spec = __smudgy_producer_spec(creator as any);
            __smudgy_note_handle(creatorId, spec, "state", n);
            return __smudgy_make_state_producer<T>(creatorId, spec, n);
        },
        createEvent: <T = unknown>(name?: string): EventHandle<T> => {
            const n = __smudgy_require_handle_name("createEvent", name);
            const spec = __smudgy_producer_spec(creator as any);
            __smudgy_note_handle(creatorId, spec, "event", n);
            return __smudgy_make_event_producer<T>(creatorId, n);
        },
        // Procedures (interop.md 6): a directed ask of this package. The implementation is
        // the constructor argument -- registered at construction, home-gated in the op layer
        // -- and the handle carries no verbs. Consumers import from smudgy:procedures/...
        // and `.post()` (fire-and-forget today; `.call` is the deferred correlated ask).
        createProcedure: <A = unknown, R = void>(
            nameOrImpl?: string | ((args: A, sender: string) => R | Promise<R>),
            maybeImpl?: (args: A, sender: string) => R | Promise<R>,
        ): ProcedureHandle<A, R> => {
            // Two author-facing shapes, like createDerived: `createProcedure(impl)` (the
            // name arrives by transpile-time injection) and the explicit
            // `createProcedure(name, impl)` for dynamic creation.
            const name = __smudgy_require_handle_name("createProcedure", nameOrImpl);
            const impl = maybeImpl;
            if (typeof impl !== "function") {
                throw new TypeError(
                    "createProcedure() expects the implementation function: createProcedure((args, sender) => { ... })",
                );
            }
            const spec = __smudgy_producer_spec(creator as any);
            __smudgy_note_handle(creatorId, spec, "procedure", name);
            __smudgy_register_procedure_impl(creatorId, name, impl);
            return Object.freeze({}) as ProcedureHandle<A, R>;
        },
        // Consumer-side derivation (plan 4b): watch a source, compute, publish the result
        // into THIS creator's own subtree under `name` -- hence itself bindable, watchable,
        // catalogued. The computation is skipped while the source reads undefined (nothing
        // published / producer absent); a computed undefined publishes nothing.
        createDerived: <U = unknown>(
            // Two author-facing shapes: `createDerived(source, compute)` (the name arrives by
            // transpile-time injection) and the explicit `createDerived(name, source, compute)`
            // for dynamic creation. The name-first form is the only one the runtime accepts;
            // a source-first call reaching here un-rewritten is dynamic creation without a
            // name, and the name guard teaches that.
            nameOrSource?: string | { value?: unknown; watch(handler: (snapshot: unknown) => void): EventSubscription },
            sourceOrCompute?: { value?: unknown; watch(handler: (snapshot: unknown) => void): EventSubscription } | ((snapshot: any) => U),
            maybeCompute?: (snapshot: any) => U,
        ): DerivedHandle<U> => {
            const name = __smudgy_require_handle_name("createDerived", nameOrSource);
            const source = sourceOrCompute as { value?: unknown; watch(handler: (snapshot: unknown) => void): EventSubscription };
            const compute = maybeCompute as (snapshot: any) => U;
            if (typeof compute !== "function") {
                throw new TypeError("createDerived() expects a compute function");
            }
            if (source === null || typeof source !== "object" || typeof (source as any).watch !== "function") {
                throw new TypeError(
                    "createDerived() expects a state consumer handle (something with .watch) as its source",
                );
            }
            const spec = __smudgy_producer_spec(creator as any);
            __smudgy_note_handle(creatorId, spec, "state", String(name));
            const out = __smudgy_make_state_producer<U>(creatorId, spec, String(name));
            // A read-only view over the published output (the resolve dedups against the
            // producer handle's own root id), so the derived handle exposes no write-through.
            const outView = __smudgy_root_ref(
                op_smudgy_interop_resolve_producer_root(
                    creatorId,
                    __smudgy_name_as_path(String(name)),
                ) as number,
                false,
            );
            const publish = (snapshot: unknown): void => {
                if (snapshot === undefined) return;
                const value = compute(snapshot);
                if (value !== undefined) out.set(value);
            };
            // Seed from the source's live view, materialized (and frozen) so the compute
            // sees the same plain-data shape watch deliveries carry, never a live proxy.
            const seed = source.value;
            publish(
                seed !== null && typeof seed === "object"
                    ? __smudgy_freeze_snapshot(JSON.parse(JSON.stringify(seed)))
                    : seed,
            );
            const sub = source.watch(publish);
            return Object.freeze({
                get value(): Readonly<U> | undefined {
                    return __smudgy_read_hop(outView, "") as Readonly<U> | undefined;
                },
                bind: out.bind,
                off: () => sub.off(),
            });
        },
        // The one dynamic escape hatch (plan 11): generic tooling that knows a producer and
        // event name only at runtime gets an untyped consumer handle. `producer` is
        // "smudgy://owner/name", "user", or a platform name ("sys"/"map").
        events: Object.freeze({
            lookup: (producer: string, name: string): EventConsumer<unknown> =>
                __smudgy_make_event_consumer(
                    __smudgy_canonical_event(
                        __smudgy_fold_name(String(producer)),
                        __smudgy_fold_name(String(name)),
                    ),
                    String(name),
                ),
        }),
        // GMCP protocol control (docs/gmcp-plan.md 3.4/6.3): `enabled` reads the
        // negotiated state; `onReady(cb)` calls cb synchronously when already enabled,
        // else once on the next gmcp ready event -- so a late-loading package doesn't
        // race the handshake. The outbound verbs (send / enableModule / disableModule /
        // mergeKeys) are gated by the gmcp:send capability at the op layer. Data
        // consumption is the state handle (`import gmcp from "smudgy:state/gmcp"`) and
        // the ready/closed events live at smudgy:events/gmcp.
        gmcp: Object.freeze({
            get enabled(): boolean {
                return op_smudgy_gmcp_enabled() as boolean;
            },
            onReady(cb: () => void): void {
                if (typeof cb !== "function") {
                    throw new TypeError("gmcp.onReady() expects a callback function");
                }
                if (op_smudgy_gmcp_enabled()) {
                    cb();
                    return;
                }
                __smudgy_make_event_consumer(
                    __smudgy_canonical_event("gmcp", "ready"),
                    "ready",
                ).once(() => cb());
            },
            send(name: string, data?: unknown): void {
                // `undefined` means no data part (Char.Items.Inv); anything else --
                // an explicit null included -- serializes, since JSON is the wire form.
                op_smudgy_gmcp_send(
                    String(name),
                    data === undefined ? null : JSON.stringify(data) ?? "null",
                );
            },
            enableModule(name: string, version?: number): void {
                const v =
                    typeof version === "number" && Number.isInteger(version) && version > 0
                        ? version
                        : 1;
                op_smudgy_gmcp_enable_module(String(name), v);
            },
            disableModule(name: string): void {
                op_smudgy_gmcp_disable_module(String(name));
            },
            mergeKeys(...names: string[]): void {
                op_smudgy_gmcp_merge_keys(names.map((n) => String(n)));
            },
        }),
    };

    return api;
}

/**
 * The shape of the per-creator api object `__smudgy_make_api` builds. Type-only (erased at
 * transpile). The drift-guard test
 * (`models/script_typings.rs::smudgy_ts_impl_conforms_to_contract`) checks this is assignable
 * to the published `SmudgyApi` contract in `smudgy-core.d.ts`, so the impl cannot silently
 * drift from the declarations authors see.
 */
export type SmudgyCoreApi = ReturnType<typeof __smudgy_make_api>;

Object.defineProperty(globalThis, "__smudgy_create_api", { value: __smudgy_make_api });

// The api bound to the user namespace, injected into inline alias/trigger scripts via
// `with (globalThis.__smudgy_user_api) { ... }` (see core's `ScriptEngine::add_script`).
Object.defineProperty(globalThis, "__smudgy_user_api", {
    value: __smudgy_make_api({ kind: "user" }),
});

// Host hook for the synthesized `smudgy:params` virtual module: read a package's param value
// by its specifier + key. Scripts `import { get } from "smudgy:params"`, whose `get` is bound
// to the importing package's specifier and bridges here.
Object.defineProperty(globalThis, "__smudgy_param_get", {
    value: (spec: string, key: string) => op_smudgy_param_get(spec, key) ?? undefined,
});

// Host hooks for the session store (docs/interop.md): the internal seam the state
// handles and the smudgy:state consumer scheme build on, like __smudgy_param_get above. Not part
// of the public smudgy:core contract. Writes are creator-attributed (the host gates them on the
// creator's origin and home isolate); values cross by value as JSON. `get` distinguishes an
// absent path (undefined) from a stored null; `watch` delivers the watched path's final state
// once per writing turn, on a later pump.
//
// Addressing is by interned root id (docs/interop-pre-gmcp-plan.md 3): the *At members take
// a root id + a root-relative subpath, which is what the handles carry. The creator/spec
// polymorphic members resolve (memoized) to ids internally, so both addressings observe
// identical identity semantics through the same interned table.
Object.defineProperty(globalThis, "__smudgy_store", {
    value: {
        set: (creator: unknown, path: string, value: unknown): void => {
            const creatorJson =
                typeof creator === "string" ? creator : JSON.stringify(creator ?? { kind: "user" });
            op_smudgy_store_set(__smudgy_creator_id(creatorJson), String(path), JSON.stringify(value ?? null));
        },
        setAt: (rootId: number, subpath: string, value: unknown): void => {
            op_smudgy_store_set(rootId, String(subpath), JSON.stringify(value ?? null));
        },
        get: (producer: string, path: string): unknown => {
            const snapshot = op_smudgy_store_get(
                __smudgy_consumer_root_id(String(producer), ""),
                String(path),
            );
            return snapshot === null || snapshot === undefined ? undefined : JSON.parse(snapshot);
        },
        getAt: (rootId: number, subpath: string): unknown => {
            const snapshot = op_smudgy_store_get(rootId, String(subpath));
            return snapshot === null || snapshot === undefined ? undefined : JSON.parse(snapshot);
        },
        // The leaf-aware reads (docs/interop-pre-gmcp-plan.md 2), same read-your-writes
        // visibility as `get`: `getTagged` answers kind + payload at a path -- "o" objects
        // cross with NO payload, "a"/"v" carry the JSON; a stored null is "vnull" -- and
        // `keys` lists an object's own keys (first-published casing, publish order).
        // Absent (and, for `keys`, non-object) maps to undefined.
        getTagged: (producer: string, path: string): string | undefined => {
            const tagged = op_smudgy_store_get_tagged(
                __smudgy_consumer_root_id(String(producer), ""),
                String(path),
            );
            return tagged === null || tagged === undefined ? undefined : tagged;
        },
        getTaggedAt: (rootId: number, subpath: string): string | undefined => {
            const tagged = op_smudgy_store_get_tagged(rootId, String(subpath));
            return tagged === null || tagged === undefined ? undefined : tagged;
        },
        keys: (producer: string, path: string): string[] | undefined => {
            const keys = op_smudgy_store_keys(
                __smudgy_consumer_root_id(String(producer), ""),
                String(path),
            );
            return keys === null || keys === undefined ? undefined : JSON.parse(keys);
        },
        keysAt: (rootId: number, subpath: string): string[] | undefined => {
            const keys = op_smudgy_store_keys(rootId, String(subpath));
            return keys === null || keys === undefined ? undefined : JSON.parse(keys);
        },
        // The previous-generation reads (plan 5): the same three shapes over the state
        // before the newest write batch the reading isolate can observe. A separate op
        // family rather than a view arm inside the head ops, so the head reads above stay
        // single-call on the hot proxy path; __smudgy_previous_root_ref routes here.
        previousGetAt: (rootId: number, subpath: string): unknown => {
            const snapshot = op_smudgy_store_previous_get(rootId, String(subpath));
            return snapshot === null || snapshot === undefined ? undefined : JSON.parse(snapshot);
        },
        previousGetTaggedAt: (rootId: number, subpath: string): string | undefined => {
            const tagged = op_smudgy_store_previous_get_tagged(rootId, String(subpath));
            return tagged === null || tagged === undefined ? undefined : tagged;
        },
        previousKeysAt: (rootId: number, subpath: string): string[] | undefined => {
            const keys = op_smudgy_store_previous_keys(rootId, String(subpath));
            return keys === null || keys === undefined ? undefined : JSON.parse(keys);
        },
        watch: (producer: string, path: string, handler: (snapshot: unknown) => void) => {
            const token = op_smudgy_store_watch(
                String(producer),
                String(path),
                (m: { snapshot: string }) => {
                    let snapshot: unknown = null;
                    try { snapshot = JSON.parse(m.snapshot); } catch { snapshot = null; }
                    handler(snapshot);
                },
                false,
            );
            return { unwatch: () => op_smudgy_store_unwatch(token) };
        },
        // The per-write cadence (plan 2, D8): one delivery per set-at-path in write order,
        // value-identical writes included, with the WRITTEN path (producer-relative) and the
        // value that write published.
        onWrite: (producer: string, path: string, handler: (path: string, snapshot: unknown) => void) => {
            const token = op_smudgy_store_watch(
                String(producer),
                String(path),
                (m: { path: string; snapshot: string }) => {
                    let snapshot: unknown = null;
                    try { snapshot = JSON.parse(m.snapshot); } catch { snapshot = null; }
                    handler(m.path, snapshot);
                },
                true,
            );
            return { unwatch: () => op_smudgy_store_unwatch(token) };
        },
        // Mint a widget-binding id on (producer, path) -- the host-side watcher `bind`
        // tokens carry (plan 7). Read-side like `watch`; deduped per bound path.
        bind: (producer: string, path: string): number =>
            op_smudgy_store_bind(String(producer), String(path)),
    },
});

// Host hook for the synthesized smudgy:state/, smudgy:events/, and smudgy:procedures/ scheme
// modules: given a producer spec ("smudgy://owner/name" or a platform name like "sys"),
// returns the per-kind consumer-handle factories the stubs export from. Consumer handles are addressers over
// (producer, name) -- they never touch the producer's live objects, so importing one never
// evaluates (or waits on) the producer. Not part of the public smudgy:core contract.
Object.defineProperty(globalThis, "__smudgy_interop_consumer", {
    value: (spec: string) => {
        const folded = __smudgy_fold_name(String(spec));
        return Object.freeze({
            state: (name: string): StateConsumer<unknown> =>
                __smudgy_make_state_consumer(folded, String(name)),
            event: (name: string): EventConsumer<unknown> =>
                __smudgy_make_event_consumer(__smudgy_canonical_event(folded, String(name)), String(name)),
            procedure: (name: string): ProcedureConsumer<unknown> =>
                __smudgy_make_procedure_consumer(folded, String(name)),
        });
    },
});
