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
/** One child or a flat array of children accepted by a widget component. */
type WidgetChildren = WidgetChild | WidgetChild[];

declare module "smudgy:widgets" {
    import type { Binding, Pane } from "smudgy:core";

    // Re-export the shared element/children/length types for module consumers.
    /** An element returned by a widget component. */
    export type Element = SmudgyElement;
    /** A widget size: pixels, `"fill"`, or `"shrink"`. */
    export type Length = WidgetLength;
    /** The values accepted as component children. */
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
    /** The same layout properties as {@link ColumnProps}. */
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

    /** Props for empty layout space (a leaf; children are ignored). Place
     *  `<Space width="fill"/>` between Row children to create a flexible gap. */
    export interface SpaceProps {
        /** Default "shrink". */
        width?: Bindable<WidgetLength>;
        /** Default "shrink". */
        height?: Bindable<WidgetLength>;
        children?: WidgetChildren;
    }

    /** Props for a checkbox. Its children form the label and may include bindings.
     *
     *  A checkbox displays the value supplied through `checked`. To make it respond
     *  visibly to a click, bind `checked` to state and update that state from
     *  `onToggle`:
     *
     *  ```tsx
     *  import { createState } from "smudgy:core";
     *  import { Checkbox } from "smudgy:widgets";
     *
     *  const cfg = createState<{ autoloot: boolean }>("preferences");
     *  cfg.set({ autoloot: false });
     *
     *  export const autolootControl = (
     *    <Checkbox checked={cfg.bind("autoloot")}
     *              onToggle={(checked) => { cfg.value.autoloot = checked; }}>
     *      Autoloot
     *    </Checkbox>
     *  );
     *  ```
     *
     *  Without `onToggle`, the checkbox is disabled and can be used as a read-only
     *  indicator. If `checked` is a fixed value, a click still calls `onToggle`, but
     *  the displayed value changes only when the caller supplies a different value. */
    export interface CheckboxProps {
        /** Whether the box is checked. Default false. */
        checked?: Bindable<boolean>;
        /** Called with the new state on click. Omitted, the checkbox renders disabled. */
        onToggle?: (checked: boolean) => void;
        /** Box size in pixels. */
        size?: Bindable<number>;
        /** Label text size in pixels. */
        text_size?: Bindable<number>;
        children?: WidgetChildren;
    }

    /** Props for one radio button. Its children form the label.
     *
     *  Smudgy does not provide a separate RadioGroup component. Radios that read and
     *  update the same `selected` state behave as one group, even when they appear in
     *  different layouts:
     *
     *  ```tsx
     *  import { createState } from "smudgy:core";
     *  import { Radio, Row } from "smudgy:widgets";
     *
     *  const cfg = createState<{ mode: string }>("preferences");
     *  cfg.set({ mode: "fast" });
     *  const selectMode = (mode: string) => { cfg.value.mode = mode; };
     *
     *  export const modeControls = (
     *    <Row>
     *      <Radio value="fast" selected={cfg.bind("mode")} onSelect={selectMode}>Fast</Radio>
     *      <Radio value="careful" selected={cfg.bind("mode")} onSelect={selectMode}>Careful</Radio>
     *    </Row>
     *  );
     *  ``` */
    export interface RadioProps {
        /** This radio's own value. Selection compares it (as a string) against
         *  `selected`. */
        value: string | number;
        /** The currently selected value. This radio renders selected when it equals
         *  `value` by string spelling (numbers compare as their decimal text). */
        selected?: Bindable<string | number>;
        /** Called with this radio's `value` on click. Required -- a radio without a
         *  handler would render clickable and do nothing; use Text for display-only
         *  markers. */
        onSelect: (value: string) => void;
        /** Dot size in pixels. */
        size?: Bindable<number>;
        /** Label text size in pixels. */
        text_size?: Bindable<number>;
        children?: WidgetChildren;
    }

    /** Where a tooltip appears relative to its target. */
    export type TooltipPosition = "top" | "bottom" | "left" | "right" | "cursor";

    /** Props for a hover tooltip. The first child is the hover target. A string,
     *  number, or binding renders in the standard tooltip style. An element uses the
     *  styles declared by that element. */
    export interface TooltipProps {
        /** The tooltip content. A `false` or null value suppresses the tooltip, so a
         *  conditional expression such as `tip={cond && "hint"}` is supported. */
        tip: string | number | Binding<any> | SmudgyElement | false | null;
        /** Which side of the target the tip appears on, or `"cursor"` to follow the
         *  pointer. Default "top". */
        position?: TooltipPosition;
        /** Distance between target and tip, in pixels. */
        gap?: Bindable<number>;
        children?: WidgetChildren;
    }

