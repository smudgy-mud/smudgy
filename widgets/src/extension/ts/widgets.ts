// Ops come from the global op module "ext:core/ops" (deno's convention; runtime-
// registered extension ops are NOT on Deno.core.ops). Extension source must be
// 7-bit ASCII (deno_core extensions.rs check) -- no non-ASCII chars below.
//
// This is the `smudgy_widgets` extension's ESM entrypoint. It does NOT install
// anything on `globalThis` except the `__smudgy_make_widgets` hook (mirroring how
// `smudgy.js` installs `__smudgy_create_api`). The user-facing surface reaches scripts
// two ways, both routed through that hook:
//   - modules/packages: the synthesized `smudgy:widgets` + `smudgy:widgets/jsx-runtime`
//     virtual modules (see `package_resolver::load_widgets_module`);
//   - inline alias/trigger bodies: we augment `globalThis.__smudgy_user_api` (built by
//     `smudgy.js`, which runs before this extension) so `with (...)` injection exposes
//     bare `createWidget`/`Column`/... with zero extra wiring.
// The op imports live HERE (not in `smudgy.js`) because these ops only exist when this
// leaf extension is loaded; `smudgy.js` lives in `core` and runs in headless test
// runtimes that don't load this extension.
import {
    op_smudgy_widget_create,
    op_smudgy_widget_remove,
    op_smudgy_widget_build_element_list,
    op_smudgy_widget_push_element,
    op_smudgy_widget_build_column,
    op_smudgy_widget_build_container,
    op_smudgy_widget_build_row,
    op_smudgy_widget_build_stack,
    op_smudgy_widget_build_text,
    op_smudgy_widget_build_progress_bar,
    op_smudgy_widget_build_button,
    op_smudgy_widget_build_scrollable,
    op_smudgy_widget_build_markdown,
    op_smudgy_widget_build_modal,
    op_smudgy_widget_build_text_editor,
    op_smudgy_widget_build_map_view,
    op_smudgy_widget_build_canvas,
    op_smudgy_widget_build_space,
    op_smudgy_widget_build_checkbox,
    op_smudgy_widget_build_radio,
    op_smudgy_widget_build_tooltip,
    op_smudgy_widget_build_table,
    op_smudgy_widget_extract_markdown_links,
    op_smudgy_widget_isolate_token,
    // The `smudgy_ops` (core) session ops, used to build `Markdown`'s default link handler and
    // to resolve `createWidget`'s `pane` option. They exist in every isolate that also loads
    // this leaf widgets extension.
    op_smudgy_session_send,
    op_smudgy_get_current_session,
    op_smudgy_pane_resolve,
    // @ts-ignore - ext:core/ops is a deno virtual module with no type decls
} from "ext:core/ops";

// Normalize any children value -- a bare child, an array (jsxs / inline), nested
// Fragment arrays, or undefined -- into one flat, filtered array. `jsx`/`jsxs` pass a
// bare-or-array child; inline authors pass an array; Fragment returns its children
// array; `flat(Infinity)` absorbs all of those. `null`/`undefined`/`false` are dropped
// so conditional children (`cond && <X/>`) work; `0` and `""` are kept (valid text).
function normalizeChildren(children: any): any[] {
    const arr =
        children === undefined || children === null
            ? []
            : Array.isArray(children)
              ? children
              : [children];
    return arr.flat(Infinity).filter((c: any) => c != null && c !== false);
}

function buildChildList(children: any) {
    const list = op_smudgy_widget_build_element_list();
    for (const child of normalizeChildren(children)) {
        op_smudgy_widget_push_element(list, child);
    }
    return list;
}

// The single-child slot shared by the wrapper components (Container/Scrollable/Modal/
// Tooltip): exactly one child renders -- the first -- and an empty slot falls back to an
// empty text element.
function firstChild(children: any) {
    const kids = normalizeChildren(children);
    return kids.length > 0 ? kids[0] : op_smudgy_widget_build_text({}, []);
}

