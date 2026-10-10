# Authoring text effects

This reference describes the current TextEffect host contract (ABI v1). It covers source WGSL imports; compiled GPU pipelines and native handles are local to a running client. There is no shader ABI version negotiation. Compatible clients must implement the complete contract below, including sampling and compositing semantics. Matching uniform names alone is insufficient.

See the [widget guide](README.md) for inline text and package permissions.

## A minimal effect

Text effects use the public `TextEffect` primitive and ordinary imported WGSL modules:

```tsx
import { TextEffect } from "smudgy:widgets";
import shader from "./my-effect.wgsl";

const highlighted = <TextEffect shader={shader} uniforms={{ strength: 0.8 }}
    composite="underlay" outset={24}>Look here</TextEffect>;
```

```wgsl
struct Parameters { strength: f32 }
@group(1) @binding(0) var<uniform> params: Parameters;
fn effect(position: vec2f) -> vec4f {
    let alpha = coverage(position + vec2f(2.0, 0.0)) * params.strength;
    return vec4f(0.2 * alpha, 0.6 * alpha, alpha, alpha);
}
```

Imports return opaque native shader handles. When a window has registered its GPU, importing also prewarms its render pipeline on the script thread and reports a labelled error if the driver rejects it. Imports before any GPU exists remain validated handles; a bounded worker queue compiles the pipeline on first use while native text stays visible. Static visible effects request frames until compilation resolves. A rejected pipeline keeps native text visible, logs its error once and stops requesting frames; the same shader is not retried on that engine. Imports and cold draws share their compilation attempt. Glyph inputs still require preparation when text or display scale changes.

The host validates WGSL and reflects the parameter struct; missing/unknown fields, wrong vector lengths, and invalid numeric values fail when the effect is constructed. Parameters support f32/i32/u32 scalars and vectors. The host supplies entry points and group 0 resources. Shader functions return premultiplied sRGB RGBA.

`sampleText(position)` returns styled glyph colour and coverage; `coverage(position)` returns its alpha. Positions and `text.text_size` use logical pixels relative to each wrapped text fragment. The `text` frame also supplies `time` in seconds, finite-duration `progress` (0..1), a per-instance `seed`, `outset`, the terminal `background`, `effect_scale`, `duration` in seconds, and the current fade `envelope`. `paint_offset` locates the paint surface relative to the text anchor. No surrounding terminal pixels or child widget images are captured.

`sampleTextLod(position, lod)` samples the cached, premultiplied glyph image at a filtered level (0 is the original capture). Levels are generated once on the GPU and included in the 1 MiB input budget. `noise3D(position, lod)` returns smooth, tiled noise in -1..1 from a shared 256×256 GPU-generated atlas; lod 0..8 controls filtering. Two filtered red-channel lookups interpolate independently hashed adjacent Z layers. The atlas uses about 342 KiB per renderer, independently of effect count, and requires no script resource ownership or frame uploads. Both helpers are available to ordinary WGSL imports and instanced glyph shaders.

Capture follows the native renderer's physical pixel placement, including fractional paragraph positions, glyph X bins and rounded line baselines. `sampleText` and `sampleTextLod` apply the occurrence's `text.capture_offset` automatically; custom effects should use those helpers rather than assume fixed texture padding. This field occupies the frame's former reserved pair of floats, preserving its byte layout. Supersampling retains the native placement while rasterizing larger outlines, so small antialiasing and hinting differences can remain. A different screen position or DPI can require new placement metadata or pixels; animation at a fixed placement reuses its capture. Image equality compares the resulting raster recipe, so integer translations can still share pixels.

`glyphCount()` and `glyphAt(index)` expose up to 256 cached, shaped glyphs in visual order. A glyph has `advance` (x, y, width, line height), `ink` (x, y, width, height), and `cluster` (paragraph UTF-8 start/end offsets, paragraph line index, baseline bits). Whitespace has zero ink; combining glyphs can share a cluster; a ligature may cover multiple characters. Out-of-range access returns zero geometry. `glyphsTruncated()` reports inputs beyond the table limit. Text colour/coverage remains complete even when metadata is truncated. Electricity attaches bridges to actual ink and skips spaces/shared clusters; frost roots use the same geometry. `textBaseline()` returns the first captured run’s baseline and `glyphBaseline(index)` returns each glyph’s run baseline, in local logical pixels. These come from shaping rather than line-height estimates; the table is uploaded with the glyph capture, never per animation frame.