    /** One value in a table cell. Elements, text, numbers, and bindings display as
     *  content. Null, undefined, and `false` produce an empty cell; `true` displays as
     *  text. */
    export type TableCell =
        | SmudgyElement
        | string
        | number
        | boolean
        | Binding<any>
        | null
        | undefined;

    /** One table column: its header plus optional layout. */
    export interface TableColumnSpec {
        /** The header cell: text or an element. */
        header?: TableCell;
        /** Column width. Default "shrink". */
        width?: WidgetLength;
        /** Horizontal alignment of the column's cells. Default "left". */
        align_x?: HorizontalAlign;
        /** Vertical alignment of the column's cells. Default "top". */
        align_y?: VerticalAlign;
    }

    /** Props for a data table (a leaf; children are ignored).
     *
     *  Supply each row as an array in column order. A bound cell repaints when its value
     *  changes. Re-mount the widget when rows are added, removed, or reordered. A row with
     *  more cells than columns is invalid; a shorter row is padded with empty cells. Wrap
     *  a tall table in `Scrollable`. */
    export interface TableProps {
        /** The columns, in order. Required and non-empty. */
        columns: TableColumnSpec[];
        /** The rows, each an array of cells in column order. */
        rows?: TableCell[][];
        width?: Bindable<WidgetLength>;
        /** Cell padding in pixels, both axes. */
        padding?: number;
        /** Separator line thickness in pixels, both axes. */
        separator?: number;
        children?: WidgetChildren;
    }

    // ---- Canvas ------------------------------------------------------------------

    /** A paint for canvas shapes: a CSS color string, or a linear gradient between two
     *  scene-space points. Gradient endpoints follow the canvas `view_box` mapping and any
     *  group transforms, like the geometry they fill. At most 8 stops. */
    export type CanvasFill =
        | string
        | {
              gradient: {
                  /** The gradient's start point, in scene coordinates. */
                  from: [number, number];
                  /** The gradient's end point, in scene coordinates. */
                  to: [number, number];
                  /** Color stops: `[offset, color]` with offsets in 0..=1, at most 8. */
                  stops: [number, string][];
              };
          };

    /** A stroke for canvas shapes. */
    export interface CanvasStroke {
        /** Stroke paint. Default black. */
        color?: CanvasFill;
        /** Stroke width in scene units. Default 1. */
        width?: number;
        /** Dash pattern (on/off lengths). Solid when omitted. */
        dash?: number[];
    }

    /** An animation easing curve. */
    export type CanvasEase = "linear" | "in" | "out" | "in-out";

    /** A tween for one numeric field of a canvas shape. */
    export interface NumberTween {
        /** The value animated to. */
        to: number;
        /** The starting value. Defaults to the shape's own value for the field. */
        from?: number;
        /** Duration of one run, in milliseconds. */
        duration: number;
        /** Delay before the first run, in milliseconds (applied once, not per repeat). */
        delay?: number;
        /** Easing curve. Default "linear". */
        ease?: CanvasEase;
        /** Run the tween this many times, or forever. Each repeat restarts from the
         *  beginning. Default 1. */
        repeat?: number | "infinite";
    }

    /** A tween for one color field of a canvas shape. Endpoints are CSS color strings. */
    export interface ColorTween {
        to: string;
        /** Defaults to the shape's own color for the field. */
        from?: string;
        duration: number;
        delay?: number;
        ease?: CanvasEase;
        repeat?: number | "infinite";
    }

    /** Fields shared by every canvas shape.
     *
     *  Give an animated shape an `id` that is unique within its scene. When a bound scene
     *  changes or the widget is re-mounted, an animation keeps its progress if both its
     *  `id` and animation specification are unchanged. Changing the specification restarts
     *  the animation. Without an `id`, Smudgy identifies the animation by the shape's
     *  position in the scene, so reordering shapes may restart it. */
    interface CanvasShapeBase {
        /** Stable identity for this shape's animations across scene rewrites. Must be
         *  unique among animated shapes within one scene. */
        id?: string;
        /** Overall opacity, 0..=1. Default 1. */
        opacity?: number;
        /** When used with `animate`, removes the shape after every tween finishes. Later
         *  writes that still contain the same shape do not display it again. Remove the
         *  shape from subsequent scene values after a one-shot effect completes. */
        transient?: boolean;
    }

