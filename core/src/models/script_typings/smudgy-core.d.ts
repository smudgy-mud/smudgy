// =============================================================================
//  smudgy:core — TypeScript declarations  (GENERATED — DO NOT EDIT)
// =============================================================================
//  smudgy writes and overwrites this file every time a session starts. It teaches
//  VS Code (and any TypeScript-aware editor) about the `smudgy:core` module so the
//  scripts in the parent `modules/` folder get autocomplete and type-checking.
//
//  Edits here are lost on the next launch — change your scripts (and your own
//  `../tsconfig.json`, which smudgy creates once and never overwrites) instead.
//
//  This file is the AUTHOR-FACING CONTRACT. The runtime implementation lives in
//  `core/src/session/runtime/js/smudgy.ts`; a drift-guard test
//  (`models/script_typings.rs::smudgy_ts_impl_conforms_to_contract`) compiles the
//  impl against this contract so the two cannot silently diverge.
//
//  The `mapper` type (`Mapper`/`Area`/`Room`/`Exit`/...) is declared in the sibling
//  `smudgy-mapper.d.ts` as global ambient types; this module references them.
//
//  Shared state and events flow through typed handles created by
//  `createState()` / `createEvent()` and consumed via the `smudgy:state/...`
//  / `smudgy:events/...` modules (see below).
// =============================================================================

declare module "smudgy:core" {
  // ---- Interop handles: shared state & events -------------------------------

  /**
   * A subscription returned by an event handle's `on`/`once` or a state
   * handle's `watch`. Call {@link EventSubscription.off} to stop listening.
   *
   * ```ts
   * import { connect } from "smudgy:events/sys";
   * const sub = connect.on(() => console.log("connected"));
   * // later, when you no longer care:
   * sub.off();
   * ```
   */
  export interface EventSubscription {
    /**
     * Cancels this subscription; the handler stops receiving deliveries.
     * Calling it again has no effect. Subscriptions are also dropped
     * automatically when the script reloads.
     */
    off(): void;
  }

  /**
   * A live connection from a shared-state path to a widget property, created
   * by a state handle's {@link StateHandle.bind | bind}. It is accepted
   * wherever a widget prop takes a value:
   *
   * ```tsx
   * import { ProgressBar, Text } from "smudgy:widgets";
   * import { vitals } from "smudgy:state/kapusniak/arctic-prompt";
   *
   * <>
   *   <ProgressBar value={vitals.bind('hp')} max={vitals.bind('maxhp')} />
   *   <Text>HP: {vitals.bind('hp')}</Text>
   * </>
   * ```
   *
   * The widget then tracks the published value on its own. No handler runs
   * and no re-mount happens on updates; the mounted widget repaints.
   */
  export interface Binding<T = unknown> {
    /** @internal the host-issued binding id -- do not access. */
    readonly __smudgyStoreBinding: number;
    /** @internal pre-serialized fallback -- do not access. */
    readonly fallback?: string;
    /** @internal display template -- do not access. */
    readonly format?: string;
    /** @internal phantom value type -- do not access. */
    readonly __smudgyBindingValue?: T;
  }

  /** Options for a state handle's {@link StateHandle.bind | bind}. */
  export interface BindOptions<T = unknown> {
    /**
     * The value the widget shows while the bound path is unpublished or
     * `null` (for example, before the producer's first write).
     */
    fallback?: T;
    /**
     * A display template for text positions: `{}` is replaced by the bound
     * value, so `format: "{}%"` renders `42` as `42%`. Ignored where the
     * binding feeds a non-text prop (a `ProgressBar` value, a size).
     */
    format?: string;
  }

  /**
   * The dotted lookup paths into `T`, for {@link StateHandle.bind | bind}
   * autocompletion (`'hp' | 'maxhp' | 'stats.str' | …`). Paths are lookups,
   * not expressions; nesting is suggested four levels deep, and any plain
   * string is accepted where the shape is unknown.
   */
  export type StatePath<T, Depth extends number = 4> = [Depth] extends [never]
    ? never
    : T extends readonly unknown[]
      ? never
      : T extends object
        ? {
            [K in keyof T & string]:
              | K
              | `${K}.${StatePath<T[K], [never, 0, 1, 2, 3][Depth]> & string}`;
          }[keyof T & string]
        : never;

  /** The value type at a {@link StatePath} into `T`. */
  export type StateAt<T, P extends string> = P extends `${infer K}.${infer Rest}`
    ? K extends keyof T
      ? StateAt<T[K], Rest>
      : unknown
    : P extends keyof T
      ? T[P]
      : unknown;

  /**
   * A shared state owned by the current script or package, created by
   * {@link createState}. Publish with {@link StateHandle.set | set} or by
   * assigning through {@link StateHandle.value | value}.
   *
   * Other scripts and packages get a read-only view by importing from
   * `smudgy:state/<owner>/<package>` (see {@link StateConsumer}).
   */
  export interface StateHandle<T = unknown> {
    /**
     * A live view of the published value. Assigning into it publishes just
     * that entry (`vitals.value.hp = 42`); assigning `value` itself replaces
     * the whole published value.
     *
     * Assigning into an entry that doesn't exist yet throws, like any
     * property chain onto `undefined`. Publishing the containing object
     * first avoids this, as does {@link StateHandle.set | set} with a path,
     * which creates the intermediate objects and is the more direct form
     * for bulk updates.
     *
     * Objects read through the view are fresh proxies each time, not stable
     * references (`v.stats !== v.stats`), so they are unsuitable as map or
     * memoization keys. `{ ...v }` copies one level (nested entries stay
     * live views); `JSON.parse(JSON.stringify(v))` copies the whole shape.
     * `Object.defineProperty` and `Object.freeze` are not supported on the
     * view and throw.
     */
    value: T;
    /**
     * The value as it looked before your latest writes, or `undefined` if
     * nothing had been published before them. Useful for working out what
     * changed.
     *
     * `previousValue` advances whenever you publish, not per state: all of
     * your states publish together, so finishing any update advances
     * `previousValue` for every state you own.
     *
     * Like {@link StateHandle.value | value}, this is a read-only live view
     * that follows your newest writes; a spread or JSON copy is a value
     * that stays put.
     */
    readonly previousValue: Readonly<T> | undefined;
    /**
     * Publishes a value. With one argument, replaces the whole value; with
     * two, replaces just the subtree at `path` (a dot/bracket lookup path
     * such as `"groupies[\"Mr. Foo\"].hp"`). The two-argument form throws on
     * an empty path.
     *
     * Values are serialized as JSON: properties whose value is `undefined`
     * are dropped, and `NaN` becomes `null`.
     */
    set(value: T): void;
    set(path: string, value: unknown): void;
    /**
     * Connects this state to a widget property (see {@link Binding}). With
     * no path the whole value is bound; with a path, just that entry:
     * `vitals.bind('hp')`, `roster.bind('groupies["Mr. Foo"].hp')`. Paths
     * are lookups, not expressions; a computed value becomes bindable once
     * published as a state of its own, for example with
     * {@link createDerived}.
     */
    bind(): Binding<T>;
    bind<P extends StatePath<T> & string>(
      path: P,
      options?: BindOptions<StateAt<T, P>>,
    ): Binding<StateAt<T, P>>;
    bind(path: string, options?: BindOptions): Binding<any>;
  }

  /**
   * An event owned by the current script or package, created by
   * {@link createEvent}. Other scripts and packages subscribe by importing
   * from `smudgy:events/<owner>/<package>` (see {@link EventConsumer}).
   */
  export interface EventHandle<T = unknown> {
    /**
     * Broadcasts a payload to every subscriber. Payloads are serialized as
     * JSON: properties whose value is `undefined` are dropped, and `NaN`
     * becomes `null`. There is no reply channel; a request/response
     * exchange is built from two events, one in each direction.
     */
    emit(payload: T): void;
  }

