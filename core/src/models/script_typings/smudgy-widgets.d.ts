// =============================================================================
//  smudgy:widgets -- TypeScript declarations  (GENERATED -- DO NOT EDIT)
// =============================================================================
//  smudgy writes and overwrites this file every time a session starts. It teaches
//  VS Code (and any TypeScript-aware editor) about the script-driven UI surface:
//    - `declare module "smudgy:widgets"`           the factories + createWidget;
//    - `declare module "smudgy:widgets/jsx-runtime"` the automatic-JSX runtime
//      (jsx/jsxs/Fragment) + the `JSX` namespace, so `.tsx` widget authoring
//      type-checks. The editor tsconfig sets `jsx: "react-jsx"` +
//      `jsxImportSource: "smudgy:widgets"`, so `<Column/>` desugars to imports
//      from `smudgy:widgets/jsx-runtime`.
//
//  smudgy has NO intrinsic (string-tag) elements -- every JSX tag is one of the
//  widget component functions below, so `JSX.IntrinsicElements` is empty.
//
//  Edits here are lost on the next launch.
// =============================================================================

// ---- Shared (global ambient) types ------------------------------------------

/**
 * One piece of on-screen UI, returned by every component below. Build it,
 * nest it as a child of another component, or mount it with `createWidget`;
 * it is not inspectable from JS.
 */
interface SmudgyElement {
    /** @internal opaque brand -- do not access. */
    readonly __smudgyWidgetElement: true;
}

/** A size: a number of pixels, `"fill"` (take all available space), or
 *  `"shrink"` (hug the contents). */
type WidgetLength = number | "fill" | "shrink";

/** Anything a component accepts as a child: another element, text (a string or
 *  number), a state binding (rendered as live text -- see `bind()` in
 *  `smudgy:core`), or `null`/`undefined`/`false` (dropped, so `cond && <Text/>`
 *  works), plus arrays of the above (flattened). */
type WidgetChild =
    | SmudgyElement
    | string
    | number
    | boolean
    | null
    | undefined
    | import("smudgy:core").Binding<any>;
type WidgetChildren = WidgetChild | WidgetChild[];

declare module "smudgy:widgets" {
    import type { Binding, Pane } from "smudgy:core";

    // Re-export the shared element/children/length types for module consumers.
    export type Element = SmudgyElement;
    export type Length = WidgetLength;
    export type Children = WidgetChildren;

    /** A prop that takes a value or a live state binding (`handle.bind(...)`
     *  from `smudgy:core`). Bound props track the published value on their
     *  own -- the widget repaints on each update, with no handler and no
     *  re-mount. */
    export type Bindable<T> = T | Binding<T>;

    /** Horizontal alignment within a container. */
    export type HorizontalAlign = "left" | "start" | "center" | "right" | "end";
    /** Vertical alignment within a container. */
    export type VerticalAlign = "top" | "start" | "center" | "bottom" | "end";

    /** Props common to the linear layout containers. */
    export interface ColumnProps {
        width?: Bindable<WidgetLength>;
        height?: Bindable<WidgetLength>;
        /** Gap between children, in pixels. */
        spacing?: Bindable<number>;
        /** Padding around the children, in pixels. */
        padding?: Bindable<number>;
        children?: WidgetChildren;
    }
    export type RowProps = ColumnProps;

    /** Props for the layering container (children stack front-to-back). */
    export interface StackProps {
        width?: Bindable<WidgetLength>;
        height?: Bindable<WidgetLength>;
        children?: WidgetChildren;
    }

    /** Props for the single-child wrapper. Only the first child is used. */
    export interface ContainerProps {
        width?: Bindable<WidgetLength>;
        height?: Bindable<WidgetLength>;
        align_x?: HorizontalAlign;
        align_y?: VerticalAlign;
        /** A CSS color string for the background. */
        background?: Bindable<string>;
        children?: WidgetChildren;
    }

    /** Props for a run of text. The children are concatenated as the text content. */
    export interface TextProps {
        /** A CSS color string. */
        color?: Bindable<string>;
        /** Text size in pixels. */
        size?: Bindable<number>;
        children?: WidgetChildren;
    }

    /** Props for a progress/health bar (a leaf -- children are ignored). */
    export interface ProgressBarProps {
        /** Range minimum (default 0). */
        min?: Bindable<number>;
        /** Range maximum (default 100). */
        max?: Bindable<number>;
        /** Current value, clamped to [min, max] (default 0). */
        value?: Bindable<number>;
        /** A CSS color string for the track background. */
        background?: Bindable<string>;
        /** A CSS color string for the filled bar. */
        color?: Bindable<string>;
        width?: Bindable<WidgetLength>;
        height?: Bindable<WidgetLength>;
        /** Render vertically (width/height are swapped). Default false. */
        vertical?: boolean;
    }