The host applies `fadeIn`/`fadeOut` after the shader. Underlays fade to transparent; replacements fade against the original glyph image. Custom shaders read `text.effect_scale` and scale their artwork while keeping `sampleText()` coordinates anchored.

Use `composite="underlay"` to keep readable native text on top, or `composite="replace"` to supply its foreground. `finish="hold"` retains the final shader output; `finish="remove"` restores ordinary text. `animated={false}` disables continuous motion requests; finite lifetimes and fade-in still request frames until they finish. Glyph geometry and shader pipelines are cached; animation uses native presentation-frame requests and does not execute per-frame JavaScript. Software renderers retain ordinary text without running an effect.

## Host bindings and memory layout

Group 0 belongs to the host. Authors may add one uniform struct at group 1, binding 0; its fields must be 32-bit numeric scalars or vectors. Every field must be supplied. Matrices, arrays, nested structs, booleans, pipeline overrides, custom textures and storage buffers are not supported.

| Group / binding | Resource | Meaning |
| --- | --- | --- |
| 0 / 0 | `text: SmudgyTextFrame` | Frame and occurrence uniforms |
| 0 / 1 | `smudgy_glyphs: texture_2d<f32>` | Premultiplied styled glyph RGBA, with mip levels |
| 0 / 2 | `smudgy_sampler: sampler` | Linear glyph filtering |
| 0 / 3 | `smudgy_geometry: SmudgyGlyphTable` | Shaped glyph metadata |
| 0 / 4 | `smudgy_noise: texture_2d<f32>` | Shared 256×256 tiled noise atlas, with mip levels |
| 0 / 5 | `smudgy_noise_sampler: sampler` | Linear, repeating noise filtering |
| 1 / 0 | Author-defined uniform struct | Reflected parameters, at most 1024 bytes |

The frame occupies 112 bytes. Vectors use WGSL host-shareable alignment. Custom uniform buffers are rounded up to a multiple of 16 bytes, with a minimum of 16.

| Frame field | WGSL type | Offset | Meaning |
| --- | --- | ---: | --- |
| `origin` | `vec2f` | 0 | Paint origin in physical target pixels |
| `resolution` | `vec2f` | 8 | Paint width and height in physical target pixels |
| `surface` | `vec2f` | 16 | Paint width and height in logical pixels |
| `texture_size` | `vec2f` | 24 | Captured glyph texture dimensions in texels |
| `text_size` | `vec2f` | 32 | Wrapped text fragment dimensions in logical pixels |
| `scale` | `f32` | 40 | Actual capture texels per logical pixel |
| `outset` | `f32` | 44 | Authored bounded paint outset in logical pixels |
| `time` | `f32` | 48 | Elapsed seconds, clamped for a finite hold; wraps at 4096 seconds |
| `progress` | `f32` | 52 | Finite elapsed/duration, clamped to 0..1; zero for continuous effects |
| `seed` | `f32` | 56 | Stable seed for this effect occurrence |
| `linearize` | `u32` | 60 | Host output adaptation flag; handled by the adapter |
| `background` | `vec4f` | 64 | Terminal background RGBA |
| `effect_scale` | `f32` | 80 | Authored artwork scale, independent of capture resolution |
| `duration` | `f32` | 84 | Duration in seconds; zero for continuous effects |
| `envelope` | `f32` | 88 | Attack/release multiplier applied by the adapter |
| `replaces_text` | `u32` | 92 | Replacement compositing flag |
| `paint_offset` | `vec2f` | 96 | Logical paint origin relative to the text anchor |
| `capture_offset` | `vec2f` | 104 | Logical texture origin relative to the text anchor |