  /**
   * A read-only view of another package's {@link StateHandle}. What
   * `import { … } from "smudgy:state/<owner>/<pkg>"` gives you.
   *
   * The two subscription verbs differ in cadence. `watch` coalesces each
   * update into one delivery; `onWrite` replays every write:
   *
   * ```ts
   * import { vitals } from "smudgy:state/kapusniak/arctic-prompt";
   * // vitals is currently { hp: 20, maxhp: 100 }; its producer now
   * // writes, within a single update:
   * //   vitals.value.hp = 15;
   * //   vitals.value.hp = 12;
   * //   vitals.value.maxhp = 100;
   *
   * vitals.watch(v => console.log(v?.hp));
   * // one delivery, after the update: v is { hp: 12, maxhp: 100 }
   *
   * vitals.onWrite((path, value) => console.log(path, value));
   * // three deliveries, in write order:
   * //   ("hp", 15), ("hp", 12), ("maxhp", 100)
   *
   * // In either handler, previousValue holds the value from before the
   * // update began:
   * vitals.previousValue;  // { hp: 20, maxhp: 100 }
   * ```
   */
  export interface StateConsumer<T = unknown> {
    /**
     * A live, read-only view of the producer's current value, or
     * `undefined` if the producer hasn't published anything. A producer
     * that isn't installed reads the same way, as `undefined`, not as an
     * error. A published value that isn't an object (a number, a string,
     * an array) reads whole, as a frozen value.
     *
     * Assigning or deleting through the view throws, and so do
     * `Object.defineProperty` and `Object.freeze`.
     *
     * Objects read through the view are fresh proxies each time, not stable
     * references (`v.stats !== v.stats`), so they are unsuitable as map or
     * memoization keys. `{ ...v }` copies one level (nested entries stay
     * live views); `JSON.parse(JSON.stringify(v))` copies the whole shape.
     */
    readonly value: Readonly<T> | undefined;
    /**
     * The producer's value as it looked before its latest writes, or
     * `undefined` if nothing had been published before them. Useful for
     * working out what changed in a {@link StateConsumer.watch | watch}
     * handler.
     *
     * It advances when the producer publishes, not per state: a producer's
     * states publish together, so any update it finishes advances
     * `previousValue` for every state it owns.
     */
    readonly previousValue: Readonly<T> | undefined;
    /**
     * Runs a handler once per writing turn in which the value was written,
     * carrying that turn's final value. Delivery is write-triggered, not
     * change-detected: a turn that rewrites the same value still fires.
     *
     * Pass a path first to watch a single entry: `vitals.watch('hp', hp => …)`.
     * A scoped watcher runs for writes at, under, or enclosing its path, so
     * a whole-value `set()` fires an `'hp'` watcher, while a write to a
     * sibling entry such as `maxhp` does not.
     */
    watch(handler: (snapshot: Readonly<T> | undefined) => void): EventSubscription;
    watch<P extends StatePath<T> & string>(
      path: P,
      handler: (snapshot: Readonly<StateAt<T, P>> | undefined) => void,
    ): EventSubscription;
    watch(path: string, handler: (snapshot: unknown) => void): EventSubscription;
    /**
     * Runs a handler for every write, in write order, including writes that
     * didn't change the value (which {@link StateConsumer.watch | watch}
     * would coalesce into one delivery). The handler receives the written
     * path (relative to this state; `""` for the whole value) and the value
     * that was written. Pass a path first to hear only writes at, above, or
     * below that entry.
     *
     * `onWrite` suits occurrences, where each write is meaningful in
     * itself; `watch` is the simpler, cheaper verb when only the current
     * value matters.
     */
    onWrite(handler: (path: string, snapshot: unknown) => void): EventSubscription;
    onWrite<P extends StatePath<T> & string>(
      path: P,
      handler: (path: string, snapshot: unknown) => void,
    ): EventSubscription;
    onWrite(path: string, handler: (path: string, snapshot: unknown) => void): EventSubscription;
    /**
     * Connects the producer's published state to a widget property. The
     * widget follows the published value on its own, repainting as writes
     * arrive, with no handler in between (see {@link Binding}).
     *
     * ```tsx
     * import { ProgressBar } from "smudgy:widgets";
     * import { vitals } from "smudgy:state/kapusniak/arctic-prompt";
     *
     * <ProgressBar value={vitals.bind('hp', { fallback: 0 })}
     *              max={vitals.bind('maxhp', { fallback: 100 })} />
     * ```
     *
     * While the producer has published nothing (including when it is not
     * installed), the bound path is unpublished and the widget shows the
     * `fallback` value, or nothing when no fallback was given.
     */
    bind(): Binding<T>;
    bind<P extends StatePath<T> & string>(
      path: P,
      options?: BindOptions<StateAt<T, P>>,
    ): Binding<StateAt<T, P>>;
    bind(path: string, options?: BindOptions): Binding<any>;
  }

  /**
   * A subscription surface for another package's {@link EventHandle}. What
   * `import { … } from "smudgy:events/<owner>/<pkg>"` (or
   * `smudgy:events/sys` / `smudgy:events/map`) gives you.
   */
  export interface EventConsumer<T = unknown> {
    /** Runs a handler on every occurrence. Payloads arrive read-only. */
    on(handler: (payload: Readonly<T>) => void): EventSubscription;
    /**
     * Returns a promise that resolves with the next occurrence:
     * `const first = await prompt.once()`. An `await` on it suspends only
     * the awaiting script; incoming lines and triggers are processed
     * normally in the meantime.
     */
    once(): Promise<Readonly<T>>;
    /** Like {@link EventConsumer.on}, but the handler fires at most once. */
    once(handler: (payload: Readonly<T>) => void): EventSubscription;
  }

  /**
   * A procedure implemented by the current script or package, created by
   * {@link createProcedure}. Other scripts and packages call it by
   * importing from `smudgy:procedures/<owner>/<package>` (see
   * {@link ProcedureConsumer}); every call runs this implementation.
   *
   * The handle has no members of its own. The implementation is passed to
   * `createProcedure`, so all there is to do with the handle is export it,
   * which names the procedure and types your callers.
   */
  export interface ProcedureHandle<A = unknown, R = void> {
    /** Type carrier only; no runtime member exists. */
    readonly __smudgyProcedure?: (args: A) => R;
  }

  /**
   * The caller's side of another package's {@link ProcedureHandle}. What
   * `import { … } from "smudgy:procedures/<owner>/<pkg>"` gives you.
   */
  export interface ProcedureConsumer<A = unknown, R = void> {
    /**
     * Sends arguments to the implementer, fire-and-forget: there is no
     * reply or receipt, and posting to a producer that isn't installed does
     * nothing. Arguments are serialized as JSON, like event payloads.
     * Answers, when a procedure has any, come back as state the producer
     * publishes or an event it emits.
     */
    post(args: A): void;
    /** Type carrier only; no runtime member exists. */
    readonly __smudgyProcedure?: (args: A) => R;
  }

  /**
   * Returned by {@link createDerived}: read the computed value, bind it to
   * widgets, and `off()` to stop computing.
   */
  export interface DerivedHandle<U = unknown> {
    /**
     * The most recently computed value, as a read-only live view.
     * `undefined` before the first computation.
     */
    readonly value: Readonly<U> | undefined;
    /** Connects the computed value to a widget property (see {@link Binding}). */
    bind(): Binding<U>;
    bind<P extends StatePath<U> & string>(
      path: P,
      options?: BindOptions<StateAt<U, P>>,
    ): Binding<StateAt<U, P>>;
    bind(path: string, options?: BindOptions): Binding<any>;
    /** Stops recomputing. The last published value remains readable. */
    off(): void;
  }

  /**
   * Maps a producer handle type to the corresponding consumer type. The
   * generated `smudgy:state/...` / `smudgy:events/...` /
   * `smudgy:procedures/...` typings use it to derive what consumers see
   * from a package's exports; you'll rarely need to name it yourself.
   */
  export type ConsumerOf<H> = H extends StateHandle<infer T>
    ? StateConsumer<T>
    : H extends EventHandle<infer T>
      ? EventConsumer<T>
      : H extends DerivedHandle<infer U>
        ? StateConsumer<U>
        : // Last: every member of ProcedureHandle is an optional phantom, so any object
          // type matches it structurally — the earlier arms must claim theirs first.
          H extends ProcedureHandle<infer A, infer R>
          ? ProcedureConsumer<A, R>
          : never;

  /**
   * The payload type a handle carries, from either side: what a handler
   * receives (state snapshots and event payloads arrive read-only), or what
   * a procedure call sends.
   *
   * ```ts
   * import { prompt } from "smudgy:events/kapusniak/arctic-prompt";
   * import type { Payload } from "smudgy:core";
   * function onPrompt(p: Payload<typeof prompt>) { console.log(p.hp); }
   * ```
   *
   * Every generated module also exports each handle's payload as a type with
   * the handle's own name, so
   * `function onPrompt(p: prompt)` works directly, and single-handle
   * subpath modules export it as `Payload`. Use this helper in generic code.
   */
  export type Payload<H> = H extends StateHandle<infer T>
    ? Readonly<T>
    : H extends StateConsumer<infer T>
      ? Readonly<T>
      : H extends EventHandle<infer T>
        ? Readonly<T>
        : H extends EventConsumer<infer T>
          ? Readonly<T>
          : H extends DerivedHandle<infer U>
            ? Readonly<U>
            : // Last for the same structural reason as in ConsumerOf.
              H extends ProcedureHandle<infer A, any>
              ? A
              : H extends ProcedureConsumer<infer A, any>
                ? A
                : never;

  /**
   * Creates a shared state object. Like {@link createEvent}, the export
   * names the state:
   *
   * ```ts
   * import { createState } from "smudgy:core";
   *
   * export interface PromptData { hp: number; maxhp: number }
   *
   * export const promptState = createState<PromptData>();
   *
   * promptState.set({ hp: 42, maxhp: 100 });
   * ```
   *
   * Consumers then get a fully typed read-only view:
   *
   * ```ts
   * import { promptState } from "smudgy:state/you/your-package";
   * const hp = promptState.value?.hp;
   * ```
   *
   * The export name is the public state name. Pass a name explicitly when the
   * variable needs a different name, or when a state and event should share one:
   * `export const promptState = createState('prompt')`.
   *
   * Use state when consumers need the current value or need to watch nested
   * paths. Use {@link createEvent | an event} when every occurrence matters.
   */
  export function createState<T = unknown>(name?: string): StateHandle<T>;

  /**
   * Creates an event emitter. Like {@link createState}, the export names
   * the event: the system-wide name of the event is the name of the export,
   * and it must be exported from the top level of a package or module.
   *
   * ```ts
   * import { createEvent } from "smudgy:core";
   *
   * interface PromptData { hp: number; maxhp: number }
   * export const prompt = createEvent<PromptData>();
   * prompt.emit({ hp: 42, maxhp: 100 });
   * ```
   *
   * If you only need light event-passing within a package or module, consider
   * using an `EventEmitter` from `node:events` instead of a system-wide event.
   */
  export function createEvent<T = unknown>(name?: string): EventHandle<T>;

  /**
   * Creates a procedure: a function other scripts and packages can call.
   * Direct function calls cannot cross sandbox boundaries, so a procedure is
   * the public entry point for an operation implemented by another package.
   *
   * Calls are delivered asynchronously and are fire-and-forget. Publish a
   * state or emit an event if the caller needs to observe an outcome.
   *
   * ```ts
   * import { createProcedure } from "smudgy:core";
   *
   * export const refresh = createProcedure((args: { full: boolean }, sender) => {
   *   console.log(`refresh requested by ${sender}`, args.full);
   * });
   * ```
   */
  export function createProcedure<A = unknown, R = void>(
    impl: (args: A, sender: string) => R | Promise<R>,
  ): ProcedureHandle<A, R>;
  export function createProcedure<A = unknown, R = void>(
    name: string,
    impl: (args: A, sender: string) => R | Promise<R>,
  ): ProcedureHandle<A, R>;