    /** A button emphasis variant, mapping to the theme's named button styles. */
    export type ButtonVariant = "primary" | "secondary" | "subtle" | "link";

    /** Props for a clickable button. A single non-text child is used as the label element;
     *  otherwise the text children are rendered as the label. */
    export interface ButtonProps {
        width?: Bindable<WidgetLength>;
        height?: Bindable<WidgetLength>;
        /** Emphasis style. Default "subtle". */
        variant?: ButtonVariant;
        /** Called when the button is pressed. */
        onPress?: () => void;
        children?: WidgetChildren;
    }

    /** Props for a multi-line text editor (a leaf -- children are ignored). */
    export interface TextEditorProps {
        /** A stable identity for the editing buffer: two editors with different
         *  ids edit independently. Omitted, sibling editors are still kept
         *  distinct. */
        id?: string;
        /** The editor's starting text. In-progress edits are preserved; a
         *  script reload resets the editor to `value`. */
        value?: string;
        /** Called with the full text on each edit (not on cursor/selection movements). */
        onChange?: (text: string) => void;
        /** Placeholder shown when empty. */
        placeholder?: string;
        /** Viewport height. When set, taller text scrolls. Unset, the editor
         *  grows to fit its content. */
        height?: WidgetLength;
        /** Padding around the text, in pixels. */
        padding?: number;
        /** Text size in pixels. */
        size?: number;
        children?: WidgetChildren;
    }

    /** Props for a modal: a dimmed, input-blocking backdrop under a centered content box. The
     *  single child is the content box (style it with a Container). */
    export interface ModalProps {
        /** Called when the backdrop is clicked. If omitted, the backdrop blocks
         *  input but does not dismiss. */
        onDismiss?: () => void;
        /** A CSS color string for the backdrop. Default translucent black. */
        background?: Bindable<string>;
        children?: WidgetChildren;
    }

    /** Props for the map view (a leaf -- props and children are ignored). */
    export interface MapViewProps {
        children?: WidgetChildren;
    }

    /** A scroll axis. Vertical by default. */
    export type ScrollDirection = "vertical" | "horizontal" | "both";

    /** Props for a scrollable single-child viewport. Only the first child is used. */
    export interface ScrollableProps {
        width?: Bindable<WidgetLength>;
        height?: Bindable<WidgetLength>;
        /** Scroll axis. Default "vertical". */
        direction?: ScrollDirection;
        /** Where the view rests. "end" sticks to the bottom (or right, when horizontal) so
         *  growing content keeps its newest line visible. Default "start". */
        anchor?: "start" | "end";
        children?: WidgetChildren;
    }

    /** Props for a rendered Markdown document. The children are concatenated as the source.
     *
     *  Styling follows the terminal color scheme. Links render as clickable
     *  command chips, and code renders monospace on a panel; a fenced block
     *  whose opening fence names a language (like js) is syntax-highlighted.
     *
     *  Links can stand in for MUD commands, two ways:
     *  - Command autolink: a bare `<command>` renders as a command link that sends that text, so
     *    `<go north>` sends `go north` and `<look>` sends `look`. It is shorthand for
     *    `[go north](<go north>)`. Works for word and multi-word commands; a command containing
     *    `=`, `/`, quotes, or other non-word punctuation (e.g. `<say hi!>`) is left as literal
     *    text; use the explicit form below for those (it also lets the visible label differ from
     *    the sent text).
     *  - Explicit link: `[label](destination)`. A bare destination cannot contain spaces
     *    (`[north gate](go north gate)` will not parse), so wrap a spaced destination in angle
     *    brackets: `[north gate](<go north gate>)` sends `go north gate`. The angle-bracket wrapper
     *    may not itself contain `<`, `>`, or a newline.
     *
     *  Real URLs (`<http://...>`) stay ordinary links, and inline `code` / fenced code blocks are
     *  left literal. */
    export interface MarkdownProps {
        /** Base text size in pixels; heading sizes scale from it. Default 16.
         *  The Markdown source itself cannot be bound (it is parsed once);
         *  render live values with `Text`. */
        size?: Bindable<number>;
        /** Called with a link's URL when it is clicked. Defaults to sending the
         *  URL to the current session as if typed. */
        onLink?: (url: string) => void;
        children?: WidgetChildren;
    }

