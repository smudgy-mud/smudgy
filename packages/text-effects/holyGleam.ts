import holyShader from "./holy-gleam.wgsl";
import { component } from "./_shared.ts";

export const holyGleam = component({ shader: holyShader, colors: ["#d68912", "#ffe8a0", "#ffffff"], outset: 320, duration: 1800 });
export default holyGleam;
