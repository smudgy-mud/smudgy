# Effect catalogue

Appearance, effect-specific controls and examples for the shipped presets. For imports, permissions and shared controls, see the [package guide](README.md). For custom programs, see [shader authoring](../../widgets/SHADERS.md).

- [Readable hero signals](#readable-hero-signals)
- [Ink, pressure and gravity](#ink-pressure-and-gravity)
- [Far-reaching attention](#far-reaching-attention)
- [Elemental and magical effects](#elemental-and-magical-effects)
- [Darkness, nature and arcane inscriptions](#darkness-nature-and-arcane-inscriptions)
- [Living inscriptions and watching shadows](#living-inscriptions-and-watching-shadows)
- [Mind and perception](#mind-and-perception)
- [Materials, bonds and restoration](#materials-bonds-and-restoration)
- [Combat, healing and travel](#combat-healing-and-travel)
- [Attention presets](#attention-presets)
- [Text motion](#text-motion)
- [Smoke and cloud controls](#smoke-and-cloud-controls)
- [Text motion and fragmentation](#text-motion-and-fragmentation)
- [Arrivals, materials and hero effects](#arrivals-materials-and-hero-effects)

## Readable hero signals

These ten components use enlarged captured lettering for impact while leaving the ordinary terminal text readable from the first frame. Native ink, position, line height, selection, links and source colors stay intact. A background-colored contour protects the source letters from the projected impressions behind them. The ten gestures share one WGSL module, three bounded meshes per shaped glyph and a cached 4x capture. No additional renderer API, texture resource or per-frame glyph upload is required.

All default to 2400ms, pane overflow and removal back to plain text. `duration={0}` repeats; `finish="hold"` rests on ordinary text. `amplitude` (0..2), `intensity`, `scale`, `speed`, `colors`, `duration` and `captureScale` use the common controls. Duration controls the finite gesture; speed controls travelling reflections and repeat cadence. Zero amplitude, intensity or speed leaves only ordinary text. Pane clipping remains deliberate: these are large attention signals, and extreme scale or a nearby vertical edge can crop the impressions. The actual source line stays readable.

| Component | Appearance |
| --- | --- |
| unarmed | Two red impressions strike, then split into empty outlines: an unarmed warning. |
| knockback | Heavy captured lettering kicks backward through three depth planes. |
| crosscut | Opposed halves of an enlarged inscription shear past one another. |
| aftershock | Three expanding engraved impressions carry a delayed shock through the word. |
| overdrive | Forward-rushing perspective impressions stretch into sharp luminous ink. |
| ricochet | Three angled strikes rebound from alternating sides of the inscription. |
| rebound | A deep projected word flexes outward, recoils and settles back around the line. |
| focusLock | Huge separated glyph contours close precisely around their readable source. |
| victoryStamp | An oblique gold inscription stamps into place and catches a broad reflection. |
| lastStand | Opposed crimson text planes compress toward the source with a final defiant pulse. |

```tsx
import Unarmed from "@text-effects/unarmed";
import VictoryStamp from "@text-effects/victoryStamp";
import { Span } from "smudgy:widgets";

// Inside a game-specific trigger, target only the relevant combat phrase.
line.replace("You hit", <Unarmed duration={1200}>You hit</Unarmed>);
echo(<VictoryStamp><Span fontFace="serif">You have gained a level!</Span></VictoryStamp>);
```

Unarmed is a visual warning, not a game-state detector. Scripts determine when the player has lost their weapon; narrow matching and a cooldown avoid playing the signal on every hit in combat spam. All ten also work through `line.insert()`, other text splices and ordinary `createWidget()` containers.

## Ink, pressure and gravity

These eight components act on the captured inscription. All default to 2400 ms, preserve its layout and selectable text, and restore ordinary text at the end. Duration zero repeats. A zero intensity or speed preserves ordinary text. The material shaders darken their colors on light terminal backgrounds to keep the strokes readable.

| Component | Appearance | Overflow |
| --- | --- | --- |
| crystal | Translucent fractured ink, refractive bevels and a travelling facet reflection | Bounded |
| lava | A molten front passes through the strokes; glowing fissures cool beneath dark crust | Bounded |
| decay | Corrosion pits exposed stroke edges and sheds small captured ink chips | Bounded |
| weightPulse | A soft wave adds and releases ink weight in reading order | Bounded |
| psionic | Invisible local pressure compresses and shears the word, then releases it in place | Bounded |
| vanish | Letters turn edge-on into a narrow seam; arrival, departure and cycle modes | Bounded |
| dragonBreath | A turbulent curling fire front crosses the pane and scorches the actual inscription | Pane |
| eventHorizon | A dark aperture stretches the inscription into orbiting ribbons, consumes it and reconstructs it | Pane |

```tsx
import Crystal from "@text-effects/crystal";
import Vanish from "@text-effects/vanish";
import EventHorizon from "@text-effects/eventHorizon";

echo(<Crystal><Span fontFace="serif" fontSize={28}>A crystal inscription</Span></Crystal>);
line.replace("hidden", <Vanish mode="departure" finish="hold">hidden</Vanish>);
echo(<EventHorizon duration={2000}>The inscription crosses the event horizon.</EventHorizon>);
```

`weightPulse`, `psionic` and `vanish` accept `amplitude` (0..2). WeightPulse and Psionic preserve the Span's colors; their palette is unused. Vanish uses the accent color for its seam and defaults to `mode="cycle"`. Arrival ends with ordinary text. A departure becomes invisible; use `finish="hold"` to retain that final image. Removal restores the underlying text, including after a departure. Scale changes pressure, ink weight, facet size, flame size or aperture size without reserving extra layout space. Baselines remain anchored in the compact effects.

The materials share `transmuted-ink.wgsl`; pressure and transitions share `pressure-ink.wgsl`. The two heroes each have their own program. They use the existing glyph capture, tessellated text fragments, filtered noise and playback interfaces. Default capture resolution is 2x, except EventHorizon at 3x for its stretched ribbons. No new client resource or effect-specific rendering path is needed.

## Far-reaching attention

These eight signals are intended to catch peripheral vision and lead it back to a particular message. Each defaults to a 2400 ms burst, pane overflow and underlay compositing: ordinary styled text stays on top, with a narrow protected contour around its ink. They reserve the original line metrics and keep selection and links anchored to the message.

| Component | Appearance | Example use |
| --- | --- | --- |
| sonar | Three staggered elliptical wavefronts expand through the pane | A friend calls for attention |
| converge | Four curved streaks enter from the pane edges and lock into word brackets | You are targeted |
| beacon | A broad golden shaft opens into a five-ray fan toward the roomier vertical pane edge | Quest character or important arrival |
| cometCall | Opposing curved comets meet at the message with a local flare and expanding wake | Formidable creature appears |
| prismSweep | A broad spectral fan sweeps around the message with a white leading edge and weaker reflection | Discovery or reward |
| alarm | Opposing moving hazard bands bracket the message across the pane | Danger or an expired protection |
| echoFrame | Three expanding rounded frames with luminous corners | Friend or group arrival |
| vitalSign | A travelling heartbeat trace and two pulses under the message | A groupmate needs healing |

```tsx
import Sonar from "@text-effects/sonar";
import VitalSign from "@text-effects/vitalSign";

echo(<Sonar duration={1800}>Aria calls for your attention.</Sonar>);
line.replace("Aria is badly wounded!", <VitalSign>Aria is badly wounded!</VitalSign>);
```

They share one WGSL program and the standard `EffectProps`: scale, intensity, speed, three RGBA colors, duration, fades, capture resolution, finish and overflow. Scale adjusts the signal's thickness and detail; travel is derived from the actual pane bounds. Beacon chooses the roomier vertical direction, comets adapt their curvature to the space above and below, and VitalSign moves its trace above text near the bottom edge. Positive durations finish at the requested time even when speed changes the pacing. Both removal and an explicit hold finish with ordinary text. Duration zero repeats; zero intensity or speed preserves ordinary text and stops requesting animation frames. Light backgrounds use a darker version of the base color.

These are GPU-only signals. They reuse the existing capture, filtered noise and compositing interfaces, with no new client API, resource type or per-frame script work. Their default capture resolution is 1x. Like other pane effects, they paint only while their source text is visible and clip at the terminal pane.

## Elemental and magical effects

| Component | Default playback | Appearance |
| --- | --- | --- |
| fire | Continuous | Glyph-seeded flames and rising embers |
| smoke | 2400 ms, restores text | Dissolve into drifting residue, then reassemble the letters |
| reverseSmoke | 2400 ms, arrives as text | Three diffuse, independently swirling bodies per glyph gather in place into styled text |
| electricity | Continuous | Eight staggered bridge slots between inked letters, quick flashes and endpoint sparks |
| lightning | Continuous, bounded overflow | Left-to-right strikes followed by two brief blooms around the struck word |
| poisonCloud | Continuous | Swirling green puffs and rising skulls |
| healingCloud | Continuous | Luminous mist and rising hearts |
| frost | 2000 ms, holds formed ice | Fixed branching crystals grow from glyphs and stop |
| ward | Continuous | Close protective frame with moving highlights and small end runes |
| impact | 1000 ms | Expanding shockwaves, sparks and an initial flash |
| animeImpact | 2600 ms, pane overflow | Broad white rays with varied widths/lengths, slow opposing rotations and a burst envelope |
| holyGleam | 1800 ms | Broad gold halo, sweeping gleam and white starbursts with soft paint limits |
| acid | Continuous | Purple wet glyph edges, pendant drops and narrow falling rivulets |
| spectral | Continuous | Readable pale words with slow translucent afterimages |
| swarm | Continuous | Winged insects with independent flight paths and wingbeats |
| thorns | 2000 ms, holds grown vines | Opposed vines wind behind and in front of the lettering, thicken, and grow hooked thorns with curled end tendrils |
| fireflies | Continuous | Up to 24 drifting fireflies with independent warm glows and wing glints |
| runicOrbit | Continuous | Two slow opposing rune orbits and a fine travelling tracer |

## Darkness, nature and arcane inscriptions

These eight presets default to 2400 ms and replacement foreground. Their own timelines provide attack and settling. Painting is bounded except for elderSign, which deliberately fills neighboring rows within its pane. Zero intensity or speed preserves ordinary text.

| Component | Appearance |
| --- | --- |
| blackTide | Dark pools join behind a traveling front, consume reached strokes, then retreat and restore the inscription |
| eldritchEcho | Source-ink impressions lag and briefly anticipate restrained glyph motion |
| aberration | Four mismatched strips of each glyph slide, shear and reconnect in place |
| rootbind | Curved, forked roots grow from sampled lower glyph ink and hold their grown form |
| autumn | Source strokes become veined leaves, tumble and gather back into letters |
| spellSeal | A glowing writing front inscribes a close elliptical seal; runes appear, charge and fade |
| leyWeave | Alternating bowed connections link shaped glyph anchors; two charge packets travel across the inscription |
| elderSign | A dark field anticipates opposing broken rings and hooked rays; the seal locks, then withdraws |

eldritchEcho and aberration accept `amplitude` (0..2). autumn also accepts `mode="arrival" | "departure" | "cycle"`, default cycle; held departure deliberately retains dispersed leaves. rootbind defaults to `finish="hold"` and stops requesting frames after growth. Override it with remove to release the roots and restore native text. The other seven restore readable source text at the end of a finite cycle or arrival, including explicit holds.

These presets reuse the existing capture, geometry, filtered noise and instancing interfaces. They require no new textures, host resources, per-frame uploads or JavaScript. spellSeal and leyWeave share a program; eldritchEcho and aberration share another. Ordinary layout and interaction targets remain anchored to the source text.

## Living inscriptions and watching shadows

These seven effects make the captured lettering their subject. They default to 2400 ms, bounded overflow, 2x cached capture and removal on completion. Explicit holds finish as readable source text. Zero intensity or speed preserves ordinary text. Scale changes painted motion and detail without changing the reserved baseline.

| Component | Appearance |
| --- | --- |
| voidBloom | Darkness consumes the inscription from within; reached strokes hollow and draw inward before recovering |
| watchers | Letter counters wake as eyes; the surrounding captured strokes open and blink, then their gazes align |
| mycelium | A growth front swells, curls and joins the source strokes into a shaded living body |
| barkMend | Wounded letters separate into two banks; fresh growth reconnects their actual strokes from left to right |
| ritualKnot | The inscription folds into a knotted ribbon made from its captured letters, then unfolds |
| counterSeal | Actual letters turn into a mirrored hostile cipher; a counterspell restores their orientation in sequence |
| soulDrain | Patches of source ink escape as ghosted impressions, leaving depleted holes while surviving strokes remain readable |

mycelium and barkMend share one program; ritualKnot and counterSeal share another. These effects use existing filtered captures, noise and bounded instancing. They add no resource type or per-frame upload.

For finite playback, positive speed biases the action earlier or later within the requested duration while retaining exact start and finish. For zero-duration playback, it changes the repeat rate. Speed zero keeps ordinary text.

## Mind and perception

The lettering enacts these effects. They reserve the original line metrics and default to 2400 ms, bounded overflow and 2x cached capture. All return to styled, readable source text, including held finishes. Zero intensity or speed shows ordinary text. For finite playback, speed biases the action within the duration; zero duration repeats the sequence.

| Component | Appearance |
| --- | --- |
| silence | Filled strokes empty from left to right into thin hollow contours, then regain their voice |
| possession | Letters bend and contract independently, then synchronize into one imposed heartbeat; their feet stay anchored |
| trueSight | Conflicting impressions twist over the inscription; a sweep peels them away to reveal one precise reading |
| forgetting | Pieces of the strokes fade first at the end of the line, return in uncertain attempts, then recover completely |

Scale controls the contour width, deformation, false-impression reach or diffusion footprint respectively. These four programs use the existing cached glyph texture and bounded fragment interface; they add no resources or per-frame uploads.

## Materials, bonds and restoration

These ten components default to 2400 ms and restore the original styled lettering on completion, including held finishes. Zero duration repeats the sequence; zero intensity or speed preserves ordinary text. Each has its own import subpath and accepts the common effect props.

| Component | Appearance |
| --- | --- |
| gilding | Irregular gold leaf adheres to the strokes, smooths its creases, and catches one polished reflection |
| stoneSkin | A mineral front thickens the lettering into fixed rough facets, shallow relief and fine fractures |
| verdant | Source ink unrolls from small living coils into shaded, veined green lettering |
| geas | Fine bands cross the actual strokes as the lettering resists, tightens, holds and releases |
| charm | Paired letters lean and bow toward each other, sharing warmth and one gentle highlight |
| poisoned | Purple bruises advance through the ink; narrow green veins and swollen patches pulse locally |
| spatialStitch | A travelling sewing front closes the torn banks of the source lettering |
| interruptNow | Two narrow faults close inward through the strokes, followed by a brief local crossed cut |
| astralConjunction | Three projected inscriptions turn through distinct planes, align into enlarged text, and settle |
| phoenixRebirth | Actual glyph pieces cool into ash, gather into curling plumes, rekindle and return to their strokes |

The first eight use bounded paint and 2x cached capture. The two hero effects default to pane overflow and 4x capture; use them for deliberate spectacles. AstralConjunction fits its enlarged main pose horizontally. PhoenixRebirth uses at most 32 ink pieces and 32 faint trails per shaped glyph. All ten use the existing text capture, geometry, noise and fragment interfaces, with no new client resources or per-frame script work. Their layout, selection and link targets remain at the original text position.

Scale changes material detail, deformation or plume reach without changing reserved line metrics. Duration controls visual playback; interruptNow does not schedule a game action or interrupt a spell.

## Combat, healing and travel

| Component | Default duration | Appearance |
| --- | ---: | --- |
| slash | 900 ms | A travelling blade tip and three fine trailing cuts |
| critical | 1100 ms | A sharp four-point flash fractures into angular shards |
| parry | 1000 ms | Two metal edges meet and release a small fan of sparks |
| cleave | 1200 ms | A tapered crescent blade sweeps clockwise with a separate trailing wake |
| riposte | 1000 ms | A compact guard recoils into a quick lunge and an endpoint flare |
| dispel | 1600 ms | A runic seal breaks into independently drifting shards |
| divineShield | 2200 ms | A close gold capsule forms around the text and catches a travelling highlight |
| healWave | 1800 ms | A bowed green-white wave sweeps through the glyphs and lifts tiny glints |
| blessing | 2000 ms | A descending gold thread and stars settle into a fine underline |
| teleport | 2400 ms | Thin glyph columns lift away from left to right and rebuild from right to left |

These default to bounded paint overflow and restore plain text when finished. Teleport also restores readable glyphs with an explicit held finish. The five combat gestures share one shader; healing and blessing share another. Their solid accents adjust toward the base colour on light terminal backgrounds.

## Attention presets

These default to pane-wide paint overflow and finite playback, except hostile which paints in close bounds. They share one compiled shader with different geometry and palettes.

| Component | Default duration | Signal |
| --- | ---: | --- |
| radiate | 2400 ms | Close red text outline and a wave expanding to 4096 logical pixels at scale 1 |
| greeting | 1600 ms | Warm outward ripples and a heart for a friend |
| hostile | 1400 ms | Short red end brackets and a hot underline converge around an enemy |
| dread | 2600 ms | A dark serrated halo for a formidable encounter |
| quest | 2400 ms | Gold diamond and upward beacon for a quest target |
| levelUp | 2600 ms | Rising gold rays and a fountain of stars |
| healNeeded | 2000 ms | A heart and inward rings draw focus to a wounded groupmate |
| expiring | 1800 ms | An hourglass, contracting broken ring and falling grains |
| rally | 1800 ms | Rising chevrons and a supporting wave |
| discovery | 2200 ms | Compass star and expanding sparkles |

## Text motion

| Component | Default duration | Motion |
| --- | ---: | --- |
| emphasis | 2400 ms | Smoothly magnifies, turns and grows heavier ink on its original baseline |
| tide | 2400 ms | A restrained wave tilts and stretches letters around their baseline |
| elastic | 2400 ms | Anticipation squash, rubber overshoot and diminishing rebounds |
| quake | 2400 ms | Two uneven impacts with short glyph aftershocks |
| explode | 2400 ms | Captured ink breaks into spinning fragments with signed depth and perspective |

`emphasis` grows the captured text, turns it in perspective, adds ink weight and a brief travelling gleam, then settles precisely back onto its original glyphs. Its default 2400 ms cycle uses a 3× peak, with continuous velocity and acceleration through its rise, short hold and release. It accepts the same text/Span children and terminal edits as the other effects.

```tsx
import Emphasis from "@text-effects/emphasis";
echo(<Emphasis peak={8} yaw={0.35} weight={0.5} duration={2400}>
    <Span fontFace="serif" fontWeight="bold" fontSize={24}>Now!</Span>
</Emphasis>);
line.replace("Stand firm!", <Emphasis peak={3}>Stand firm!</Emphasis>);
```

- `peak`: 1..8, default 3. Intensity and scale multiply the growth above 1×.
- `yaw`: -1..1 radians, default 0.35. Zero leaves the text facing forward.
- `weight`: 0..2, default 0.5. Adds ink around the source strokes during the surge; it does not change font metrics or select a new font weight.
- `anchor`: `start`, `center` or `end`, default center. Sets the horizontal growth pivot.
- `fitToPane`: default true. Smoothly limits growth at vertical edges and fits horizontally, keeping the native baseline fixed. Set false for deliberate clipping. Bounded overflow fits to its authored paint surface; pane overflow fits to the containing viewport.

This is paint motion: line height, wrapping, links and selection targets remain at their original text positions. A Span's font settings still determine reserved layout. Both remove and hold finish with ordinary-sized readable text. Zero duration repeats the cycle; speed controls the repeating clock; finite growth and a single gentle yaw follow duration. Zero speed preserves ordinary text. Emphasis defaults to no host fades because its own motion returns the text to rest.

Emphasis requests a cached higher-resolution glyph input based on its maximum growth. `captureScale` can override this (1..8); the same dimension/pixel budgets still apply, so long text or large fonts can receive a lower actual capture resolution. It does not recapture on animation frames.

## Smoke and cloud controls

Fire adds `embers` (0..2). The two cloud presets add `particles` (symbol density, 0..2) and `swirl` (0..2). Zero density leaves only the puffs. Hearts and skulls share the cloud program. Particles have seeded birth phases, lifetimes, trajectories, expansion and fades; everything animates on the GPU without per-frame JavaScript.

```tsx
import PoisonCloud from "@text-effects/poisonCloud";
import Smoke from "@text-effects/smoke";
import ReverseSmoke from "@text-effects/reverseSmoke";
echo(<PoisonCloud duration={900} fadeOut={350} scale={1.5}>
    A brief poisonous poof.
</PoisonCloud>);
echo(<Smoke duration={1200} finish="remove">The text returns.</Smoke>);
echo(<ReverseSmoke duration={2400}>A figure emerges from the smoke.</ReverseSmoke>);
```

`reverseSmoke` stretches and curls the actual captured ink into three independently swirling bodies per shaped glyph. Its opening uses broad, irregular folds with fivefold displacement, filtered diffusion and a shallow, lit volume of layered 3D noise. The bodies circulate around the original line; there is no rising or falling word motion. The extra opening distortion settles before the final glyph-forming phase. Their coordinate maps unwind to the original positions while the same material thickens and regains its source colors. Opacity is shared as the bodies merge, keeping the finished glyph weight intact. The lettering does not fade in behind separate clouds.

It defaults to no host fades so the arrival begins with smoke. Duration controls the gathering timeline; speed controls its curling motion. With zero duration it repeats gathering, a readable pause, and dispersal. Zero speed or intensity leaves ordinary text. Its default 2× capture and GPU-generated filter levels stay within the existing dimension/pixel budgets and remain cached throughout playback; no simulation buffer or per-frame capture is required. The noise atlas is generated and filtered once per renderer on the GPU.

## Text motion and fragmentation

`tide`, `elastic` and `quake` move the shaped text itself around its captured font baseline. Tide travels through the letters with restrained tilt, stretch and oscillation. Elastic squashes in anticipation, rebounds with diminishing overshoot, then rests. Quake delivers two uneven letter impacts and small, balanced vertical aftershocks. They accept `amplitude` (0..2, default 1), along with the shared controls.

`explode` breaks captured glyph ink into a grid of pieces. Each has a seeded spherical trajectory, signed depth and independent rotation around X, Y and Z. Perspective enlarges approaching pieces and shrinks receding ones; a near-plane fade prevents infinite zoom. This is an analytical sprite explosion without physics collisions or depth-buffer occlusion. The pieces preserve source typography and colours, with a brief impact highlight. `pieces` is whole 2..4 per axis (4..16 pieces per glyph, default 3), `depth` and `spin` are 0..2, and `spread` is 0.25..2; all default to 1 except pieces.

```tsx
import Explode from "@text-effects/explode";
import Quake from "@text-effects/quake";
echo(<Explode duration={2400} pieces={3} depth={1.5} spin={1}>
    <Span fontWeight="bold" fontSize={28}>The seal shatters!</Span>
</Explode>);
line.replace("earthquake", <Quake amplitude={1.5}>earthquake</Quake>);
```

All four default to 2400 ms, pane overflow, replacement foreground and no host fades: their own motion timeline starts and finishes with readable text. `duration={0}` repeats the complete cycle. `finish="hold"` stops on restored text. Motion reserves the Span's ordinary line height, selection and hit regions. The default captureScale is 2 for whole-letter motion and 3 for Explode; it remains subject to the usual capture budget. Emphasis is also a production preset; wrap a Span to tune its typography, and use `peak`, `yaw` and `weight` to tune its gesture.

## Arrivals, materials and hero effects

The presets below default to 2400 ms, replacement foreground, pane overflow and their own envelopes (no host fades). Native text, wrapping, fonts, links, selection, copy and logs retain their ordinary positions. Finite arrivals assemble into readable text and stop. Zero duration repeats the authored gesture. Hero effects deliberately occupy adjacent rows; use bounds overflow to confine them to an authored outset.

| Component | Default motion |
| --- | --- |
| implode | Spinning glyph chunks from all four pane corners meet at one instant, pulse and settle |
| wind | Stroke grains curl through a travelling gust, then reconstruct the word |
| sand | Strokes lose support from the bottom upward and settle into small piles, then rebuild |
| recoil | Horizontal anticipation and release, followed by damped rebounds |
| accordion | Alternating folded glyph cards unfold across their reserved line |
| pendulum | A travelling impulse swings letters from hinges at their ink tops |
| domino | Letters tip edge-on in reading order and stand back up |
| tumble | Whole glyphs roll and bounce in from the left |
| magnetic | Disordered hovering letters lock into place in a travelling wave |
| vortex | Intact glyphs spiral around the word, then unfurl onto the line |
| shatter | Actual strokes fracture into angular triangles and reconstruct |
| unravel | Thin strips of source ink curl into ribbons and knit back into glyphs |
| liquefy | Strokes sag into hanging liquid ink, then pull themselves crisp |
| inkBloom | Wet patches spread into the glyphs, briefly bleed and dry sharp |
| duel | Opposing word halves collide into an enlarged pose and broad opposed light bars |
| eclipse | A looming outlined silhouette catches light beneath a rotating elliptical halo |
| dimensionalRift | Edge-on letters turn and unfurl through a vertical slit that closes behind them |
| shockfront | An oversized tilted word rushes in, squashes at impact and launches light bars outward |
| judgement | Long columns of source-glyph pieces descend onto the baseline, followed by a narrow beam and rays |
| ascension | Enlarged text rises above several impressions of its own ink; the impressions converge as it settles |

Implode shares Explode’s `pieces`, `depth`, `spin` and `spread` controls. The other presets accept `amplitude` (0..2, default 1). Wind, Sand, Accordion, Tumble, Magnetic, Vortex, Shatter, Unravel, Liquefy and InkBloom also accept `mode="arrival" | "departure" | "cycle"`. Accordion/Tumble/Magnetic/Vortex/InkBloom default to arrival; the others default to a complete cycle. `direction="left" | "right"` controls Wind’s gust (default right). The grain grid is fixed at eight subdivisions per axis, bounded at 64 pieces per shaped glyph. Larger scale changes trajectories rather than particle count.

```tsx
import Implode from "@text-effects/implode";
import Wind from "@text-effects/wind";
import InkBloom from "@text-effects/inkBloom";
import Sand from "@text-effects/sand";
echo(<Implode pieces={3} spin={1.5}><Span fontWeight="bold">The seal reforms!</Span></Implode>);
line.replace("arrives", <Wind mode="arrival" direction="left">arrives</Wind>);
line.insert(<InkBloom><Span fontFace="serif">An inscription appears.</Span></InkBloom>, 0);
echo(<Sand mode="departure" finish="hold">The inscription crumbles.</Sand>);
```

`finish="remove"` always restores native text when duration ends. `finish="hold"` freezes the authored final frame: arrival/cycle hold readable text, while departure intentionally holds dispersed, folded or melted text. The original characters remain selectable and copyable. Finite holds do not request continuing frames.

Typography gestures pivot on captured font baselines. Emphasis never moves its baseline to fit a vertical edge; it smoothly reduces growth instead. Its horizontal fitting can move enlarged text within the pane. Animated transforms still do not alter native interaction geometry.

These are analytical transformations, without collisions, simulated fluid, depth-buffer occlusion or a backdrop texture. Fragment shaders sample the existing glyph RGBA; their vertex shaders move bounded instanced quads. No effect runs CPU animation or per-frame scripts, and changing phase, scale, amplitude, mode or direction reuses the captured text. Grain and hero overloads still require profiling: a bounded count is not a promise that dozens of simultaneous pane-wide spectacles will meet a monitor’s frame budget.
