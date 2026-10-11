import signalShader from "../kinetic-signals.wgsl";
import { component, range } from "../_shared.ts";
import type { MotionProps } from "../types.ts";

/** Large captured-ink impressions behind an unchanged, immediately readable Span. */
export const kineticSignal = (variant: number, colors: readonly [string, string, string]) =>
    component<MotionProps>({ shader: signalShader, colors, outset: 512, duration: 2400,
        pane: true, fadeIn: 0, fadeOut: 0, captureScale: () => 4,
    }, props => ({ variant, amplitude: range(props.amplitude ?? 1, "amplitude", 0, 2) }));