    /** A rectangle, optionally rounded. */
    export interface CanvasRect extends CanvasShapeBase {
        kind: "rect";
        x?: number;
        y?: number;
        width?: number;
        height?: number;
        /** Corner radius. */
        rx?: number;
        fill?: CanvasFill;
        stroke?: CanvasStroke;
        animate?: Partial<
            Record<"x" | "y" | "width" | "height" | "rx" | "opacity" | "stroke_width", NumberTween>
        > & { fill?: ColorTween; stroke?: ColorTween };
    }

    /** A circle. */
    export interface CanvasCircle extends CanvasShapeBase {
        kind: "circle";
        cx?: number;
        cy?: number;
        r?: number;
        fill?: CanvasFill;
        stroke?: CanvasStroke;
        animate?: Partial<
            Record<"cx" | "cy" | "r" | "opacity" | "stroke_width", NumberTween>
        > & { fill?: ColorTween; stroke?: ColorTween };
    }

    /** An axis-aligned ellipse. */
    export interface CanvasEllipse extends CanvasShapeBase {
        kind: "ellipse";
        cx?: number;
        cy?: number;
        rx?: number;
        ry?: number;
        fill?: CanvasFill;
        stroke?: CanvasStroke;
        animate?: Partial<
            Record<"cx" | "cy" | "rx" | "ry" | "opacity" | "stroke_width", NumberTween>
        > & { fill?: ColorTween; stroke?: ColorTween };
    }

    /** A line segment. */
    export interface CanvasLine extends CanvasShapeBase {
        kind: "line";
        x1?: number;
        y1?: number;
        x2?: number;
        y2?: number;
        stroke?: CanvasStroke;
        animate?: Partial<
            Record<"x1" | "y1" | "x2" | "y2" | "opacity" | "stroke_width", NumberTween>
        > & { stroke?: ColorTween };
    }

    /** An open run of connected line segments. */
    export interface CanvasPolyline extends CanvasShapeBase {
        kind: "polyline";
        /** The vertices, as `[x, y]` pairs. */
        points: [number, number][];
        stroke?: CanvasStroke;
        animate?: Partial<Record<"opacity" | "stroke_width", NumberTween>> & {
            stroke?: ColorTween;
        };
    }

    /** A closed polygon. */
    export interface CanvasPolygon extends CanvasShapeBase {
        kind: "polygon";
        /** The vertices, as `[x, y]` pairs. The shape closes itself. */
        points: [number, number][];
        fill?: CanvasFill;
        stroke?: CanvasStroke;
        animate?: Partial<Record<"opacity" | "stroke_width", NumberTween>> & {
            fill?: ColorTween;
            stroke?: ColorTween;
        };
    }

    /** An arbitrary path in SVG path-data syntax. */
    export interface CanvasPath extends CanvasShapeBase {
        kind: "path";
        /** SVG path data (`M`/`L`/`H`/`V`/`C`/`S`/`Q`/`T`/`A`/`Z`, absolute and relative). */
        d: string;
        fill?: CanvasFill;
        stroke?: CanvasStroke;
        animate?: Partial<Record<"opacity" | "stroke_width", NumberTween>> & {
            fill?: ColorTween;
            stroke?: ColorTween;
        };
    }

    /** Text drawn in the canvas.
     *
     *  Canvas text is drawn after non-text shapes, regardless of scene order. A later
     *  rectangle therefore cannot cover earlier text. */
    export interface CanvasText extends CanvasShapeBase {
        kind: "text";
        x?: number;
        y?: number;
        /** The text content. */
        text: string;
        /** Text size in scene units. Default 16. */
        size?: number;
        /** A CSS color string. Default white. */
        color?: string;
        /** Which part of the text sits at `x`. Default "left". */
        align_x?: HorizontalAlign;
        /** Which part of the text sits at `y`. Default "top". */
        align_y?: VerticalAlign;
        /** Font family. Default the UI font. */
        font?: "default" | "monospace";
        animate?: Partial<Record<"x" | "y" | "size" | "opacity", NumberTween>> & {
            color?: ColorTween;
        };
    }

