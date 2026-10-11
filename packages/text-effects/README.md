# Text effects

The official `text-effects` package provides 118 independently importable components, sharing 62 WGSL modules. They use the public `TextEffect` primitive and imported WGSL modules. Their text remains available to selection, copy, search, links and logs.

```tsx
import HolyGleam from "@text-effects/holyGleam";
import Greeting from "@text-effects/greeting";
import Radiate from "@text-effects/radiate";
import { Span } from "smudgy:widgets";
import { echo, line } from "smudgy:core";

echo(<HolyGleam duration={1800} scale={1.5}>
    <Span fontFace="serif" fontSize={28}>A brilliant holy light surrounds you.</Span>
</HolyGleam>);
line.replace("Aria", <Greeting duration={1400}>Aria</Greeting>);
echo(`Attention starts here ${<Radiate duration={2400}>Look here</Radiate>}`);
```

Raw or styled text is shorthand for one Span. A Span can contain styled runs, font overrides and links. Widget children and nested effects are rejected. Every component works in echo, line.insert, line.replaceAt, line.replace, template interpolation, and Container/createWidget.

Choose an effect in the [catalogue](CATALOGUE.md), or write one with the [shader authoring reference](../../widgets/SHADERS.md).

## Imports and migration

Import the component you need from its own subpath. The default export can have any local name; the named export uses the catalogue name. `@text-effects/fire` is shorthand for `smudgy:@text-effects/fire`. Package names are globally unique, and a local package with the same name takes priority. Subpaths are case-sensitive: `/holyGleam`, `/reverseSmoke`, and so on.

```tsx
import Fire from "@text-effects/fire";
import { lightning as Lightning } from "@text-effects/lightning";
import type { FireProps } from "@text-effects/types";
```

The package root opens the interactive playground, including when the package is installed and enabled. It keeps the loader and type exports for compatibility, but ordinary scripts should use individual effect subpaths. The shader-free `/load` entry exports `effectNames`, `loadEffect` and `loadEffects`; `/types` exports prop types. Each component evaluates only its shader and shared helpers. Components in a family reuse one compiled shader through normal module caching, even when mixed with direct imports.

For a user-selected effect, await its loader before building synchronous JSX:

```tsx
import { loadEffect, loadEffects } from "@text-effects/load";

const Fire = await loadEffect("fire");
echo(<Fire embers={0.5}>Flames!</Fire>);
const chosen = await loadEffects(["wind", "poisonCloud"]);
echo(<chosen.wind mode="arrival" direction="left">Arriving</chosen.wind>);
```

Loaders preserve each component's prop types and reject unknown names. `effectNames` and the selected result are frozen. The showcase uses these loaders: listing buttons compiles no shaders; selecting an effect loads its program once. Installed package files can still be fetched together; selective imports avoid evaluating and compiling unused shader modules. Loaded modules remain cached until the script generation is retired, under the existing native resource limits.

**Migration:** the old root `Effects` export has moved to the explicit eager `/all` entry. Prefer individual imports in normal scripts. Code that deliberately uses the entire catalogue can opt in:

```tsx
import { Effects } from "@text-effects/all";
```

`/all` waits for every registered component during module evaluation; the import
above needs no extra loading call and preserves each component's own prop types.

Sandboxed packages that import WGSL or construct `TextEffect` instances must request `permissions.smudgy.widgets: ["shaders"]`. This is independent of `["create"]`: displaying inline effects or mounting widgets still needs widget creation permission, and echoing or editing terminal text retains its own permission checks. Package manifests use the explicit `smudgy:@text-effects` address in `dependencies`, rather than the import shorthand. For example:

```json
{ "dependencies": ["smudgy:@text-effects"], "permissions": { "smudgy": { "widgets": ["create", "shaders"], "session": ["echo"] } } }
```

The shader permission is checked before compilation and GPU prewarming, and again when constructing a `TextEffect`. It participates in dependency permission unions and update consent; an existing widget creation grant does not authorize shaders. Local user modules and trusted packages retain full access.

Raw shader handles remain available through `.wgsl` subpaths, for use with the public `TextEffect` primitive and authored uniforms.

## Trying the library

Install and enable `text-effects` to open its playground. You can also open it explicitly from a module:

```tsx
import "@text-effects";
```

The panel starts with individual-import usage examples and provides all presets, scale/intensity/speed/duration/font controls, and terminal previews. `effects` reopens the panel; `effect fire` previews a named preset. The package declares the pane, widget, shader, echo and alias permissions needed to run it in an installed sandbox. Choosing a preset, changing a control, or replaying echoes a new line in the terminal default colors. Finite effects expire in place; duration 0 loops while visible. `examples/inline-effects.tsx` demonstrates inline controls, selection, tooltips and both Canvas and shader attention effects.

Publication is separate from the source checkout. For local development, copy this directory into a server's `packages/text-effects` directory. Use the same `@text-effects` and `@text-effects/fire` imports; the local directory takes priority over the published package, so no import changes are needed.

## Shared controls

