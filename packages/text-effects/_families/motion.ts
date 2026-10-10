import motionShader from "../motion.wgsl";
import { component, range } from "../_shared.ts";
import type { MotionProps } from "../types.ts";

export const motion = (variant: number) => component<MotionProps>({ shader: motionShader,
    colors: ["#7192a8", "#e6f2ff", "#ffffff"], outset: 96, duration: 2400, replace: true,
    pane: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
}, props => ({ variant, amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2) }));