    /** One link in a Markdown document, as {@link extractMarkdownLinks} reports it. */
    export interface MarkdownLink {
        /** The link's visible text. For a bare `<command>` link it equals the
         *  command; an empty label (`[](<look>)`) falls back to the destination. */
        label: string;
        /** What clicking the link sends: the explicit destination, or for a
         *  bare `<command>` link, the command itself. */
        url: string;
    }

    /** A vertical layout. Children are laid out top-to-bottom. */
    export function Column(props?: ColumnProps, children?: WidgetChildren): SmudgyElement;
    /** A horizontal layout. Children are laid out left-to-right. */
    export function Row(props?: RowProps, children?: WidgetChildren): SmudgyElement;
    /** A layering layout. Children are stacked front-to-back. */
    export function Stack(props?: StackProps, children?: WidgetChildren): SmudgyElement;
    /** A single-child wrapper with alignment/background. Only the first child is used. */
    export function Container(props?: ContainerProps, children?: WidgetChildren): SmudgyElement;
    /** A run of (optionally colored) text. */
    export function Text(props?: TextProps, children?: WidgetChildren): SmudgyElement;
    /** A progress/health bar. */
    export function ProgressBar(props?: ProgressBarProps, children?: WidgetChildren): SmudgyElement;
    /** A scrollable single-child viewport. Only the first child is used. */
    export function Scrollable(props?: ScrollableProps, children?: WidgetChildren): SmudgyElement;
    /** A rendered Markdown document. Children are concatenated as the Markdown source. */
    export function Markdown(props?: MarkdownProps, children?: WidgetChildren): SmudgyElement;
    /** A modal: a dimmed, input-blocking backdrop under a centered single child. */
    export function Modal(props?: ModalProps, children?: WidgetChildren): SmudgyElement;
    /** A multi-line text editor. Read its text via the `onChange` callback. */
    export function TextEditor(props?: TextEditorProps, children?: WidgetChildren): SmudgyElement;
    /** A clickable button. */
    export function Button(props?: ButtonProps, children?: WidgetChildren): SmudgyElement;
    /** The map view for the current session. */
    export function MapView(props?: MapViewProps, children?: WidgetChildren): SmudgyElement;

    /** Options for {@link createWidget}. */
    export interface CreateWidgetOptions {
        /**
         * The session pane to mount into: a `Pane` handle from `smudgy:core`,
         * or a pane name (`"main"` is the main pane). Defaults to the main
         * pane.
         */
        pane?: Pane | string;
    }

    /**
     * Put a widget on screen (or replace the one already mounted under `name`).
     * Re-mounting an existing name keeps its enabled state, and moves the
     * widget when `options.pane` changes.
     */
    export function createWidget(
        name: string,
        element: SmudgyElement,
        options?: CreateWidgetOptions,
    ): void;
    /** Remove a previously-mounted named widget. */
    export function removeWidget(name: string): void;

    /**
     * The links `source` contains when read as a Markdown document, in order:
     * exactly the links a `Markdown` widget shows for it, including bare
     * `<command>` links, with backslash escapes honored and inline `code` /
     * fenced code left literal. Use it to act on the same links a widget
     * displays, like running the first link in a room's notes.
     */
    export function extractMarkdownLinks(source: string): MarkdownLink[];

    /** The default export bundles every member above. */
    interface WidgetsApi {
        Column: typeof Column;
        Row: typeof Row;
        Stack: typeof Stack;
        Container: typeof Container;
        Text: typeof Text;
        ProgressBar: typeof ProgressBar;
        Scrollable: typeof Scrollable;
        Markdown: typeof Markdown;
        Modal: typeof Modal;
        TextEditor: typeof TextEditor;
        Button: typeof Button;
        MapView: typeof MapView;
        createWidget: typeof createWidget;
        removeWidget: typeof removeWidget;
        extractMarkdownLinks: typeof extractMarkdownLinks;
    }
    const api: WidgetsApi;
    export default api;
}

declare module "smudgy:widgets/jsx-runtime" {
    /** The automatic-JSX factory for elements. */
    export function jsx(type: unknown, props: Record<string, unknown>, key?: unknown): SmudgyElement;
    /** The automatic-JSX factory for elements with multiple children. */
    export function jsxs(type: unknown, props: Record<string, unknown>, key?: unknown): SmudgyElement;
    /** Groups children with no wrapper element (`<>...</>`). */
    export function Fragment(props: { children?: WidgetChildren }): SmudgyElement;

    /** The JSX namespace TypeScript resolves for `jsxImportSource: "smudgy:widgets"`. */
    export namespace JSX {
        type Element = SmudgyElement;
        // No host string tags -- every JSX tag must be a widget component function.
        interface IntrinsicElements {}
        interface ElementChildrenAttribute {
            children: {};
        }
    }
}
