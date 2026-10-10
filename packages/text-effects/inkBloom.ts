import inkShader from "./ink.wgsl";
import { material } from "./_shared.ts";

export const inkBloom = material(inkShader, 1, "arrival");
export default inkBloom;
