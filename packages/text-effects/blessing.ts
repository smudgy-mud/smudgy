import graceShader from "./grace.wgsl";
import { component } from "./_shared.ts";

export const blessing = component({ shader: graceShader, colors: ["#98651c", "#f1c661", "#fff1c9"], outset: 44, duration: 2000 }, () => ({ variant: 1 }));
export default blessing;
