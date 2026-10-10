import ritualShader from "./ritual.wgsl";
import { component } from "./_shared.ts";

export const spellSeal = component({ shader: ritualShader, colors: ["#756038", "#ddc27a", "#fff4d1"],
        outset: 64, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0,
    }, () => ({ variant: 0 }));
export default spellSeal;