`SmudgyGlyph` occupies 48 bytes: `advance: vec4f` at 0, `ink: vec4f` at 16, and `cluster: vec4u` at 32. `SmudgyGlyphTable` begins with `info: vec4u`, followed at byte 16 by 256 glyphs with a 48-byte stride (12304 bytes total). `info` contains the glyph count, truncation flag, first baseline as f32 bits, and a reserved zero word. Prefer the supplied geometry helpers over decoding these fields directly.

## Moving captured glyphs

An imported WGSL module can opt into the same single-pass instanced-quad path. Declare a `u32` constant `const TEXT_FRAGMENTS: u32 = 1u;` (1..64) and implement both `textVertex(instance: u32, corner: vec2f) -> SmudgyFragment` and `textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f`, in addition to `effect(position)`. Constant arithmetic folded by WGSL is also accepted; the evaluated count must stay within that bound. Instances are 0..`glyphCount()*TEXT_FRAGMENTS`; instance / TEXT_FRAGMENTS identifies a shaped glyph. Corner ranges from (0,0) to (1,1). The host paints `effect` across the surface first, then the fragment quads in instance order within the same draw call.

`SmudgyFragment(position, source, perspective, opacity)` takes a projected logical position relative to the text anchor, a logical source-sampling position, positive clip W for perspective-correct interpolation, and opacity. The host clamps W to at least 0.05 and opacity to 0..1, samples styled premultiplied glyph RGBA, and applies the shared envelope to the result. Authors must bound near-camera growth, preserve complete source coverage, and provide a `glyphsTruncated()` policy. Motion retains the complete ordinary word above 256 shaped glyphs; Explode uses a coarser whole-word grid. There are at most 16,384 quads per admitted fragment, six generated vertices each, no per-frame vertex upload, new texture, simulation buffer or CPU effect drawing. Glyph captures and geometry remain cached.

## Implementing the contract in another client

A compatible host needs to validate and reflect source WGSL, supply the host declarations and both entry-point adapters, pack uniforms with the layout above, and provide the glyph and noise resources. It also needs equivalent font shaping, glyph baselines and UTF-8 clusters; logical-to-physical placement and capture offsets; mip filtering; premultiplied sRGB output adaptation; finite playback and envelopes; pane clipping; and the optional instanced-quad path. Fonts and rasterizers can still produce small visual differences. Native handles are not transferable between clients.

The source of truth is the [shared WGSL declarations](../session_model/src/text_shader_common.wgsl), [surface adapter](../session_model/src/text_shader.wgsl), [fragment adapter](../session_model/src/text_shader_fragments.wgsl), and [CPU upload layouts](../ui_shared/src/text_effect/gpu/abi.rs). Tests compare every CPU frame/glyph field and the glyph array stride against WGSL reflection. An incompatible contract change needs an explicit compatibility design before release; appending fields silently is not a versioning scheme.

## Validation and performance

Validation checks structure and types; it does not prove a shader will finish
quickly. Keep loops bounded and measure representative text, large paint
surfaces, simultaneous effects, DPI changes and retirement. Source typography,
selection, links, copy and logs stay anchored to native text even when the
shader moves its pixels. See [renderer profiling](../CONTRIBUTING.md#renderer-profiling)
for baseline and instrumented measurements.

The host bounds imported source, uniform payloads, retained handles, queued
pipelines, visible captures and idle cache memory separately. Limits are defined
in [source validation](../session_model/src/text_shader.rs),
[isolate budgets](src/shader_budget.rs),
[capture admission](../ui_shared/src/text_effect/mod.rs),
[texture capture](../ui_shared/src/text_effect/gpu/capture.rs) and
[idle image retention](../ui_shared/src/text_effect/gpu/cache.rs).
Budget or compilation failure preserves native text. These bounds do not limit
all driver memory or make an arbitrary shader inexpensive.

The ink-free object metric font is generated by
[its source](../ui_shared/src/inline_object/metrics.py), which records its
repository license. It reserves native inline widget geometry without painting
placeholder glyphs.