  /**
   * Creates a state whose value is computed from another package's state.
   * Use it to bind computed values to widgets, because binding paths are
   * plain lookups and cannot contain expressions:
   *
   * ```ts
   * import { createDerived } from "smudgy:core";
   * import { vitals } from "smudgy:state/kapusniak/arctic-prompt";
   * export const hpPct = createDerived(vitals, v => v.hp / v.maxhp);
   * // <ProgressBar value={hpPct.bind()} />
   * ```
   *
   * Like {@link createState}, the export names the handle; pass a name
   * first (`createDerived('hpPct', vitals, …)`) to set it explicitly.
   *
   * The computation re-runs when the source changes (once per writing turn),
   * and the result is published under the name as your own shared state, so
   * other scripts can bind, watch, and consume it like any state you
   * declare. Nothing is computed while the source has no published value.
   */
  export function createDerived<U = unknown, S = any>(
    source: StateConsumer<S>,
    compute: (snapshot: Readonly<S>) => U,
  ): DerivedHandle<U>;
  export function createDerived<U = unknown, S = any>(
    name: string,
    source: StateConsumer<S>,
    compute: (snapshot: Readonly<S>) => U,
  ): DerivedHandle<U>;

  /**
   * Looks up an event by name at runtime, for generic tooling that doesn't
   * know the event ahead of time. `producer` is `"smudgy://owner/name"`,
   * `"user"`, or a platform name (`"sys"`, `"map"`); the payload is
   * untyped. The `smudgy:events/...` modules serve the same handles fully
   * typed.
   */
  export const events: {
    lookup(producer: string, name: string): EventConsumer<unknown>;
  };

  // ---- GMCP ----------------------------------------------------------------

  /**
   * Everything the server has sent over GMCP, one entry per message name.
   * `import gmcp from "smudgy:state/gmcp"` serves the live tree:
   * `gmcp.value.Char.Vitals.hp` is the latest reading, and
   * `gmcp.watch("Char.Vitals", ...)` runs on each vitals message.
   *
   * Paths reach inside payloads too: `gmcp.watch("Char.Vitals.hp", ...)`
   * hands the handler just the number. It runs on every message that
   * covers the path, so a vitals update that left `hp` unchanged still
   * fires; compare against `gmcp.previousValue` to react to change alone.
   *
   * Message names are matched case-insensitively, so `Char.Vitals` finds the
   * data whether the server spells it `Char.Vitals` or `char.vitals`.
   *
   * The declared entries are a widely-implemented set of GMCP state
   * objects. Games send others, and every message the server sends appears
   * in the tree whether or not it is declared here; an undeclared message
   * reads as `unknown`. A script can type the messages of the game it
   * supports by extending this interface and casting the handle. A game
   * that adds a `Room.Weather` message keeps the declared `Room.Info`
   * typing by intersecting:
   *
   * ```ts
   * import gmcp from "smudgy:state/gmcp";
   * import type { StateConsumer, GmcpTree } from "smudgy:core";
   *
   * interface FenworldGmcp extends GmcpTree {
   *   Room?: NonNullable<GmcpTree['Room']> & {
   *     Weather?: { temp?: number; rain?: boolean };
   *   };
   * }
   *
   * const fenGmcp = gmcp as StateConsumer<FenworldGmcp>;
   * const temp = fenGmcp.value?.Room?.Weather?.temp;  // number | undefined
   * ```
   */
  export interface GmcpTree {
    Char?: {
      /** Hit points, mana, and their maximums; some servers add more. */
      Vitals?: { hp?: number; maxhp?: number; mp?: number; maxmp?: number; [field: string]: unknown };
      /** Character status: level, guild, and whatever else the game reports. */
      Status?: { level?: number; [field: string]: unknown };
      Name?: { name?: string; fullname?: string; [field: string]: unknown };
      [message: string]: unknown;
    };
    Room?: {
      /**
       * The room the character is in: a server-wide room number, the room
       * name, the area/zone, and an exits map of direction to destination
       * room number.
       */
      Info?: {
        num?: number;
        name?: string;
        area?: string;
        zone?: string;
        environment?: string;
        terrain?: string;
        exits?: Record<string, number>;
        [field: string]: unknown;
      };
      [message: string]: unknown;
    };
    Comm?: {
      /** A chat/channel message: which channel, who spoke, and the text. */
      Channel?: { chan?: string; player?: string; msg?: string; [field: string]: unknown };
      [message: string]: unknown;
    };
    [pkg: string]: unknown;
  }

  /**
   * The shape of the MSDP tree (`import msdp from "smudgy:state/msdp"`), one
   * entry per variable the server reports. MSDP is string-typed on the wire,
   * so every scalar arrives as a string ("14100", not 14100) — parse numbers
   * where you need them. The well-known room variables are typed; everything
   * else is open. Games document their own variables; narrow the same way as
   * {@link GmcpTree}.
   */
  export interface MsdpTree {
    /**
     * The composite room table (servers that send one): the server's room
     * number, name, area, and an exits map of direction to destination room
     * number.
     */
    ROOM?: {
      VNUM?: string;
      NAME?: string;
      AREA?: string;
      TERRAIN?: string;
      ENVIRONMENT?: string;
      EXITS?: Record<string, string>;
      COORDS?: { X?: string; Y?: string; Z?: string; [field: string]: unknown };
      [field: string]: unknown;
    };
    /** Flat spellings some servers send instead of (or beside) `ROOM`. */
    ROOM_VNUM?: string;
    ROOM_NAME?: string;
    ROOM_EXITS?: Record<string, string>;
    ROOM_TERRAIN?: string;
    AREA_NAME?: string;
    /** The variables the server can report, sent in reply to the handshake. */
    REPORTABLE_VARIABLES?: string[];
    [variable: string]: unknown;
  }

  /**
   * GMCP protocol status and control for the current session.
   */
  export const gmcp: {
    /** Whether GMCP is active on the current connection. */
    readonly enabled: boolean;
    /**
     * Runs `callback` once GMCP is ready. When GMCP is already active, the
     * callback is called immediately, before `onReady` returns; otherwise
     * it runs once when the server next completes GMCP negotiation. Code
     * that runs at load time gets its callback whether it loads before or
     * after the connection.
     */
    onReady(callback: () => void): void;
    /**
     * Sends a GMCP message to the game. Call `gmcp.send("Char.Items.Inv")`
     * with only a name, or pass JSON-serializable data as the second argument.
     * Messages are dropped (with a one-time notice) while GMCP is not active;
     * use `onReady` to wait for it.
     */
    send(name: string, data?: unknown): void;
    /**
     * Asks the server to turn a GMCP module on — `gmcp.enableModule("IRE.Rift")`
     * — so its messages start arriving. `version` defaults to 1. Modules are
     * shared: the server keeps a module on while anything still uses it, and
     * turning one on that another script already enabled costs nothing.
     * A module enabled before the connection is requested as part of the
     * GMCP handshake.
     */
    enableModule(name: string, version?: number): void;
    /**
     * Releases this script's use of a GMCP module. The server is asked to
     * turn the module off only when no other script still uses it.
     */
    disableModule(name: string): void;
    /**
     * Marks message names whose payloads merge into the retained value
     * instead of replacing it — for servers that send only the changed
     * fields after an initial full send. `Char.Status` is always treated
     * this way; `gmcp.mergeKeys("Char.Defences")` adds more.
     */
    mergeKeys(...names: string[]): void;
  };

  // ---- Sessions -----------------------------------------------------------

  /** The name and subtext (caption) associated with a session. */
  export interface Profile {
    name?: string;
    subtext?: string;
  }

  /** The terminal's color scheme, as `#rrggbb` hex strings. */
  export interface Palette {
    /**
     * The 16 ANSI colors as `#rrggbb` strings: the 8 normal shades first, then
     * the 8 bright ones (black, red, green, yellow, blue, magenta, cyan, white).
     */
    ansi: string[];
    foreground: string;
    background: string;
    echo: string;
    warn: string;
    output: string;
    selection: string;
    inputBackground: string;
    /** The app accent color, if the color scheme defines one. */
    accent?: string;
  }

  /**
   * The read-only app settings returned by {@link getSettings}. Only display
   * and behavior settings are exposed. `palette` can be briefly absent right
   * after a session starts.
   */
  export interface Settings {
    /** Separates multiple commands typed on one input line (e.g. `;`); empty
     *  disables splitting. */
    commandSeparator: string;
    /** Lines starting with this prefix are sent verbatim; empty disables it. */
    rawLinePrefix: string;
    /** The scrollback buffer's maximum line count. */
    scrollbackLength: number;
    terminalFontFamily: string;
    /** Terminal font size in pixels (line height is `size * 1.25`). */
    terminalFontSize: number;
    /** Maximum terminal line length in columns; absent means wrap to pane width. */
    terminalLineLength?: number;
    /** The active color-scheme name. */
    theme: string;
    /** What the command input does with the text after a send. */
    commandInputBehavior: "selectAllClearOnBlur" | "selectAll" | "clear";
    /** The resolved terminal palette; can be briefly absent at session start. */
    palette?: Palette;
  }

  /** How a saved automation's `script` body runs: `"js"`/`"ts"` execute it as
   *  code; `"plaintext"` (the default) sends it as a literal command template. */
  export type ScriptLang = "plaintext" | "js" | "ts";

  /** A saved alias, as stored in `aliases.json` and shown in the automations
   *  window. */
  export interface SavedAlias {
    /** The regex matched against what you type. */
    pattern: string;
    /** The body: a command template, or code when `language` is `"js"`/`"ts"`. */
    script?: string;
    /** Defaults to `true`. */
    enabled?: boolean;
    /** Higher values run first. Defaults to `0`; equal priorities keep registration order. */
    priority?: number;
    /** Continue checking later aliases from the same script/package. Defaults to `true`. */
    fallthrough?: boolean;
    /** Defaults to `"plaintext"`. */
    language?: ScriptLang;
    /** Optional folder grouping in the automations window. */
    package?: string;
  }

