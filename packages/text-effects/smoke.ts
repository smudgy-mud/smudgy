import smokeShader from "./smoke.wgsl";
import { component } from "./_shared.ts";

export const smoke = component({ shader: smokeShader, colors: ["#52606e", "#b1bdcc", "#e2e8ed"], outset: 56, duration: 2400, replace: true });
export default smoke;
