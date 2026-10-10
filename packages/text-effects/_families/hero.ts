import heroShader from "../hero.wgsl";
import { component, range } from "../_shared.ts";
import type { MotionProps } from "../types.ts";

export const hero = (variant: number) => component<MotionProps>({ shader: heroShader,
    colors: ["#777da6", "#d8e4ff", "#ffffff"], outset: 400, duration: 2400, replace: true,
    pane: true, fadeIn: 0, fadeOut: 0, captureScale: () => 3,
}, props => ({ variant, amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2) }));