  /** A saved trigger, as stored in `triggers.json` and shown in the
   *  automations window. */
  export interface SavedTrigger {
    /** Regexes matched against each incoming line's displayed text. */
    patterns?: string[];
    /** Regexes matched against the raw incoming line, before ANSI color codes
     *  are stripped. Use these to match on colors. */
    rawPatterns?: string[];
    /** Vetoes: if any of these match, the trigger does not fire. */
    antiPatterns?: string[];
    script?: string;
    /** Defaults to `true`. */
    enabled?: boolean;
    /** Also test prompts, not just complete lines. Defaults to `false`. */
    prompt?: boolean;
    /** Higher values run first. Defaults to `0`; equal priorities keep registration order. */
    priority?: number;
    /** Continue checking later triggers from the same script/package. Defaults to `true`. */
    fallthrough?: boolean;
    /** Defaults to `"plaintext"`. */
    language?: ScriptLang;
    package?: string;
  }

  /** A saved hotkey, as stored in `hotkeys.json` and shown in the automations
   *  window. */
  export interface SavedHotkey {
    /** The main key (e.g. `"A"`, `"F1"`, `"Space"`). */
    key: string;
    /** Modifier keys held with it (e.g. `["Control", "Shift"]`). */
    modifiers?: string[];
    script?: string;
    /** Defaults to `true`. */
    enabled?: boolean;
    /** Defaults to `"plaintext"`. */
    language?: ScriptLang;
    package?: string;
  }

  /**
   * A handle to one saved automation, returned by a registry's `save`/`get`.
   * Reads are a snapshot: `def()` returns the definition as last read, and
   * `refresh()` re-reads it from disk. `update()` and `delete()` write to disk
   * and reload the server's other sessions.
   */
  export interface SavedAutomationHandle<Def> {
    /** The automation's name (its key in the saved set). */
    readonly name: string;
    /** The saved definition as last read into this handle. */
    def(): Def;
    /** Re-read the definition from disk. Returns `false` if the automation no
     *  longer exists. */
    refresh(): boolean;
    /** Save a partial change: `patch`'s fields are merged onto the current
     *  saved definition and written back. */
    update(patch: Partial<Def>): boolean;
    /** Remove the saved automation. */
    delete(): boolean;
  }

  /** A handle to a saved alias. */
  export type SavedAliasHandle = SavedAutomationHandle<SavedAlias>;
  /** A handle to a saved trigger. */
  export type SavedTriggerHandle = SavedAutomationHandle<SavedTrigger>;
  /** A handle to a saved hotkey. */
  export type SavedHotkeyHandle = SavedAutomationHandle<SavedHotkey>;

  /**
   * Manage one kind of saved automation. `save` creates or replaces and
   * returns a handle; `get` returns a handle to an existing name;
   * `list`/`exists` inspect; `delete` removes. Every write is saved to disk,
   * takes effect in this session, and reloads the server's other sessions.
   */
  export interface SavedAutomationRegistry<Def, Handle> {
    save(name: string, def: Def): Handle;
    get(name: string): Handle | undefined;
    list(): string[];
    exists(name: string): boolean;
    delete(name: string): boolean;
  }

  /**
   * Create and edit the saved automations (the aliases, triggers, and hotkeys
   * shown in the automations window), as opposed to the ones scripts create
   * with `createAlias`/`createTrigger`/`createHotkey`. One
   * {@link SavedAutomationRegistry} per kind.
   *
   * Not available to sandboxed packages: saved automations run outside any
   * sandbox, so writing one would let a package run code outside its own.
   */
  export interface UserAutomations {
    aliases: SavedAutomationRegistry<SavedAlias, SavedAliasHandle>;
    triggers: SavedAutomationRegistry<SavedTrigger, SavedTriggerHandle>;
    hotkeys: SavedAutomationRegistry<SavedHotkey, SavedHotkeyHandle>;
  }

  // ---- Panes ----------------------------------------------------------------

  /** Which side of the pane you split from the new pane appears on. */
  export type SplitDirection = "left" | "right" | "top" | "bottom";

  /**
   * When a pane's title bar (its header, which is also its drag handle) is
   * shown. `'normal'` follows the global distraction-free rule: headers show
   * while the window's toolbar is expanded, or when the "hide panel headers"
   * setting is off. `'always-show'` keeps the header visible regardless. A
   * pane without a visible header cannot be drag-rearranged; dividers still
   * resize it.
   */
  export type TitleBarSpec = "normal" | "always-show";

  /**
   * A pane's own input line (see {@link PaneSpecBase.input}). What the user
   * submits there goes to your `onSubmit` handler and nowhere else: nothing
   * is sent, matched against aliases, or echoed unless the handler does it,
   * and the main input's history is untouched. `session.send(text)` inside
   * the handler reproduces normal typed-command behavior.
   *
   * ```ts
   * import { session } from "smudgy:core";
   * // A chat pane whose input auto-prefixes the channel.
   * session.mainPane.split("right", {
   *   name: "Chat",
   *   width: 300,
   *   input: { onSubmit: (text) => session.send(`gt ${text}`), placeholder: "group tell..." },
   * });
   * ```
   */
  export interface PaneInputSpec {
    /** Receives each submitted line. The text is yours alone: nothing is
     *  sent to the server, matched against aliases, or echoed unless you do
     *  it here, and the main input's history never records it. */
    onSubmit: (text: string) => void;
    /** Hint text shown while the input is empty. */
    placeholder?: string;
  }

  /** The direction-independent half of the spec for {@link Pane.split}. */
  export interface PaneSpecBase {
    /** Required. Names are case-insensitive (display case is preserved) and
     *  namespaced per package. Up to 64 printable characters; `main`, `get`,
     *  `list`, `exists` and `then` are reserved. */
    name: string;
    /** Default `true`. Pass `false` for a widgets-only pane with no terminal;
     *  `echo`/`clear` throw on it. Every pane can host widgets either way. */
    terminal?: boolean;
    /** Default `'normal'`. Also applies to an **existing** pane: `split()`
     *  naming an existing pane (including `'main'`) with an explicit
     *  `titleBar` updates its policy. */
    titleBar?: TitleBarSpec;
    /** Give the pane its own input line (see {@link PaneInputSpec}). Part of
     *  what the pane is, like `terminal`: `split()` naming an existing pane
     *  that has no input while asking for one throws (close it first). Works
     *  on either pane kind; usable on your own session only. Re-splitting
     *  with the same spec re-registers `onSubmit`, which is also how a
     *  handler comes back after your script reloads. */
    input?: PaneInputSpec;
  }

  /**
   * The spec for {@link Pane.split}. Give the new pane's starting size in
   * pixels along the split axis: `width` when splitting `left`/`right`,
   * `height` when splitting `top`/`bottom`. The user can resize it afterwards.
   */
  export type PaneSpec<D extends SplitDirection> = PaneSpecBase &
    (D extends "left" | "right"
      ? { width?: number; height?: never }
      : { height?: number; width?: never });

  /**
   * A handle to one session pane. Panes are keyed by name: `split()` with an
   * existing name returns that pane. Most of the spec is then ignored, with
   * two exceptions. An explicit `titleBar` updates the pane's policy. And
   * `input` is part of what the pane is: asking for one on an existing pane
   * that has none throws (close it first), while re-splitting a pane that
   * has one re-registers its `onSubmit` (placeholder changes are ignored).
   * A pane closes when `close()` is called, when the session ends, or when no
   * script re-claims it during a reload; any `split()` naming it during the
   * reload keeps it, placement untouched. A later `split()` with the same
   * name recreates the pane and re-attaches its widgets.
   *
   * ```ts
   * import { session, createTrigger, line } from "smudgy:core";
   * // A chat pane above the main terminal; clan tells route into it.
   * const chat = session.mainPane.split("top", { name: "Chat", height: 100 });
   * createTrigger(/tells your clan '/, () => line.redirect(chat));
   * ```
   */
  export interface Pane {
    /** The pane's name in its display case. */
    readonly name: string;
    /** Whether this pane has a terminal (`"terminal"`) or is widgets-only
     *  (`"widgets"`). Every pane can host widgets; the main pane is always
     *  `"terminal"`. */
    readonly kind: "terminal" | "widgets";
    readonly isMain: boolean;
    /** `false` when `split()` returned an already-existing pane. */
    readonly created?: boolean;
    /** Write whole lines into this pane's terminal. Throws on widgets-only panes.
     *  Takes styled text too, and works directly as a template tag. */
    echo(text: string | StyledText): void;
    echo(text: TemplateStringsArray, ...values: unknown[]): void;
    /** Clear this pane's terminal scrollback (works on main). Throws on widgets-only panes. */
    clear(): void;
    /** Close this pane. Throws on the main pane; safe to repeat otherwise. */
    close(): void;
    /** This pane's own input line, or `undefined` for panes created without
     *  one (see {@link PaneInputSpec}). The same handle as {@link InputHandle},
     *  addressed at this pane: its text, focus, masking, completion words,
     *  and history are all the pane input's own, independent of the main
     *  input's. On the main pane this is `undefined` too; its input is
     *  {@link Session.input}. */
    readonly input: InputHandle | undefined;
    /** Split a new pane off this one (get-or-create by name; an explicit
     *  `titleBar` also updates an existing pane's policy, including main's). */
    split<D extends SplitDirection>(direction: D, spec: PaneSpec<D>): Pane;
  }

  /**
   * A session's pane registry: `get`/`list`/`exists` cover your own panes
   * (plus the main pane), and dot access reaches any name
   * (`session.panes.chat`). On another session, only `split`, `close`, and a
   * pane's `echo`/`clear` may be used; lookups work on your own session only.
   */
  export interface PaneRegistryMethods {
    get(name: string): Pane | undefined;
    list(): Pane[];
    exists(name: string): boolean;
  }
  /** A pane registry with both method and property access (`panes.get("chat")`
   *  and `panes.chat`). */
  export type PaneRegistry = PaneRegistryMethods & { readonly [name: string]: Pane | undefined };

