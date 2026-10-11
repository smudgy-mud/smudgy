import combatShader from "./combat.wgsl";
import { component } from "./_shared.ts";

export const critical = component({ shader: combatShader, colors: ["#a02b1c", "#ffba62", "#fff5d6"], outset: 88, duration: 1100 }, () => ({ variant: 1 }));
export default critical;
