import frostShader from "./frost.wgsl";
import { component } from "./_shared.ts";

export const frost = component({ shader: frostShader, colors: ["#326eae", "#91e6ff", "#f2fcff"], outset: 32, duration: 2000, hold: true });
export default frost;
