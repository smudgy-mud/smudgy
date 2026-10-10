import combatShader from "./combat.wgsl";
import { component } from "./_shared.ts";

export const parry = component({ shader: combatShader, colors: ["#626c88", "#bfddf5", "#fff2ca"], outset: 80, duration: 1000 }, () => ({ variant: 2 }));
export default parry;
