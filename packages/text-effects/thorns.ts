import thornsShader from "./thorns.wgsl";
import { component } from "./_shared.ts";

export const thorns = component({ shader: thornsShader, colors: ["#3c4f26", "#83b34d", "#e0c886"], outset: 24, duration: 2000, hold: true, replace: true });
export default thorns;
