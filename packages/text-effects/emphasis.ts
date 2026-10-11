import emphasisShader from "./emphasis.wgsl";
import { component, range } from "./_shared.ts";
import type { EmphasisProps } from "./types.ts";

export const emphasis = component<EmphasisProps>({ shader: emphasisShader, colors: ["#7192a8", "#e6f2ff", "#ffffff"], outset: 400,
        duration: 2400, replace: true, pane: true, fadeIn: 0, fadeOut: 0,
        captureScale: props => Math.min(8, 1 + (props.peak ?? 3) * (props.intensity ?? 1) * (props.scale ?? 1)),
    }, props => {
        const anchor = props.anchor ?? "center";
        if (!["start", "center", "end"].includes(anchor)) throw new TypeError("emphasis anchor must be start, center or end");
        if (props.fitToPane !== undefined && typeof props.fitToPane !== "boolean") throw new TypeError("emphasis fitToPane must be boolean");
        return { peak: range(props.peak ?? 3, "peak", 1, 8), yaw: range(props.yaw ?? 0.35, "yaw", -1, 1),
            weight: range(props.weight ?? 0.5, "weight", 0, 2), pivot: anchor === "start" ? 0 : anchor === "end" ? 1 : 0.5,
            fit: props.fitToPane === false ? 0 : 1 };
    });
export default emphasis;
