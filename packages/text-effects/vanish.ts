import shader from "./pressure-ink.wgsl";
import { range, component, transitionMode } from "./_shared.ts";
import type { TransitionProps } from "./types.ts";

/** Letters turn edge-on and slip into a seam; arrival reverses the gesture. */
export const vanish = component<TransitionProps>({ shader,
    colors: ["#425a7b", "#add1ef", "#e9f6ff"], outset: 6, duration: 2400,
    replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 },
    props => ({ variant: 2 + transitionMode(props, "cycle"),
        amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2) }));
export default vanish;
