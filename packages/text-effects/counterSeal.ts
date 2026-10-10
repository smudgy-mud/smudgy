import ritualNetsShader from "./ritual-nets.wgsl";
import { component } from "./_shared.ts";

export const counterSeal = component({ shader: ritualNetsShader, colors: ["#c94744", "#72ccd8", "#edffff"],
        outset: 64, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
    }, () => ({ variant: 1 }));
export default counterSeal;