// A single-value content slot (a Tooltip tip, a Table cell/header): an element passes
// through, text and binding tokens become a text run, and arrays/functions fail loudly
// here -- past this point they would die opaquely at the element boundary, far from the
// offending call site.
function elementOrText(value: any, slot: string) {
    if (Array.isArray(value) || typeof value === "function") {
        throw new TypeError(
            "widgets: " + slot + " must be text, a binding, or a single element",
        );
    }
    if (typeof value === "object" && value !== null && !isBindingToken(value)) {
        return value;
    }
    return op_smudgy_widget_build_text({}, textParts([value]));
}

// A store-binding token from smudgy:core's `handle.bind(path?)` -- plain frozen data carrying
// a host-minted id. Tokens are valid at prop positions and as Text children (the build ops
// resolve them to live value cells); they are NOT elements, so a bare token child of a layout
// container fails the same way a bare string child does -- wrap it in <Text>.
function isBindingToken(value: any): boolean {
    return (
        typeof value === "object" &&
        value !== null &&
        typeof value.__smudgyStoreBinding === "number"
    );
}

// Text content for the text build op: strings pass through String() as before, binding
// tokens pass through verbatim so the op can resolve them.
function textParts(children: any): any[] {
    return normalizeChildren(children).map((c: any) => (isBindingToken(c) ? c : String(c)));
}

/**
 * Resolve a `createWidget` `pane` option to the pane's interned name id, throwing if the pane
 * does not exist. Accepts a `Pane` handle (own-session only -- a foreign session's panes live
 * in that session's windows and registry) or a pane name, resolved in the calling isolate's
 * namespace via `op_smudgy_pane_resolve` -- so targeting needs the `panes` capability on top
 * of `widgets`, and a package can never target another namespace's pane ("main" resolves in
 * every namespace). The returned id is matched against live panes at render time, so a pane
 * closed after mounting hides the widget (and a same-name recreate re-attaches it).
 */
function resolvePaneTarget(pane: any): number {
    let name: string;
    if (typeof pane === "string") {
        name = pane;
    } else if (
        typeof pane === "object" &&
        pane !== null &&
        typeof pane._name === "string" &&
        typeof pane._sessionId === "number"
    ) {
        if (pane._sessionId !== op_smudgy_get_current_session()) {
            throw new TypeError(
                "widgets: a Pane belonging to another session cannot host this session's widgets",
            );
        }
        name = pane._name;
    } else {
        throw new TypeError("widgets: options.pane must be a Pane or a pane name");
    }
    const info = op_smudgy_pane_resolve(op_smudgy_get_current_session(), name);
    if (info === null || info === undefined) {
        throw new Error("widgets: no pane named '" + name + "'");
    }
    return info.nameId;
}

/**
 * The links a `Markdown` widget would render for `source`, in document order, as
 * `{ label, url }`. Backed by the same host pipeline the widget parses with (command-autolink
 * expansion included), so escapes, code spans, and reference links behave exactly as they
 * display. Provenance-free pure parsing, so it lives outside `makeWidgets` and its op is
 * ungated by the `widgets` capability.
 */
function extractMarkdownLinks(source: string): { label: string; url: string }[] {
    return op_smudgy_widget_extract_markdown_links(String(source));
}

/**
 * Build the per-creator widget surface. Both delivery paths consume this: the synthesized
 * `smudgy:widgets` virtual module (one instance per importer, `__creator` baked in) and the
 * inline-injection augmentation below. The component factories are provenance-free; only
 * `createWidget`/`removeWidget` are creator-bound -- the widget registry keys mounts by
 * `(creator, name)`, so a package only ever sees/replaces its own widgets.
 */
