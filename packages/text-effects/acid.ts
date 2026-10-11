import acidShader from "./acid.wgsl";
import { component } from "./_shared.ts";

export const acid = component({ shader: acidShader, colors: ["#4d1a78", "#ad5be8", "#edd7ff"], outset: 40 });
export default acid;
