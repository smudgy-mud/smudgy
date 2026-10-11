import type { TextEffectChildren, InlineElement } from "smudgy:widgets";

export interface EffectProps {
    /** Strength, 0..2. */
    intensity?: number;
    /** Procedural motion, 0..8. Duration independently controls the playback timeline. */
    speed?: number;
    /** Artwork size, 0.25..4. Keeps font metrics, line height and particle count unchanged. Emphasis scales painted growth. */
    scale?: number;
    /** Base, bright and accent colours, as #RRGGBB or #RRGGBBAA. */
    colors?: readonly [string, string, string];
    /** Milliseconds, 0..3600000. Zero repeats/continues until removed. */
    duration?: number;
    /** Attack/release in milliseconds; overlapping fades are shortened to fit duration. */
    fadeIn?: number;
    fadeOut?: number;
    /** Remove restores plain text; hold freezes the final image. Defaults to remove, except frost, thorns and rootbind. */
    finish?: "remove" | "hold";
    /** Most attention presets default to pane. The text anchor must remain visible. */
    overflow?: "bounds" | "pane";
    /** Explicit paint overflow, 0..2048 logical pixels. Automatic overflow follows scale. */
    outset?: number;
    /** Cached glyph resolution, 1..8. Emphasis chooses this from its maximum growth. */
    captureScale?: number;
    children?: TextEffectChildren;
}
export interface MotionProps extends EffectProps { /** Motion amplitude, 0..2, default 1. */ amplitude?: number; }
export type TransitionMode = "arrival" | "departure" | "cycle";
export interface TransitionProps extends MotionProps {
    /** Arrival assembles, departure disperses, cycle returns to ordinary text. */ mode?: TransitionMode;
}
export interface WindProps extends TransitionProps { /** Gust direction, default right. */ direction?: "left" | "right"; }
export interface ExplodeProps extends EffectProps {
    /** Grid subdivisions per glyph axis, whole 2..4; default 3 (nine pieces). */ pieces?: number;
    /** Signed depth travel, 0..2; zero keeps particles in the text plane. */ depth?: number;
    /** Three-axis tumble, 0..2. */ spin?: number;
    /** Outward travel, 0.25..2. */ spread?: number;
}
export interface FireProps extends EffectProps { /** Ember density, 0..2. */ embers?: number; }
export interface EmphasisProps extends EffectProps {
    /** Peak text magnification, 1..8, default 3; intensity and scale multiply its growth. */
    peak?: number;
    /** Maximum yaw in radians, -1..1, default 0.35. */
    yaw?: number;
    /** Additional ink weight, 0..2, default 0.5. Paints heavier strokes without changing font layout. */
    weight?: number;
    /** Horizontal growth pivot, default center. */
    anchor?: "start" | "center" | "end";
    /** Fit enlarged text horizontally and limit growth at vertical edges, keeping its baseline; default true. */
    fitToPane?: boolean;
}
export interface CloudProps extends EffectProps {
    /** Symbol density, 0..2; zero leaves only cloud puffs. */ particles?: number;
    /** Horizontal swirl strength, 0..2. */ swirl?: number;
}
export interface FirefliesProps extends EffectProps { /** Firefly density, 0..2, at most 24. */ particles?: number; }
export interface SwarmProps extends EffectProps { /** Insect density, 0..2, at most 24. */ particles?: number; }
export type PoisonCloudProps = CloudProps;
export type EffectComponent<P extends EffectProps = EffectProps> = (props?: P, children?: TextEffectChildren) => InlineElement;
