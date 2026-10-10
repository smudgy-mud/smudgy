import inkShader from "./ink.wgsl";
import { material } from "./_shared.ts";

export const liquefy = material(inkShader, 0, "cycle");
export default liquefy;
