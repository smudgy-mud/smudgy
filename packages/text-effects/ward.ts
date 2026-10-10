import wardShader from "./ward.wgsl";
import { component } from "./_shared.ts";

export const ward = component({ shader: wardShader, colors: ["#435bbd", "#a28cff", "#e8e0ff"], outset: 14 });
export default ward;
