import electricityShader from "./electricity.wgsl";
import { component } from "./_shared.ts";

export const electricity = component({ shader: electricityShader, colors: ["#3263ff", "#80eaff", "#f3ffff"], outset: 12 });
export default electricity;
