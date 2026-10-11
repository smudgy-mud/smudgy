import ritualNetsShader from "./ritual-nets.wgsl";
import { component } from "./_shared.ts";

export const ritualKnot = component({ shader: ritualNetsShader, colors: ["#4b376e", "#ab86da", "#f1d9ff"],
        outset: 64, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
    }, () => ({ variant: 0 }));
export default ritualKnot;
