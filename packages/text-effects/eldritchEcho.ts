import eldritchShader from "./eldritch.wgsl";
import { component, range } from "./_shared.ts";
import type { MotionProps } from "./types.ts";

export const eldritchEcho = component<MotionProps>({ shader: eldritchShader, colors: ["#27283d", "#6f7eab", "#d5d9e9"],
        outset: 40, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
    }, props => ({ variant: 0, amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2) }));
export default eldritchEcho;
