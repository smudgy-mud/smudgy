import runicShader from "./runic-orbit.wgsl";
import { component } from "./_shared.ts";

export const runicOrbit = component({ shader: runicShader, colors: ["#4b519d", "#b4b5f4", "#f5ecff"], outset: 24 });
export default runicOrbit;
