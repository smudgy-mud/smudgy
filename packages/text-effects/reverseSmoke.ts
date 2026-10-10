import reverseSmokeShader from "./reverse-smoke.wgsl";
import { component } from "./_shared.ts";

export const reverseSmoke = component({ shader: reverseSmokeShader, colors: ["#52606e", "#b1bdcc", "#e2e8ed"], outset: 144, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default reverseSmoke;
