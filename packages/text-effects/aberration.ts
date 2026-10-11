import eldritchShader from "./eldritch.wgsl";
import { component, range } from "./_shared.ts";
import type { MotionProps } from "./types.ts";

export const aberration = component<MotionProps>({ shader: eldritchShader, colors: ["#352441", "#997fae", "#e7d4e9"],
        outset: 40, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
    }, props => ({ variant: 1, amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2) }));
export default aberration;