  // ---- The command input ----------------------------------------------------

  /**
   * Tab-completion words registered by this script (see
   * {@link InputHandle.completion}). Registry methods expose only this
   * script's words; the input combines contributions from every script when
   * it offers completions.
   *
   * Words are case-insensitive single tokens of at most 64 characters. A
   * registry holds up to 512 words. Adding an existing word is idempotent and
   * updates its stored casing. Registrations do not persist across reloads.
   *
   * ```ts
   * import { input } from "smudgy:core";
   * input.completion.add("fireball", "featherfall", "Fjord");
   * input.completion.blacklist.add("ooc");
   * ```
   */
  export interface WordSetRegistry {
    /** Register words. Each is one token: non-empty, no spaces, at most 64
     *  characters. A set holds up to 512 of your words. */
    add(...words: string[]): void;
    /** Remove one of your words (matched case-insensitively). Returns whether
     *  it was there. */
    delete(word: string): boolean;
    /** Whether you registered this word (matched case-insensitively). */
    has(word: string): boolean;
    /** Your words, in the order you added them, as registered. */
    list(): string[];
    /** Remove all of your words. Other scripts' words stay. */
    clear(): void;
  }

  /**
   * An input's shared command history: the lines the Up arrow recalls (see
   * {@link InputHandle.history}). Every script sees and changes the same
   * history for that input.
   *
   * `list()` reflects history as of the most recent submission (or scripted
   * change). Password-mode submissions never enter history, so they never
   * appear here.
   *
   * ```ts
   * import { input, createAlias } from "smudgy:core";
   * // "again 2" offers back the command typed two submissions ago. When
   * // the alias runs, list()[0] is the "again 2" line itself, so the
   * // command before it sits at [1].
   * createAlias(/^again (?<n>\d+)$/, ({ n }) => {
   *   const entry = input.history.list()[Number(n)];
   *   if (entry) input.propose(entry);
   * });
   * ```
   */
  export interface InputHistory {
    /** The history entries, newest first. */
    list(): string[];
    /**
     * Add a line to history without sending it, exactly as if the user had
     * submitted it: the line becomes the newest entry, an older duplicate is
     * removed, and the oldest entry falls off once history is full (100
     * entries). The line must be non-blank and a single line.
     */
    push(text: string): void;
    /** Remove every history entry. */
    clear(): void;
  }

  /**
   * Inspect or edit a command input. The main input is exported as
   * {@link input}; a pane with its own input exposes the same API through
   * {@link Pane.input}.
   *
   * Input state is synchronized from the UI and can briefly trail very recent
   * typing. Text delivered by the `submit` event is exact.
   *
   * The command line belongs to the user. Use `propose()` to offer a command
   * without overwriting text they are editing. The proposed text is selected:
   * Enter submits it, while typing replaces it. Use `replace()` only when the
   * script should overwrite the current contents.
   *
   * Cursor and selection positions count UTF-16 code units, the same units
   * as JavaScript string indexing into `value`.
   *
   * ```ts
   * import { input } from "smudgy:core";
   * // Offer a command for the user to confirm or amend.
   * input.propose("cast 'heal' Tom");
   * input.focus();
   * ```
   */
  export interface InputHandle {
    /** The input's current text. Empty while masked. */
    readonly value: string;
    /** The cursor position, in UTF-16 code units (the units of JavaScript
     *  string indexing). */
    readonly cursor: number;
    /** The selected range, in UTF-16 code units, or `null` when nothing is
     *  selected. */
    readonly selection: { start: number; end: number } | null;
    /** Whether the input has keyboard focus. */
    readonly focused: boolean;
    /**
     * Enable password mode. While masked, `value` is empty, completion and
     * history are disabled, and submissions are not echoed. Revealing the
     * text in the UI does not make it readable by scripts.
     *
     * Text already in the input is restored when masking ends. Text entered
     * while masked is never exposed through this handle.
     *
     * A masked pane input still sends submitted text to that pane's
     * `onSubmit` handler. No other script receives it.
     *
     * Script-controlled masking and the main input's server-controlled
     * password masking are independent. Setting `masked = false` releases
     * only the mask established by the script.
     */
    masked: boolean;

    /** Replace the input's text. The cursor moves to the end. */
    replace(text: string): void;
    /** Add text at the end of the input. The cursor moves to the end. */
    append(text: string): void;
    /** Empty the input. */
    clear(): void;
    /**
     * Put a command in the input, fully selected: Enter sends it, and typing
     * anything discards it. Prefer this over `replace()` when suggesting a
     * command.
     */
    propose(text: string): void;

    /** Place the cursor at a position, counted in UTF-16 code units. */
    setCursor(pos: number): void;
    /** Select from `start` to `end`, counted in UTF-16 code units. */
    select(start: number, end: number): void;
    /** Select everything in the input. */
    selectAll(): void;

    /** Give the input keyboard focus. */
    focus(): void;
    /** Take keyboard focus away from the input. */
    blur(): void;
    /** Submit the input's contents, exactly as if the user pressed Enter. */
    submit(): void;

    /**
     * Your tab-completion words for this input (see {@link WordSetRegistry}).
     * When the user presses Tab, registered words matching the typed prefix
     * are offered first, then words from recent output. Every script's
     * contributions are offered together, in registration order. `blacklist`
     * holds words never to offer, from either source, matched
     * case-insensitively.
     */
    readonly completion: WordSetRegistry & { readonly blacklist: WordSetRegistry };

    /** The input's command history, newest first: what the Up arrow recalls
     *  (see {@link InputHistory}). */
    readonly history: InputHistory;
  }

  /**
   * The submission a `submit` event handler is processing: what the user
   * typed, on its way into the client. Only meaningful inside a handler for
   * the `submit` event from `smudgy:events/sys`; anywhere else it throws.
   *
   * Handlers run in order and act on the same submission, so a later handler
   * reads any replacement an earlier one made, and a cancel from any handler
   * is final.
   */
  export interface Submission {
    /** The submitted line as it currently stands. */
    readonly text: string;
    /**
     * Substitute the line: aliases, command splitting, and prefix handling
     * all process the new text instead of what was typed.
     */
    replace(text: string): void;
    /**
     * Discard the submission entirely. Nothing reaches aliases or the MUD,
     * and no later handler can restore it. The input has already applied its
     * normal post-submit behavior; cancellation only stops further processing.
     */
    cancel(): void;
  }

  /** The submission a `submit` event handler is processing (see
   *  {@link Submission}). */
  export const submission: Submission;

  /**
   * A MUD session. Every method acts on the session the handle names, which
   * need not be the one your script is running in: {@link session} is your
   * own, and {@link getSessions} / {@link byName} reach any connected one, so
   * `byName("scout")?.send("look")` drives another character.
   *
   * On a session other than your own, panes can be split, closed, and written
   * to, but not listed or looked up.
   */
  export interface Session {
    /** The session's numeric id. */
    readonly id: number;
    /** The session's profile (name + subtext). */
    readonly profile: Profile;
    /** Echo a line into this session's output (local; not sent to the MUD).
     *  Takes styled text too, and works directly as a template tag. */
    echo(line: string | StyledText): void;
    echo(text: TemplateStringsArray, ...values: unknown[]): void;
    /** Send a command to this session's MUD (alias processing + command splitting). */
    send(line: string): void;
    /** Send text to this session's MUD verbatim. */
    sendRaw(line: string): void;
    /** Reload this session's scripts and automations. */
    reload(): void;
    /** This session's main (output + input) pane. */
    readonly mainPane: Pane;
    /** This session's pane registry (see {@link PaneRegistry}). */
    readonly panes: PaneRegistry;
    /** This session's command input (see {@link InputHandle}). Usable on your
     *  own session only. */
    readonly input: InputHandle;
    toString(): string;
  }

  /** The session your script is running in. */
  export const session: Session;
  /** Your session's command input (see {@link InputHandle}). */
  export const input: InputHandle;
  /** Your session's numeric id. */
  export const id: number;
  /**
   * All connected sessions.
   *
   * ```ts
   * import { getSessions, createAlias } from "smudgy:core";
   * // Typing "*<anything>" sends that command to every connected session.
   * createAlias(/^\*(?<command>.*)$/, ({ command }) => {
   *   for (const s of getSessions()) s.send(command);
   * });
   * ```
   */
  export function getSessions(): Session[];
  /** Your session's profile. */
  export function getProfile(): Profile;
  /** The current app settings as set in the preferences window. Read-only. */
  export function getSettings(): Settings;
  /** Your script's (or package's) data directory (`$DATA`), as an absolute path. */
  export function getDataDir(): string;
  /** Manage the saved automations (see {@link UserAutomations}). */
  export const userAutomations: UserAutomations;
  /** The first connected session whose profile name is `name`.  Returns `undefined` if no match is found. */
  export function byName(name: string): Session | undefined;

  // ---- Session output -----------------------------------------------------

  /**
   * A piece of styled text, built with {@link style} or {@link link}. Accepted
   * everywhere plain text is: `echo` (and a session's or pane's `echo`), and a
   * line's `insert`, `replaceAt`, and `replace`. Fragments nest: interpolate one
   * inside another and the inner text keeps its own styling, inheriting anything
   * it didn't set from the fragment around it.
   */
  export interface StyledText {
    /** Marks a value as styled text. Fragments come from the {@link style} tag;
     *  this property just keeps other values from being mistaken for one. */
    readonly __smudgyStyled: true;
  }

  /** A template tag producing {@link StyledText}. Interpolated fragments keep
   *  their styling; any other value becomes plain text, exactly as it would in
   *  an ordinary template string. */
  export interface StyleTag {
    (text: TemplateStringsArray, ...values: unknown[]): StyledText;
  }

