import combatShader from "./combat.wgsl";
import { component } from "./_shared.ts";

export const cleave = component({ shader: combatShader, colors: ["#9d4027", "#ffba72", "#fff5dd"], outset: 48, duration: 1200 }, () => ({ variant: 3 }));
export default cleave;
