import { TextEffect, type TextShader } from "smudgy:widgets";
import type { EffectComponent, EffectProps, TransitionMode, TransitionProps } from "./types.ts";

export type Palette = readonly [string, string, string];
interface Defaults<P extends EffectProps> {
    shader: TextShader; colors: Palette; outset: number;
    duration?: number; replace?: boolean; hold?: boolean; pane?: boolean;
    fadeIn?: number; fadeOut?: number;
    captureScale?: (props: P) => number;
}
export function range(value: number, name: string, low: number, high: number): number {
    if (!Number.isFinite(value) || value < low || value > high) throw new TypeError(`${name} must be between ${low} and ${high}`);
    return value;
}
function milliseconds(value: number, name: string): number {
    range(value, name, 0, 3600000);
    if (!Number.isInteger(value)) throw new TypeError(`${name} must be whole milliseconds`);
    return value;
}
function color(value: string): [number, number, number, number] {
    if (typeof value !== "string" || !/^#[0-9a-f]{6}([0-9a-f]{2})?$/i.test(value)) throw new TypeError("Effect colors must be #RRGGBB or #RRGGBBAA");
    return [parseInt(value.slice(1, 3), 16) / 255, parseInt(value.slice(3, 5), 16) / 255,
        parseInt(value.slice(5, 7), 16) / 255, value.length === 9 ? parseInt(value.slice(7, 9), 16) / 255 : 1];
}
export function component<P extends EffectProps = EffectProps>(defaults: Defaults<P>,
    extra: (props: P) => Record<string, number> = () => ({})): EffectComponent<P> {
    return (props = {} as P, children = props.children) => {
        const intensity = range(props.intensity ?? 1, "intensity", 0, 2);
        const speed = range(props.speed ?? 1, "speed", 0, 8);
        const scale = range(props.scale ?? 1, "scale", 0.25, 4);
        const duration = milliseconds(props.duration ?? defaults.duration ?? 0, "duration");
        const finish = props.finish ?? (defaults.hold ? "hold" : "remove");
        const colors = props.colors ?? defaults.colors;
        if (colors.length !== 3) throw new TypeError("Effect colors must contain exactly three colors");
        const fadeIn = milliseconds(props.fadeIn ?? defaults.fadeIn ?? (duration ? Math.min(90, Math.floor(duration * 0.1)) : 0), "fadeIn");
        const fadeOut = milliseconds(props.fadeOut ?? defaults.fadeOut ?? (duration && finish === "remove" ? Math.min(240, Math.floor(duration * 0.2)) : 0), "fadeOut");
        const captureScale = range(props.captureScale ?? defaults.captureScale?.(props) ?? 1, "captureScale", 1, 8);
        return TextEffect({
            shader: defaults.shader, scale, captureScale, duration, fadeIn, fadeOut, finish,
            uniforms: { ...extra(props), intensity, speed, base: color(colors[0]), bright: color(colors[1]), accent: color(colors[2]) },
            outset: props.outset ?? Math.ceil(defaults.outset * scale),
            overflow: props.overflow ?? (defaults.pane ? "pane" : "bounds"),
            composite: defaults.replace ? "replace" : "underlay",
            animated: intensity > 0 && speed > 0,
        }, children);
    };
}
export function transitionMode(props: TransitionProps, fallback: TransitionMode): number {
    const mode = props.mode ?? fallback;
    if (!["arrival", "departure", "cycle"].includes(mode)) throw new TypeError("mode must be arrival, departure or cycle");
    return mode === "arrival" ? 0 : mode === "departure" ? 1 : 2;
}
export const material = (shader: TextShader, variant: number, mode: TransitionMode) => component<TransitionProps>({ shader,
    colors: ["#785296", "#c5a9ec", "#ffffff"], outset: 120, duration: 2400, replace: true,
    pane: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
}, props => ({ variant, amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2), mode: transitionMode(props, mode) }));