  /**
   * Builds styled text. Use it as a template tag, optionally picking colors
   * first. Each step is itself a tag, so all of these work:
   *
   * ```ts
   * import { echo, style } from "smudgy:core";
   *
   * echo`A ${style.red`red`} word and ${style.blue.bgYellow`a loud one`}.`;
   * echo(style.fg({ r: 255, g: 128, b: 0 })`exact orange`);
   * echo(style({ fg: "cyan", bg: "black" })`both at once`);
   * ```
   *
   * Color names mean what they mean everywhere else (see {@link Color}): the
   * ANSI names are the bright variant, the theme roles (`default`, `echo`,
   * `output`, `warn`) follow the color scheme, and `fg`/`bg` accept any
   * {@link Color} form, including `{ color, bold: false }` for the dimmer
   * shade. Text a fragment leaves unstyled behaves like plain text: the usual
   * echo color when echoed, the surrounding style when spliced into a line.
   */
  export interface StyleBuilder extends StyleTag {
    /** Both colors at once, in the same shape `highlight` takes. */
    (options: LineColorOptions): StyleBuilder;
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

  /** Builds {@link StyledText} for `echo` and the line-editing methods (see
   *  {@link StyleBuilder}). */
  export const style: StyleBuilder;

  /** Modifier keys held when a link was clicked. */
  export interface LinkClick {
    shift: boolean;
    ctrl: boolean;
    alt: boolean;
  }

  /**
   * Makes text clickable. Pass a command, and clicking the text sends it exactly as
   * if you typed it into the clicked window's session. Pass a function instead, and
   * clicking runs it with the modifier keys that were held:
   *
   * ```ts
   * import { echo, link, send } from "smudgy:core";
   *
   * echo`You see an exit ${link("north")`to the north`}.`;
   * echo`${link((click) => send(click.shift ? "open north" : "north"))`north`}`;
   * ```
   *
   * Links are underlined over a faint wash of the text's own color, so they read as
   * links whatever the text's colors are. Style the text freely — the affordance
   * keeps up:
   *
   * ```ts
   * import { line, link, style } from "smudgy:core";
   *
   * line.replace("north", link("north")`${style.cyan`north`}`);
   * ```
   *
   * A command link works forever, even on old lines. A function link lives with the
   * script that made it: after a script reload the text remains but clicking it does
   * nothing, and only the most recent function links are kept, so a very old one can
   * expire early. Prefer command links for anything long-lived.
   */
  export function link(command: string): StyleTag;
  export function link(onClick: (click: LinkClick) => void): StyleTag;

  /**
   * Print a line in your session's output window; nothing is sent to the MUD.
   * Also usable directly as a template tag:
   *
   * ```ts
   * import { echo, style } from "smudgy:core";
   *
   * echo`hi ${style.red`there`}`;
   * ```
   */
  export function echo(line: string | StyledText): void;
  export function echo(text: TemplateStringsArray, ...values: unknown[]): void;
  /** Send a command to the MUD as if you typed it: aliases run, and the command
   *  separator (e.g. `;`) splits it into multiple commands. */
  export function send(command: string): void;
  /** Send text to the MUD exactly as given: no alias processing, no splitting
   *  on the command separator. */
  export function sendRaw(text: string): void;
  /** Reload the current session's scripts and automations. */
  export function reload(): void;

  // ---- Captures + automations ---------------------------------------------

  /**
   * The captures handed to a trigger or alias handler. `matches[0]` is the
   * whole matched text; `matches[1]`, `matches[2]`, and so on are the capture
   * groups in order. A named group like `(?<who>...)` can also be read by
   * name, as `matches.who`, and handlers often destructure it:
   * `({ who }) => ...`. Every group of the pattern that fired is present: one
   * that matched nothing (an optional group, say) is the empty string, not
   * `undefined` as in standard JavaScript regex matches.
   *
   * When a trigger has several patterns, only the fired pattern's groups are
   * present; the other patterns' groups are absent and read as `undefined`.
   * `"who" in matches` tells you which pattern fired.
   */
  export type Matches = {
    readonly [group: number]: string;
    readonly [name: string]: string;
  };

  /**
   * A trigger/alias body written as a plain string instead of a function: a
   * command template sent to the MUD after substitution.
   * - `$1` … `$9` insert capture groups (single digit; write `${10}` for group ten)
   * - `$name` / `${name}` insert a named group
   * - `$$` is a literal dollar sign
   * Unknown or non-matching groups become the empty string.
   */
  export type InlineTemplate = string;

  /**
   * A match pattern: a regular expression, written either as a `RegExp`
   * (`/^You follow/`) or as a string of regex source (`"^You follow"`).
   * Strings are compiled as regexes, not matched literally.
   */
  type Pattern = string | RegExp;

  /** The three pattern lists a trigger can match with. Most triggers set only
   *  `patterns`. */
  export type TriggerPatterns = {
    /** Regexes tested against each incoming line's displayed text. */
    patterns?: Pattern[];
    /** Regexes tested against the raw incoming line, before ANSI color codes
     *  are stripped. Use these to match on colors. */
    rawPatterns?: Pattern[];
    /** Vetoes: if any of these match the line, the trigger does not fire. */
    antiPatterns?: Pattern[];
  };

  /** Options for {@link createAlias}. */
  export type AliasOptions = {
    /** A name of your choosing. Without one, the alias is named after its
     *  pattern, which is usually all the automations window needs; name it
     *  yourself to tell apart two aliases that share a pattern, or to keep a
     *  stable label your code looks up later. */
    name?: string;
    /** Keep the first registration: if a singleton automation with this name
     *  already exists in the session, `create*` returns the existing one (its
     *  handle reports `created: false`) instead of replacing it. */
    singleton?: boolean;
    /** The alias removes itself after firing this many times (`1` = one-shot). */
    fireLimit?: number;
    /** Higher values run first. Defaults to `0`; equal priorities keep registration order. */
    priority?: number;
    /** Continue checking later aliases from this script/package. Defaults to `true`. */
    fallthrough?: boolean;
  };

  /** Options for {@link createTrigger}. */
  export type TriggerOptions = {
    /** A name of your choosing. Without one, the trigger is named after its
     *  pattern, which is usually all the automations window needs; name it
     *  yourself to tell apart two triggers that share a pattern, or to keep a
     *  stable label your code looks up later. */
    name?: string;
    /** Also test prompts (the partial line the MUD leaves waiting for input),
     *  not just complete lines. Default `false`. */
    prompt?: boolean;
    /** Start enabled? Default `true`; pass `false` to create it switched off
     *  (e.g. a follow-on trigger that an earlier trigger enables). */
    enabled?: boolean;
    /** Keep the first registration: if a singleton automation with this name
     *  already exists in the session, `create*` returns the existing one (its
     *  handle reports `created: false`) instead of replacing it. */
    singleton?: boolean;
    /** The trigger removes itself after firing this many times (`1` = one-shot). */
    fireLimit?: number;
    /** The trigger removes itself after testing this many incoming lines,
     *  whether or not they fired it. */
    lineLimit?: number;
    /** Higher values run first. Defaults to `0`; equal priorities keep registration order. */
    priority?: number;
    /** Continue checking later triggers from this script/package. Defaults to `true`. */
    fallthrough?: boolean;
  };

  /** One trigger in a {@link createTriggers} batch: its patterns, its body,
   *  and the same options as {@link TriggerOptions} (except `name` — the
   *  batch's key is the name). */
  export type TriggerDef = TriggerPatterns & {
    /** The trigger body: a command template string or a function (see
     *  {@link AutomationScript}). */
    script: InlineTemplate | ((matches: Matches) => string | void);
    prompt?: boolean;
    enabled?: boolean;
    singleton?: boolean;
    fireLimit?: number;
    lineLimit?: number;
    priority?: number;
    fallthrough?: boolean;
  };

  /** Options for {@link createTimer}. */
  export type TimerOptions = {
    /** A name of your choosing; without one, the timer is named after its
     *  interval and callback. Re-creating a timer with the same name replaces
     *  the old one. */
    name?: string;
    /** Time between fires, in milliseconds (1000 = one second). Required. */
    intervalMs: number;
    /** Keep firing until stopped. Default `false`: fire once, then the timer
     *  removes itself. */
    repeat?: boolean;
    /** With `repeat`, the timer removes itself after this many fires. */
    fireLimit?: number;
  };

  /** The key combination for {@link createHotkey}. */
  export type KeySpec = {
    /** The main key (e.g. `"F1"`, `"a"`). */
    key: string;
    /** Modifier keys that must be held with it (e.g. `["ctrl", "shift"]`). */
    modifiers?: string[];
  };

  /** Options for {@link createHotkey}. */
  export type HotkeyOptions = {
    /** A name of your choosing; without one, the hotkey is named after its
     *  key combination (e.g. `"ctrl+h"`). Re-creating a hotkey with the same
     *  name replaces the old binding. */
    name?: string;
  };

  /**
   * Either body form an automation accepts: a command template string (see
   * {@link InlineTemplate}), or a function called with the {@link Matches}. If
   * the function returns a string, that string is sent to the MUD as a command
   * (aliases apply to it).
   */
  type AutomationScript = InlineTemplate | ((matches: Matches) => string | void);

  /** A handle to a script-created alias: enable/disable it with `enabled`,
   *  remove it with `delete()`. Returned by {@link createAlias}. */
  export interface Alias {
    /** Its name: the `name` option if one was given, otherwise the pattern. */
    readonly name: string;
    /** `false` when a `singleton` request found an existing automation and
     *  returned that one instead of creating a new one. */
    readonly created?: boolean;
    /** Whether the alias is active: set `false` to disable, `true` to
     *  re-enable. */
    enabled: boolean;
    /** The first pattern's regex source (`""` if the alias no longer exists). */
    readonly pattern: string;
    /** Relative evaluation priority (higher runs first). */
    readonly priority: number;
    /** The declarative fallthrough default for this alias. */
    readonly fallthrough: boolean;
    /** Remove the alias. Safe to call more than once. */
    delete(): void;
  }

