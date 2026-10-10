import grainsShader from "../grains.wgsl";
import { component, range, transitionMode } from "../_shared.ts";
import type { WindProps } from "../types.ts";

export const grain = (variant: number) => component<WindProps>({ shader: grainsShader,
    colors: ["#a17743", "#dfba7d", "#fff1cd"], outset: 400, duration: 2400, replace: true,
    pane: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
}, props => {
    const direction = props.direction ?? "right";
    if (!["left", "right"].includes(direction)) throw new TypeError("direction must be left or right");
    return { variant, amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2),
        mode: transitionMode(props, "cycle"), direction: direction === "left" ? -1 : 1 };
});
