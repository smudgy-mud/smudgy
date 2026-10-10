import implodeShader from "./implode.wgsl";
import { component, range } from "./_shared.ts";
import type { ExplodeProps } from "./types.ts";

export const implode = component<ExplodeProps>({ shader: implodeShader, colors: ["#466cc4", "#abdfff", "#ffffff"],
        outset: 400, duration: 2400, replace: true, pane: true, fadeIn: 0, fadeOut: 0, captureScale: () => 3,
    }, props => {
        const pieces = range(props.pieces ?? 3, "pieces", 2, 4);
        if (!Number.isInteger(pieces)) throw new TypeError("pieces must be a whole number");
        return { pieces, depth: range(props.depth ?? 1, "depth", 0, 2),
            spin: range(props.spin ?? 1, "spin", 0, 2), spread: range(props.spread ?? 1, "spread", 0.25, 2) };
    });
export default implode;