  /** A handle to a script-created trigger; the same shape as {@link Alias}.
   *  Returned by {@link createTrigger}. */
  export interface Trigger {
    readonly name: string;
    readonly created?: boolean;
    enabled: boolean;
    readonly pattern: string;
    readonly priority: number;
    readonly fallthrough: boolean;
    delete(): void;
  }

  /** A handle to a script-created timer. Returned by {@link createTimer};
   *  timers are cleared on script reload. */
  export interface Timer {
    readonly name: string;
    /** Whether the timer is running: set `false` to pause, `true` to resume. */
    enabled: boolean;
    /** Stop and remove the timer. Safe to call more than once. */
    delete(): void;
  }

  /** A handle to a script-created hotkey. Returned by {@link createHotkey};
   *  hotkeys are cleared on script reload. */
  export interface Hotkey {
    readonly name: string;
    /** Whether the key is bound: set `false` to unbind, `true` to rebind. */
    enabled: boolean;
    /** Unbind and remove the hotkey. Safe to call more than once. */
    delete(): void;
  }

  /**
   * Look up the automations of one kind that your own scripts created. Each
   * script sees only its own; two scripts can both own a `"heal"` trigger
   * without colliding.
   */
  export interface AutomationRegistry<H> {
    /** The handle for `name`, or `undefined` if you have no such automation. */
    get(name: string): H | undefined;
    /** The names of your automations of this kind. */
    list(): string[];
    /** Whether you have an automation named `name`. */
    exists(name: string): boolean;
  }

  /**
   * Create an alias: a shortcut that watches what **you type** and runs a
   * script instead of sending it. `patterns` is one regex or several; when
   * your input matches, `script` runs: a command template string, or a
   * function that receives the {@link Matches}.
   *
   * ```ts
   * import { createAlias } from "smudgy:core";
   * // Typing "gt any message here" sends "guildtell any message here".
   * createAlias("^gt (.+)$", "guildtell $1");
   * ```
   *
   * The typed command is consumed by default (see {@link capture}). Aliases
   * created this way last until the next script reload, and show up in the
   * automations window named after their pattern (pass `options.name` to
   * label one yourself). Returns an {@link Alias} handle.
   */
  export function createAlias(
    patterns: Pattern | Pattern[],
    script: AutomationScript,
    options?: AliasOptions,
  ): Alias;
  /**
   * Create a trigger: it watches every line **arriving from the MUD** and runs
   * a script on a match. `patterns` is one regex, or a {@link TriggerPatterns}
   * object for raw/anti patterns; `script` is a command template string, or a
   * function that receives the {@link Matches}.
   *
   * ```ts
   * import { createTrigger, send } from "smudgy:core";
   * // Congratulate, reusing the captured name.
   * createTrigger("^(\\w+) has advanced a level", "say Grats, $1!");
   * // A function body can decide what to do; named groups arrive by name.
   * createTrigger(/^(?<hp>\d+)H /, ({ hp }) => {
   *   if (parseInt(hp) < 100) send("flee");
   * });
   * ```
   *
   * Triggers created this way last until the next script reload, and show up
   * in the automations window named after their patterns (pass `options.name`
   * to label one yourself); see {@link TriggerOptions} for prompt matching,
   * fire limits, and more. Returns a {@link Trigger} handle.
   */
  export function createTrigger(
    patterns: Pattern | TriggerPatterns,
    script: AutomationScript,
    options?: TriggerOptions,
  ): Trigger;
  /** Create several triggers in one call: pass an object mapping each name
   *  to its {@link TriggerDef}; get back the same names mapped to their
   *  {@link Trigger} handles. The keys make this the natural form for a
   *  staged chain (`chain.row.enabled = true`) and give multi-pattern
   *  triggers a readable name in the automations window. */
  export function createTriggers(triggers: Record<string, TriggerDef>): Record<string, Trigger>;
  /**
   * Create a timer that runs `callback` after `intervalMs` milliseconds:
   * once by default, or repeatedly with `repeat: true`.
   *
   * ```ts
   * import { createTimer, send } from "smudgy:core";
   * // Keep sipping, every 30 seconds until deleted:
   * const sip = createTimer({ intervalMs: 30000, repeat: true },
   *   () => send("drink potion"));
   * // later: sip.delete();
   * ```
   *
   * Timers are cleared on script reload. Returns a {@link Timer} handle; set
   * `enabled = false` to pause it, or `delete()` to stop it.
   */
  export function createTimer(options: TimerOptions, callback: () => void): Timer;
  /**
   * Bind a keyboard shortcut: `handler` runs whenever the {@link KeySpec}
   * combination is pressed in this session.
   *
   * ```ts
   * import { createHotkey, send } from "smudgy:core";
   * createHotkey({ key: "F1" }, () => send("flee"));
   * createHotkey({ key: "h", modifiers: ["ctrl"] }, () => send("cast 'heal' self"));
   * ```
   *
   * Hotkeys are cleared on script reload. Returns a {@link Hotkey} handle.
   */
  export function createHotkey(keySpec: KeySpec, handler: () => void, options?: HotkeyOptions): Hotkey;

  /** The registry of aliases your scripts created. */
  export const aliases: AutomationRegistry<Alias>;
  /** The registry of triggers your scripts created. */
  export const triggers: AutomationRegistry<Trigger>;
  /** The registry of timers your scripts created. */
  export const timers: AutomationRegistry<Timer>;
  /** The registry of hotkeys your scripts created. */
  export const hotkeys: AutomationRegistry<Hotkey>;

  // ---- Variables ----------------------------------------------------------

  /**
   * Variables shared by every script on this server, persisted across reloads
   * and characters. Read and write plain properties:
   *
   * ```ts
   * import { vars, send } from "smudgy:core";
   * vars.target = "goblin";        // set it in one script...
   * send(`kill ${vars.target}`);   // ...use it in another
   * ```
   *
   * These are internally stored as JSON, so only valid JSON types will
   * persist.
   */
  export const vars: Record<string, any>;

  // ---- Line / buffer / capture --------------------------------------------

  /**
   * A color accepted by the line-styling APIs. One of:
   * - an ANSI color name (`"black"`, `"red"`, `"green"`, `"yellow"`, `"blue"`,
   *   `"magenta"`, `"cyan"`, `"white"`, meaning the bright variant), or a
   *   theme role: `"default"`, `"echo"`, `"output"`, `"warn"`
   * - `{ r, g, b }` with each component 0-255, for an exact color
   * - `{ color, bold }`: an ANSI color name plus an explicit bright/bold flag
   *   (`bold: false` selects the normal, dimmer variant)
   */
  export type Color =
    | string
    | { r: number; g: number; b: number }
    | { color: string; bold: boolean };

  /** One styled run read back from a line. `begin`/`end` are byte offsets into
   *  the line's text (not character counts; multi-byte characters span
   *  several bytes). */
  export interface StyleSpan {
    begin: number;
    end: number;
    fg: Color;
    bg: Color;
  }

  /** Foreground and/or background color for a line write. */
  export interface LineColorOptions {
    fg?: Color;
    bg?: Color;
  }

  /**
   * A line of output you can read and edit. Inside a trigger, {@link line} is
   * the line being processed right now; `buffer.line(n)` reaches an
   * already-printed line by number. The handle remembers which line it points
   * at; methods never take a line number.
   *
   * The line being processed accepts changes only while a trigger or
   * `receive` handler for it is running. Once that handler returns, or after
   * an `await` inside it, the line has moved on: any edit, `gag()`,
   * `redirect()`, or `copy()` then throws rather than landing on whatever
   * line comes next. Reads stay safe anywhere (`text` is `""` and `styles`
   * an empty array outside a handler). Already-printed lines reached through
   * `buffer.line(n)` can be edited at any time.
   *
   * The text-search methods (`replace`, `highlight`, `remove`) find their
   * target by string; the `*At` forms take byte offsets (e.g. from `styles`).
   */
  export interface Line {
    /** Insert `text` at byte offset `begin` (replacing up to `end` if given),
     *  with optional colors. Styled text keeps its own colors and links;
     *  `options` then supplies the colors its unstyled parts get. */
    insert(
      text: string | StyledText,
      begin: number,
      end?: number,
      options?: LineColorOptions,
    ): void;
    /** Replace the byte range `[begin, end)` with `text`. Styled text keeps its
     *  own colors and links; its unstyled parts blend into the surrounding style. */
    replaceAt(text: string | StyledText, begin: number, end: number): void;
    /** Recolor the byte range `[begin, end)`. */
    highlightAt(begin: number, end: number, options?: LineColorOptions): void;
    /** Remove the byte range `[begin, end)`. */
    removeAt(begin: number, end: number): void;
    /** Replace the first occurrence of `oldStr` with `newStr` (plain or styled;
     *  the search side is always plain text). Returns `true` if it was found. */
    replace(oldStr: string, newStr: string | StyledText): boolean;
    /** Recolor the first occurrence of `str`. Returns `true` if it was found. */
    highlight(str: string, options?: LineColorOptions): boolean;
    /** Remove the first occurrence of `str`. Returns `true` if it was found. */
    remove(str: string): boolean;
    /** Hide this line: it never reaches the screen. Current-line only (a
     *  no-op on a buffer line). */
    gag(): void;
    /**
     * Take the current line out of the main view and deliver it to `pane`
     * instead. Styling is kept and later edits still apply; if called
     * repeatedly, the last call wins. Current-line only (a no-op on a buffer
     * line). A `Pane` handle from another session throws.
     */
    redirect(pane: Pane | string): void;
    /**
     * Deliver the current line to `pane` as well as the main view.
     * Current-line only (a no-op on a buffer line).
     */
    copy(pane: Pane | string): void;
    /** The line's text (`""` for a buffer line outside the recent-lines window). */
    readonly text: string;
    /** The line's style runs (`undefined` for a buffer line outside the window). */
    readonly styles: StyleSpan[] | undefined;
    /** The line's number (the current line reports the number it is about to
     *  be assigned). */
    readonly number: number;
  }

