import ritualShader from "./ritual.wgsl";
import { component } from "./_shared.ts";

export const leyWeave = component({ shader: ritualShader, colors: ["#315887", "#82d5d5", "#e9ffff"],
        outset: 48, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0,
    }, () => ({ variant: 1 }));
export default leyWeave;
