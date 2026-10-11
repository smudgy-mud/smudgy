# Adding text-effects presets

The client contract is documented in the [shader authoring reference](../../widgets/SHADERS.md). See the [package guide](README.md) for imports and permissions and the [catalogue](CATALOGUE.md) for shipped effects.

## Adding a catalogue preset

Give the component its own `name.ts` subpath, with matching named and default
exports. Use `component<Props>()` in `_shared.ts` to apply the common controls,
and a family helper when multiple presets share one program. The optional
`EffectComponent<Props>` type retains specialized props for reusable components.
Keep shader imports in the component or its family helper; `/load` and `/types`
must remain shader-free.

Register a literal import in `load.ts`. This is the only registry: `/all` awaits
that list during module evaluation and exports the resulting frozen `Effects`
object. Ordinary `import { Effects } from "…/all"` remains sufficient. Add a
caption and a family button in `showcase.tsx`, and describe the appearance and
specialized controls in [CATALOGUE.md](CATALOGUE.md). The playground forwards its
controls to the chosen preset and keeps that preset's finish behavior.

Add the preset's GPU input in
[catalogue fixtures](../../tests/text-effects/fixtures.rs).
A GPU-free test checks those inputs against all public component files and
reflects their uniforms. Hardware playback assertions remain explicitly opt-in
and serialized. Native scenarios are separated into content, imports/permissions
and reload modules under `tests/text-effects/native/`.

The native integration suite checks component files against the registry, matching
named/default/lazy/eager exports, shared prop boundaries, shader sharing and
playground buttons. The declaration regression compiles the package and examples,
then recompiles a consumer against only the generated declarations. Run both with:

```sh
cargo test -p smudgy_core --lib --locked text_effects_sources_and_published_declarations
cargo test -p smudgy_ui --test text_effects_catalogue --locked -- --test-threads=1
```

## Source and assets

The catalogue ships WGSL source under the repository license. Visual references
are not bundled assets. ReverseSmoke uses glyph-derived density, three depth
samples and analytic gradient lighting; the shared noise helper interpolates
independently hashed atlas layers. Neither includes the supplied Shadertoy scene,
noise function or volume-density implementation. Keep source attribution and
license review separate from the publication guard's path/content checks when
introducing externally authored shader code.

Preview images and runtime measurements are development outputs; they are not shipped in the package and cannot change fixed regression inputs.
