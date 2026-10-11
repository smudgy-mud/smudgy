import ribbonsShader from "./ribbons.wgsl";
import { material } from "./_shared.ts";

export const unravel = material(ribbonsShader, 1, "cycle");
export default unravel;