    /** A transformed group of shapes. Transform components always apply in the order
     *  translate, then rotate, then scale, about the group's local origin. */
    export interface CanvasGroup extends CanvasShapeBase {
        kind: "group";
        transform?: {
            translate?: [number, number];
            /** Rotation in degrees, clockwise. */
            rotate?: number;
            /** A uniform factor, or `[sx, sy]`. */
            scale?: number | [number, number];
        };
        children: CanvasShape[];
        animate?: Partial<
            Record<"translate_x" | "translate_y" | "rotate" | "scale", NumberTween>
        >;
    }

    /** One record in a canvas scene. Shapes draw in scene order, back to front (except
     *  text; see {@link CanvasText}). */
    export type CanvasShape =
        | CanvasRect
        | CanvasCircle
        | CanvasEllipse
        | CanvasLine
        | CanvasPolyline
        | CanvasPolygon
        | CanvasPath
        | CanvasText
        | CanvasGroup;

    /** A pointer event on a canvas, in scene coordinates (the same numbers you draw
     *  with). `down` and `up` always arrive in pairs; `move` only arrives while a button
     *  is held, at most once per frame, and the release is delivered even when it happens
     *  outside the canvas. */
    export interface CanvasPointerEvent {
        kind: "down" | "move" | "up";
        x: number;
        y: number;
        button: "left" | "middle" | "right";
    }

    /** Props for a script-drawn canvas (a leaf; children are ignored).
     *
     *  A scene is an array of shape records. When `scene` is a binding, each write to the
     *  bound path repaints the drawing, including changes to the number or order of shapes.
     *  Smudgy evaluates declared animations between writes; scripts do not need to submit
     *  an update for each frame.
     *
     *  Smudgy validates the whole scene before displaying it. If the scene exceeds a
     *  complexity limit or contains duplicate animation IDs, Smudgy reports an error and
     *  continues displaying the previous valid scene.
     *
     *  Drawing is clipped to the canvas bounds, including animated shapes. A canvas without
     *  `onPointer` does not capture pointer input from content behind it. With a `view_box`,
     *  a `"fill"`-sized canvas rescales the scene when its pane changes size. Fixed numeric
     *  dimensions keep a fixed widget size. Without a `view_box`, scene units are pixels. */
    export interface CanvasProps {
        /** Default "fill". */
        width?: Bindable<WidgetLength>;
        /** Default "fill". */
        height?: Bindable<WidgetLength>;
        /** The scene rectangle `[x, y, width, height]` mapped onto the widget's bounds.
         *  Scene coordinates (and pointer events) then stay resolution-independent.
         *  Omitted, scene units are pixels. See {@link CanvasProps.fit} for how the
         *  mapping treats a mismatched aspect ratio. */
        view_box?: [number, number, number, number];
        /** How the `view_box` meets the widget bounds when their aspect ratios differ.
         *  `"fill"` (default) stretches the scene to cover the bounds exactly;
         *  `"contain"` scales uniformly to the limiting axis and centers, keeping the
         *  scene's aspect ratio with empty margins (a pointer event in a margin reports
         *  coordinates outside the `view_box`). Without a `view_box`, `fit` is ignored. */
        fit?: "fill" | "contain";
        /** The shapes to draw, in paint order. */
        scene?: Bindable<CanvasShape[]>;
        /** Pointer input, in scene coordinates. Omitted, the canvas is display-only. */
        onPointer?: (event: CanvasPointerEvent) => void;
        children?: WidgetChildren;
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
    /** Draws shape records and runs their declared animations. */
    export function Canvas(props?: CanvasProps, children?: WidgetChildren): SmudgyElement;
    /** Empty layout space (use `width="fill"` as a flexible spacer). */
    export function Space(props?: SpaceProps, children?: WidgetChildren): SmudgyElement;
    /** A checkbox. Children are the label. */
    export function Checkbox(props?: CheckboxProps, children?: WidgetChildren): SmudgyElement;
    /** One radio button; radios sharing a `selected` source form a group. */
    export function Radio(props: RadioProps, children?: WidgetChildren): SmudgyElement;
    /** A hover tooltip around its single child. */
    export function Tooltip(props: TooltipProps, children?: WidgetChildren): SmudgyElement;
    /** A data table: columns as records, rows as arrays of cells. */
    export function Table(props: TableProps, children?: WidgetChildren): SmudgyElement;

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
        Canvas: typeof Canvas;
        Space: typeof Space;
        Checkbox: typeof Checkbox;
        Radio: typeof Radio;
        Tooltip: typeof Tooltip;
        Table: typeof Table;
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
