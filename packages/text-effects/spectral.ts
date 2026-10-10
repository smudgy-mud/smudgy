import spectralShader from "./spectral.wgsl";
import { component } from "./_shared.ts";

export const spectral = component({ shader: spectralShader, colors: ["#4c6894", "#a5d1e3", "#f0fbff"], outset: 36, replace: true });
export default spectral;