function makeWidgets(creator: { kind: string } | string) {
    // The creator arrives as the descriptor object (synthesized module / jsx-runtime) or
    // already-stringified (inline augmentation); normalize to the JSON string the ops key on,
    // matching the alias/trigger creator convention in smudgy.ts.
    const creatorJson = typeof creator === "string" ? creator : JSON.stringify(creator);
    // This isolate's routing token, read once here and tagged onto button callbacks so `core`
    // dispatches an `onPress` back into the creating isolate (see op_smudgy_widget_isolate_token).
    const isolateToken = op_smudgy_widget_isolate_token();
    const Column = (props: Record<string, any>, children: any) =>
        op_smudgy_widget_build_column(buildChildList(children), props || {});

    const Row = (props: Record<string, any>, children: any) =>
        op_smudgy_widget_build_row(buildChildList(children), props || {});

    const Stack = (props: Record<string, any>, children: any) =>
        op_smudgy_widget_build_stack(buildChildList(children), props || {});

    const Container = (props: Record<string, any>, children: any) =>
        op_smudgy_widget_build_container(props || {}, firstChild(children));

    const Text = (props: Record<string, any>, children: any) =>
        op_smudgy_widget_build_text(props || {}, textParts(children));

    const ProgressBar = (props: Record<string, any>, _children?: any) =>
        op_smudgy_widget_build_progress_bar(props || {});

    const Scrollable = (props: Record<string, any>, children: any) =>
        op_smudgy_widget_build_scrollable(props || {}, firstChild(children));

    const Modal = (props: Record<string, any>, children: any) =>
        op_smudgy_widget_build_modal(props || {}, firstChild(children), isolateToken);

    const TextEditor = (props: Record<string, any>, _children?: any) => {
        // The editor's buffer is stateful UI-side; a live binding cannot drive it. Loud,
        // because silently ignoring the token would read as "binding is broken".
        if (isBindingToken((props || {}).value)) {
            throw new TypeError(
                "widgets: TextEditor value cannot use a store binding -- pass a string " +
                    "and track edits via onChange",
            );
        }
        return op_smudgy_widget_build_text_editor(props || {}, isolateToken);
    };

    const Markdown = (props: Record<string, any>, children: any) => {
        const p = props || {};
        const kids = normalizeChildren(children);
        // Markdown source is parsed + interned once per distinct document; a live binding in
        // the source would re-parse per store write. Loud rather than "[object Object]".
        if (kids.some(isBindingToken)) {
            throw new TypeError(
                "widgets: Markdown content cannot use store bindings -- render bound values " +
                    "with Text, or re-mount the widget from a watch()",
            );
        }
        const content = kids.map((c: any) => String(c)).join("");
        // Default link handler: send the clicked URL to the current session as if the user typed
        // it (smudgy:core `send`). Routing through op_smudgy_session_send enforces the `session`
        // capability, so a package without it sees the same NotCapable error a direct send(url)
        // would throw. Inlined rather than importing smudgy:core to stay isolate-agnostic:
        // op_smudgy_session_send(op_smudgy_get_current_session(), url) IS what send(url) does.
        const onLink =
            p.onLink ||
            ((url: string) => op_smudgy_session_send(op_smudgy_get_current_session(), url));
        return op_smudgy_widget_build_markdown({ ...p, onLink }, content, isolateToken);
    };

    const Button = (props: Record<string, any>, children: any) => {
        const kids = normalizeChildren(children);
        // A single non-text child is an Element; otherwise render the kids (strings and
        // binding tokens alike) as a text label.
        const child =
            kids.length === 1 && typeof kids[0] !== "string" && !isBindingToken(kids[0])
                ? kids[0]
                : op_smudgy_widget_build_text({}, textParts(kids));
        return op_smudgy_widget_build_button(props || {}, child, isolateToken);
    };

    const MapView = (_props?: Record<string, any>, _children?: any) =>
        op_smudgy_widget_build_map_view();

    const Space = (props: Record<string, any>, _children?: any) =>
        op_smudgy_widget_build_space(props || {});

    const Checkbox = (props: Record<string, any>, children: any) => {
        const p = props || {};
        // The host delivers the new checked state as the string "true"/"false"; hand the
        // script a real boolean.
        const onToggle =
            typeof p.onToggle === "function"
                ? (raw: string) => p.onToggle(raw === "true")
                : undefined;
        return op_smudgy_widget_build_checkbox({ ...p, onToggle }, textParts(children), isolateToken);
    };

    const Radio = (props: Record<string, any>, children: any) => {
        const p = props || {};
        // Required: iced renders a radio enabled whenever it has a click handler, and the
        // host must always give it one -- a silently inert-but-clickable radio would read
        // as broken. Display-only one-of-N markers belong to Text/Canvas.
        if (typeof p.onSelect !== "function") {
            throw new TypeError("widgets: Radio requires an onSelect handler");
        }
        if (p.value === undefined || p.value === null) {
            throw new TypeError("widgets: Radio requires a value");
        }
        // Selection compares by string spelling: a static `selected` coerces exactly like
        // `value` does (so selected={5} matches value={5}); binding tokens pass through for
        // the host to resolve with the same spelling.
        const selected =
            p.selected === undefined || p.selected === null || isBindingToken(p.selected)
                ? p.selected
                : String(p.selected);
        return op_smudgy_widget_build_radio(
            { ...p, value: String(p.value), selected },
            textParts(children),
            isolateToken,
        );
    };

    const Tooltip = (props: Record<string, any>, children: any) => {
        const p = props || {};
        const target = firstChild(children);
        // Conditional-friendly: a false/absent tip renders the target bare, so
        // tip={cond && "hint"} toggles the tooltip without restructuring the tree.
        if (p.tip === undefined || p.tip === null || p.tip === false) {
            return target;
        }
        // A string/binding tip becomes a text run inside the tooltip's themed chrome; an
        // element tip passes through chrome-free (the author's element is all there is).
        const tipIsElement = typeof p.tip === "object" && !isBindingToken(p.tip);
        const tip = elementOrText(p.tip, "a Tooltip tip");
        const { tip: _tipProp, ...rest } = p;
        return op_smudgy_widget_build_tooltip({ ...rest, tip_chrome: !tipIsElement }, target, tip);
    };

    // A cell/header slot: null/undefined is an empty cell (handy for conditional cells);
    // everything else classifies through the shared element-or-text rule.
    const contentElement = (value: any, slot: string) => {
        if (value === undefined || value === null) {
            return op_smudgy_widget_build_text({}, []);
        }
        return elementOrText(value, slot);
    };

    const Table = (props: Record<string, any>, _children?: any) => {
        const p = props || {};
        if (!Array.isArray(p.columns) || p.columns.length === 0) {
            throw new TypeError("widgets: Table requires a non-empty columns array");
        }
        const columnCount = p.columns.length;
        const headers = op_smudgy_widget_build_element_list();
        for (const column of p.columns) {
            const header = column === null || column === undefined ? undefined : column.header;
            op_smudgy_widget_push_element(headers, contentElement(header, "a Table header"));
        }
        const cells = op_smudgy_widget_build_element_list();
        const rows = p.rows === undefined || p.rows === null ? [] : p.rows;
        if (!Array.isArray(rows)) {
            throw new TypeError("widgets: Table rows must be an array of cell arrays");
        }
        for (const row of rows) {
            if (!Array.isArray(row)) {
                throw new TypeError("widgets: each Table row must be an array of cells");
            }
            // Overflow throws: with row-major cell indexing, extra cells would silently
            // shift every later row one column left. Short rows pad with empty cells,
            // loudly -- silent misalignment is worse than noise.
            if (row.length > columnCount) {
                throw new TypeError(
                    "widgets: a Table row has " + row.length + " cells but there are only " +
                        columnCount + " columns",
                );
            }
            if (row.length < columnCount) {
                console.warn(
                    "widgets: a Table row has " + row.length + " cells; padding to " +
                        columnCount + " columns",
                );
            }
            for (let i = 0; i < columnCount; i++) {
                op_smudgy_widget_push_element(cells, contentElement(row[i], "a Table cell"));
            }
        }
        // Rows ride in the cells list, never in the prop bag; `columns` passes through
        // whole -- the op reads only its layout keys (width/align_x/align_y) and ignores
        // `header`, whose elements ride positionally in the headers list.
        const { rows: _rowsProp, ...rest } = p;
        return op_smudgy_widget_build_table(rest, headers, cells);
    };

    const Canvas = (props: Record<string, any>, _children?: any) => {
        const p = props || {};
        // The host delivers pointer events as one JSON string argument; hand the script a
        // real object so the contract's CanvasPointerEvent is what the callback sees.
        const onPointer =
            typeof p.onPointer === "function"
                ? (raw: string) => p.onPointer(JSON.parse(raw))
                : undefined;
        return op_smudgy_widget_build_canvas({ ...p, onPointer }, isolateToken);
    };

    // Fragment: no iced analog -- it just yields its children for the parent to absorb
    // (via `normalizeChildren`'s flatten). It is a component `type` like the others.
    const Fragment = (_props: Record<string, any>, children: any) => children;

    // The automatic JSX runtime calls `jsx(type, props, key?)` (0/1-child) and
    // `jsxs(type, props, key?)` (2+-children). `type` is always a widget component
    // function (smudgy has no intrinsic/string host tags). We strip `key` + `children`
    // from the forwarded props and pass the normalized children array as the 2nd arg.
    function jsx(type: any, props: Record<string, any>) {
        const p = props || {};
        const rest: Record<string, any> = {};
        for (const k in p) {
            if (k !== "children" && k !== "key") rest[k] = p[k];
        }
        if (typeof type !== "function") {
            throw new Error(
                "widgets: this JSX targeted smudgy:widgets but the element type is not a " +
                    "smudgy widget component -- third-party React JSX must set " +
                    "/** @jsxImportSource react */",
            );
        }
        return type(rest, normalizeChildren(p.children));
    }
    // jsxs has the identical contract here (children is already an array; normalizeChildren handles it).
    const jsxs = jsx;

    // createWidget upserts a named on-screen widget. A top-level Fragment yields an array
    // of children, which `createWidget` collapses into an implicit Column (the documented
    // rule), since the mount op takes a single root element. `options.pane` mounts into that
    // pane's widget stack (over the terminal on terminal panes, the whole body on
    // widgets-only panes); omitted, the widget overlays the session's main pane as before
    // (-1 = no target on the wire).
    const createWidget = (name: string, element: any, options?: { pane?: any }) => {
        const root = Array.isArray(element) ? Column({}, element) : element;
        const pane = options === undefined || options === null ? undefined : options.pane;
        const target = pane === undefined || pane === null ? -1 : resolvePaneTarget(pane);
        op_smudgy_widget_create(creatorJson, String(name), root, target);
    };

    const removeWidget = (name: string) => {
        op_smudgy_widget_remove(creatorJson, String(name));
    };

    return {
        createWidget,
        removeWidget,
        extractMarkdownLinks,
        Column,
        Row,
        Stack,
        Container,
        Text,
        ProgressBar,
        Scrollable,
        Markdown,
        Modal,
        TextEditor,
        Button,
        MapView,
        Canvas,
        Space,
        Checkbox,
        Radio,
        Tooltip,
        Table,
        jsx,
        jsxs,
        Fragment,
    };
}

