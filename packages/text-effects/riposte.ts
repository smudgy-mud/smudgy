import combatShader from "./combat.wgsl";
import { component } from "./_shared.ts";

export const riposte = component({ shader: combatShader, colors: ["#315287", "#9fdcf0", "#ffffff"], outset: 42, duration: 1000 }, () => ({ variant: 4 }));
export default riposte;