- `scale`: 0.25..4, default 1. Scales artwork dimensions, turbulence, particles and travel distances; emphasis scales painted growth. Native font metrics, layout, selection and particle count stay unchanged. Automatic paint outset grows with scale; explicit outset is a final authored bound.
- `intensity`: 0..2, default 1. `speed`: 0..8, default 1. Speed controls procedural motion; finite progress and fades follow duration independently.
- `colors`: exactly three `#RRGGBB` or `#RRGGBBAA` strings: base, bright and accent.
- `duration`: whole milliseconds, 0..3600000. Every effect supports a finite introduction. Zero continues or repeats until removed; continuous smoke dissolves and reforms.
- `fadeIn` / `fadeOut`: whole milliseconds. Finite effects default to a short attack and release; fades are shortened proportionally when their sum exceeds duration. Continuous effects have no release. A held final image defaults to no release.
- `finish`: `remove` restores ordinary text; `hold` freezes the last image and stops animation requests. Defaults to remove, except frost, thorns and rootbind which hold their fully formed final geometry and stop requesting frames. Smoke, reverseSmoke and teleport restore readable glyphs even when explicitly held.
- `overflow`: `bounds` or `pane`. Pane paints across the containing viewport while the text anchor is visible, without changing layout or hit targets. Scrolling the anchor offscreen stops painting/frame requests; its clock continues. Wrapped fragments remain separate anchors. This does not capture or distort the terminal backdrop.
- `outset`: 0..2048 logical pixels for bounded painting. Pane mode uses the viewport instead. Font settings on Span determine line layout.
- `captureScale`: glyph capture resolution multiplier, 1..8, default 1 unless the preset requests higher quality. Higher values improve enlarged text at a one-time capture cost within the existing dimension/pixel budgets; long text or large fonts can receive a lower actual resolution. This is also available on the public TextEffect primitive.

Effect-specific controls, arrival/departure modes and playback examples are described in the [catalogue](CATALOGUE.md).

## Custom shaders

Import a `.wgsl` file and pass its native handle to `TextEffect`. The [authoring reference](../../widgets/SHADERS.md) defines the functions, bindings, coordinates, colour convention, glyph metadata and optional fragment interface. The built-in presets use that same public contract.

## Resource and performance bounds

These are single-pass effects with analytical particle emitters. There are no persistent simulation buffers, collisions, sprite atlases, feedback textures or backdrop capture. Naga validates the shader ABI, not its execution cost: authors must bound loops and profile large paint extents. At a fixed captureScale, scale and pane overflow increase shaded pixel area without increasing glyph-input texture size. Emphasis chooses its captureScale from the authored growth, within the same input budget.

Each script isolate can retain up to 4096 shader handles within a 64 MiB source/reflection payload budget. This counts the complete validated source, including host declarations, labels and reflected fields; it excludes allocator overhead and GPU resources. WGSL author source is limited to 64 KiB, labels to 1 KiB and custom uniforms to 1 KiB. Imports normally remain cached for the script generation. Released handles reclaim their budget on the next compilation, provided no effect still owns the shader.

Capture admission is separate: up to 64 text captures across rendered effects, with wrapped fragments counting separately. A wrapped effect is admitted as a unit. Each glyph input is at most 2048 pixels per dimension and 1 MiB of RGBA payload including filter levels, plus a 12 KiB geometry table; oversized text reduces input resolution. Admission failure keeps native text visible and retries when capacity is available.

Each GPU engine retains up to 64 idle pipelines after trimming, in addition to pipelines owned by active draw commands. Eviction follows recent use and does not interrupt a playing effect. Imports and cold draws share a compilation result, with at most 64 queued/in-progress cold programs per engine. Driver compilation errors are reported with the shader label. A failed cold program leaves native text visible and stops requesting frames; it is not retried for that shader on the same engine. A new import or engine can try again. These bounds do not cover all driver or renderer memory.

Matching captures share an immutable glyph image within one GPU renderer. Equality covers the ordered resolved glyphs and font identities, colour/alpha, physical placement and effective capture resolution. Each occurrence keeps its own source offsets and geometry. Animation frames use the existing paragraph lookup without rebuilding a content key; uniforms, duration and effect choice do not change captured pixels.

After an occurrence retires, the renderer retains up to 50 idle images within a 64 MiB capture budget, evicting the least recently used images. The budget includes mip levels, complete glyph keys and ink bounds, so very long strings also have bounded retained metadata. Active captures keep their existing admission limits and remain valid during cache eviction. Scrollback removal releases the occurrence; it does not invalidate reusable pixels or retain the old line, widget or script generation. Renderer teardown releases the cache. Per-occurrence geometry, other renderer resources and driver overhead are separate.

## MUD trigger bursts

[`mud-effects.tsx`](../../examples/text-effects/mud-effects.tsx) and [`effect-burst.ts`](../../examples/text-effects/effect-burst.ts) show deferred stored-line edits, latest-wins coalescing per target/event, a 1200 ms per-target cooldown, a 64-target bound, and three starts per 120 ms drain. Change the literal trigger phrases for your MUD. The examples never send server commands. Timers and pending script state disappear on reload; no JavaScript runs for shader animation.

## Native measurement

Contributors can run the opt-in GPU regression suite and the native integration/reload suite with:

```sh
cargo test -p smudgy_ui_shared --lib --locked -- --ignored --test-threads=1
cargo test -p smudgy_ui --test text_effects_catalogue --locked -- --include-ignored --test-threads=1
```

The GPU suite requires a working hardware renderer. Run it serialized; several tests share process-wide font and capture state. The integration suite includes 32 script generations to check retirement during reload.

For application measurements, see [Renderer profiling](../../CONTRIBUTING.md#renderer-profiling).
The `profiling` feature is off by default; setting `SMUDGY_FRAME_PROBE` alone does
not enable instrumentation. Performance results must distinguish baseline and
instrumented builds. Configurable artwork capture and synthetic timing tools are
development tooling, separate from the fixed-input regression tests.

The [inline tutorial](../../examples/inline-effects.tsx) combines selectable text effects, buttons, a tall panel, tooltips and a canvas attention animation. To tune typography, wrap the effect text in a Span; Emphasis also accepts controls such as `peak={3}` and `yaw={0.35}`.
