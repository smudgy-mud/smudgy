# Script widgets and inline terminal content

Scripts import UI components from `smudgy:widgets`. Use `createWidget()` for a
named UI mounted in a pane, or pass an element to `echo()` to put it in the
transcript. Both use the same components and callbacks.

```tsx
import { echo, line } from "smudgy:core";
import { Button, Container, Span, createWidget } from "smudgy:widgets";

const more = <Button onPress={() => echo("More information.")}>Details</Button>;
echo(`A clue appears. ${more}`);

createWidget("status", <Container>
    <Span fontWeight="bold">Ready</Span>
</Container>);

// Inside a trigger: replace matching source text with an inline element.
line.replace("Ready", <Span fontWeight="bold">Ready</Span>);
```

JSX requires a `.tsx` module. Builder calls such as
`Span({ fontWeight: "bold" }, "Ready")` also work without JSX.
Builders also accept `children` in their props. An explicit second child argument
takes precedence when both are supplied.

## Text and layout

`Text` is a widget label. `Span` is selectable terminal-style text that also
works as an ordinary widget. A Span accepts styled text and links, plus
`fontFace`, `fontSize`, `fontWeight` and `fontStyle`. Its typography contributes
to line layout. Plain text outside an interpolated widget also contributes its
normal line height; a widget-only line uses the widget's actual layout size.

Inline buttons, images, canvases and compound widgets reserve their own layout
space. A tall panel can increase a transcript row's height. Layout size and
paint overflow are independent: an attention canvas can reserve a small slot
while its animation paints across the containing pane.

```tsx
import { Canvas } from "smudgy:widgets";

echo(`Attention starts here ${<Canvas width={12} height={12} overflow="pane"
    terminalText="[pulse]" scene={[
        { kind: "circle", cx: 6, cy: 6, r: 4, fill: "#ff3030", opacity: 0.3,
          animate: {
              r: { to: 4096, duration: 1200, ease: "out" },
              opacity: { to: 0, duration: 1200, ease: "out" },
          }, transient: true },
    ]}/>}`);
```

Pane overflow stays inside the terminal viewport. It does not enlarge the
widget's hit target or escape into other panes or controls. Ordinary widget
mounts remain subject to their ancestors' clipping.

## Text projection and line edits

Rich output carries a text projection for selection, copy, search and logs.
Span and text effects preserve their source text. Atomic widgets can provide
`terminalText` for a useful textual representation.

`echo(element)`, template interpolation, `line.insert()`, `line.replaceAt()` and
`line.replace()` accept inline elements. Numeric line-edit positions are UTF-8
byte offsets, as with ordinary text edits. Use `line.replace()` for literal
matching when you do not need to calculate offsets. Editing or removing a
range also updates the associated inline content.

## GPU text effects

`TextEffect` shades text or one Span. Strings and styled text are shorthand for
that Span. Other widget children and nested effects are rejected: this API
captures glyphs rather than arbitrary child widget images.

```tsx
import { TextEffect } from "smudgy:widgets";
import tint from "./tint.wgsl";

echo(<TextEffect shader={tint} duration={1200}>
    <Span fontFace="serif" fontSize={24}>A warning burns brightly.</Span>
</TextEffect>);
```

Effects preserve native layout and interaction geometry even when the shader
moves or enlarges the lettering. Shader animation runs on the GPU without
per-frame JavaScript. Software renderers display ordinary text. See the
[shader authoring reference](SHADERS.md) for WGSL, playback, resource limits and
portability. A minimal `tint.wgsl` can return captured glyphs unchanged:

```wgsl
fn effect(position: vec2f) -> vec4f { return sampleText(position); }
```

## Package permissions

Sandboxed packages need `permissions.smudgy.widgets: ["create"]` to display
widgets. Importing WGSL or constructing a TextEffect additionally needs
`"shaders"`; widget creation permission does not imply shader permission.
Echoing, line editing, creating panes and registering automations retain their
own permission checks. Local user modules and trusted packages have full
access.

The editor's generated `smudgy:widgets` declarations document the complete
component and prop surface.

## Callback lifetime

Native controls send callback leases and the creating isolate's instance token.
V8 function handles remain in that isolate's registry on the script thread;
render factories, native trees and queued UI events contain no V8 handles.
Dispatch checks the instance and the lease's registry before invoking JavaScript.
An old click after reload or shutdown is inert, even if a new callback receives
the same numeric ID. Dropped widgets release their leases; unused handles are
swept at the next callback registration or during isolate teardown.

Rendering and animation never invoke these callbacks. Button, slider, checkbox,
radio, Markdown, modal, text-editor and Canvas pointer callbacks use this same
route. Stateful render caches synchronize access; Rust checks the factories'
`Send + Sync` bounds rather than relying on unsafe thread markers.
