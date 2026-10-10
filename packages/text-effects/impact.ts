import impactShader from "./impact.wgsl";
import { component } from "./_shared.ts";

export const impact = component({ shader: impactShader, colors: ["#dc5724", "#ffd35c", "#fff7df"], outset: 260, duration: 1000 });
export default impact;