Object.defineProperty(globalThis, "__smudgy_make_widgets", { value: makeWidgets });

// Inline alias/trigger bodies run inside `with (globalThis.__smudgy_user_api) { ... }`.
// `smudgy.ts` (the `smudgy_ops` extension entrypoint) runs before this one and builds
// that object, so augment it here with the user-creator builder surface. Inline bodies
// are not modules and cannot use JSX, so we expose the builder factories +
// createWidget/removeWidget, not jsx/jsxs/Fragment.
if ((globalThis as any).__smudgy_user_api) {
    const w = makeWidgets({ kind: "user" });
    Object.assign((globalThis as any).__smudgy_user_api, {
        createWidget: w.createWidget,
        removeWidget: w.removeWidget,
        extractMarkdownLinks: w.extractMarkdownLinks,
        Column: w.Column,
        Row: w.Row,
        Stack: w.Stack,
        Container: w.Container,
        Text: w.Text,
        ProgressBar: w.ProgressBar,
        Scrollable: w.Scrollable,
        Markdown: w.Markdown,
        Modal: w.Modal,
        TextEditor: w.TextEditor,
        Button: w.Button,
        MapView: w.MapView,
        Canvas: w.Canvas,
        Space: w.Space,
        Checkbox: w.Checkbox,
        Radio: w.Radio,
        Tooltip: w.Tooltip,
        Table: w.Table,
    });
}
