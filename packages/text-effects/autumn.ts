import autumnShader from "./autumn.wgsl";
import { component, range, transitionMode } from "./_shared.ts";
import type { TransitionProps } from "./types.ts";

export const autumn = component<TransitionProps>({ shader: autumnShader, colors: ["#984625", "#db9e39", "#f7d08d"],
        outset: 160, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
    }, props => ({ amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2), mode: transitionMode(props, "cycle") }));
export default autumn;
