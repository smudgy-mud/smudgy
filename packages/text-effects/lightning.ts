import lightningShader from "./lightning.wgsl";
import { component } from "./_shared.ts";

export const lightning = component({ shader: lightningShader, colors: ["#704bff", "#9cadff", "#ffffff"], outset: 40 });
export default lightning;
