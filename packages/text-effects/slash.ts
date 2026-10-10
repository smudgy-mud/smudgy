import combatShader from "./combat.wgsl";
import { component } from "./_shared.ts";

export const slash = component({ shader: combatShader, colors: ["#23518c", "#8acbff", "#ffffff"], outset: 52, duration: 900 }, () => ({ variant: 0 }));
export default slash;