  /** Already-printed lines, looked up by number (only roughly the most recent
   *  1000 are reachable). */
  export interface Buffer {
    /** A handle to the already-printed line `lineNumber`. */
    line(lineNumber: number): Line;
  }

  /** The line a trigger is processing. Only meaningful inside a trigger (or
   *  `receive` event) handler for it: elsewhere reads come back empty and
   *  edits throw (see {@link Line}). */
  export const line: Line;
  /** This session's recent-lines buffer. */
  export const buffer: Buffer;
  /**
   * From an **alias** handler: controls whether the command you typed (the one
   * that matched) still goes to the MUD. By default an alias **replaces** your
   * command: the typed line is captured, and the script sends something in its
   * place. Call `capture(false)` to let the original line through. This is
   * useful for scripts that watch what is typed but don't want to change it,
   *  or for aliases that only sometimes want to replace the command.
   *
   * `capture(true)` forces a line to be captured, even if a previously or
   * subsequently alias calls `capture(false)`.
   *
   * No effect in a **trigger** handler: incoming lines are always shown. Use
   * `line.gag()` for similar behavior there.
   */
  export function capture(value: boolean): void;

  /**
   * Inside an alias or trigger function handler, decide whether later matching
   * automations from the same script/package may run for this dispatch. This
   * overrides the automation's `fallthrough` option for this invocation only.
   * Nested `send()` calls begin a fresh alias dispatch.
   *
   * @throws {Error} When called outside an alias or trigger function handler.
   */
  export function fallthrough(value: boolean): void;

  // ---- Mapper -------------------------------------------------------------

  /** The current session's map API (see {@link Mapper}). */
  export const mapper: Mapper;

  // ---- Default export: the current-session facade -------------------------

  /**
   * The whole current-session API on one object. Every member mirrors the
   * named export of the same name.
   */
  export interface SmudgyApi {
    echo(line: string | StyledText): void;
    echo(text: TemplateStringsArray, ...values: unknown[]): void;
    readonly style: StyleBuilder;
    readonly link: typeof link;
    send(command: string): void;
    sendRaw(text: string): void;
    reload(): void;
    capture(value: boolean): void;
    fallthrough(value: boolean): void;
    byName(name: string): Session | undefined;
    getSessions(): Session[];
    getProfile(): Profile;
    getSettings(): Settings;
    getDataDir(): string;
    readonly userAutomations: UserAutomations;
    createState: typeof createState;
    createEvent: typeof createEvent;
    createProcedure: typeof createProcedure;
    createDerived: typeof createDerived;
    readonly events: typeof events;
    readonly gmcp: typeof gmcp;
    createAlias: typeof createAlias;
    createTrigger: typeof createTrigger;
    createTriggers: typeof createTriggers;
    createTimer: typeof createTimer;
    createHotkey: typeof createHotkey;
    readonly aliases: AutomationRegistry<Alias>;
    readonly triggers: AutomationRegistry<Trigger>;
    readonly timers: AutomationRegistry<Timer>;
    readonly hotkeys: AutomationRegistry<Hotkey>;
    readonly vars: Record<string, any>;
    readonly line: Line;
    readonly buffer: Buffer;
    /** The submission a `submit` event handler is processing. */
    readonly submission: Submission;
    /** The map API. */
    readonly mapper: Mapper;
    /** The current session. */
    readonly session: Session;
    /** The current session's command input. */
    readonly input: InputHandle;
    /** The current session id. */
    readonly id: number;
  }

  const api: SmudgyApi;
  export default api;
}

// =============================================================================
//  Platform event catalogs — typed consumer handles for the host's own events.
//  The runtime synthesis lives in script/src/package_resolver.rs
//  (`platform_event_catalog`); a drift test in models/script_typings.rs checks
//  these declarations name exactly the synthesized exports.
// =============================================================================

declare module "smudgy:events/sys" {
  import type { EventConsumer } from "smudgy:core";

  /** Fires when the session connects to the MUD. Empty payload. */
  export const connect: EventConsumer<Record<string, never>>;

  /** Fires when the session disconnects from the MUD. Empty payload. */
  export const disconnect: EventConsumer<Record<string, never>>;

  /**
   * Fires just before a command goes to the MUD. `command` is the final
   * outgoing line, after alias expansion and command splitting.
   */
  export const send: EventConsumer<{ command: string }>;

  /**
   * Fires for each complete line received from the MUD, after triggers have
   * run but before the line is displayed. `text` is the line as originally
   * received; any trigger edits are applied afterward.
   *
   * Inside the handler, the ambient `line` from `smudgy:core` refers to
   * this same incoming line, so `line.gag()`, `line.redirect()`, and
   * `line.replace()` work just as they do in a trigger.
   */
  export const receive: EventConsumer<{ text: string }>;

  /**
   * Fires when a command is submitted from the command input, whether the
   * user pressed Enter or a script called `input.submit()`. `text` is the
   * line exactly as typed, before aliases, command splitting, or prefix
   * handling. Lines sent by scripts do not fire it, and neither do masked
   * (password) submissions.
   *
   * Inside the handler, the ambient `submission` from `smudgy:core` refers
   * to this same submission: `submission.replace()` changes what the rest
   * of the client processes, and `submission.cancel()` discards it. The
   * pairing mirrors `receive` and the ambient `line`.
   */
  export const submit: EventConsumer<{ text: string }>;
}

declare module "smudgy:events/map" {
  import type { EventConsumer } from "smudgy:core";

  /**
   * Fires when the current map location changes, whether or not a mapper
   * package is installed. `areaId` is the area's UUID as a string;
   * `roomNumber` is the room number, or `null` when the location has no
   * specific room.
   *
   * Note that the string `areaId` is a different representation from the
   * `AreaId` pair the `mapper` API uses; the two are not interchangeable.
   * 
   * Unstable: This event is new and may change in future releases. The event
   * itself is guaranteed to remain, but the payload, particularly the
   * areaId, may change.
   */
  export const room: EventConsumer<{ areaId: string; roomNumber: number | null }>;
}

declare module "smudgy:events/input" {
  import type { EventConsumer } from "smudgy:core";

  /**
   * Fires after a command input's text changes. `source` identifies whether
   * the change came from the user, a script, a command link, or another client
   * action such as history recall. `pane` is absent for the main input.
   *
   * Use this event to observe edits. To replace or cancel a submitted command,
   * use `submit` from `smudgy:events/sys`.
   *
   * Identical consecutive states are coalesced. While the input is masked,
   * typing emits no events and no text is reported. The event that begins
   * masking contains `masked: true` without `value`; the event that ends it
   * contains the restored text. Read `input.masked` when the current masking
   * state matters.
   *
   * Changing the input from a handler emits another `change` event. Only write
   * when the new value differs, or the handler can loop.
   *
   * ```ts
   * import { change } from "smudgy:events/input";
   * change.on(({ value, source }) => {
   *   if (source === "user") console.log("draft:", value ?? "");
   * });
   * ```
   */
  export const change: EventConsumer<{
    value?: string;
    masked?: true;
    pane?: string;
    source: "user" | "script" | "link" | "other";
  }>;

  /**
   * Fires when a command input gains or loses keyboard focus. `pane` names
   * the pane whose input it is; it is absent for the main input. `masked` is
   * present (and `true`) while that input is in password mode.
   */
  export const focus: EventConsumer<{
    focused: boolean;
    masked?: true;
    pane?: string;
  }>;
}

declare module "smudgy:events/gmcp" {
  import type { EventConsumer } from "smudgy:core";

  /**
   * Fires once GMCP negotiation completes and the handshake has been sent;
   * GMCP data starts flowing from this moment. For code that may load after
   * the connection, `gmcp.onReady` from `smudgy:core` covers both orders.
   */
  export const ready: EventConsumer<Record<string, never>>;

  /**
   * Fires when GMCP stops on a live connection: the server withdrew it, or
   * the connection dropped while it was active. The last-received data stays
   * readable through `smudgy:state/gmcp`.
   */
  export const closed: EventConsumer<Record<string, never>>;
}

declare module "smudgy:state/gmcp" {
  import type { StateConsumer, GmcpTree } from "smudgy:core";

  /**
   * The live GMCP tree, one entry per message name (see {@link GmcpTree}):
   * read the latest value with `gmcp.value`, subscribe with
   * `gmcp.watch(path, ...)`, and wire widgets with `gmcp.bind(path)`.
   * Each message the server sends is committed as its own update, so a
   * watcher at or under the message's path runs once per message, repeats
   * included.
   */
  const gmcp: StateConsumer<GmcpTree>;
  export { gmcp };
  export default gmcp;
}

declare module "smudgy:events/msdp" {
  import type { EventConsumer } from "smudgy:core";

  /**
   * Fires once MSDP negotiation completes and the room variables have been
   * requested; MSDP data starts flowing from this moment.
   */
  export const ready: EventConsumer<Record<string, never>>;

  /**
   * Fires when MSDP stops on a live connection: the server withdrew it, or
   * the connection dropped while it was active. The last-received data stays
   * readable through `smudgy:state/msdp`.
   */
  export const closed: EventConsumer<Record<string, never>>;
}

declare module "smudgy:state/msdp" {
  import type { StateConsumer, MsdpTree } from "smudgy:core";

  /**
   * The live MSDP tree, one entry per variable name (see {@link MsdpTree}):
   * read the latest value with `msdp.value`, subscribe with
   * `msdp.watch(path, ...)`, and wire widgets with `msdp.bind(path)`. Each
   * server update commits as its own change, so a watcher at or under a
   * variable's path runs once per update.
   */
  const msdp: StateConsumer<MsdpTree>;
  export { msdp };
  export default msdp;
}
