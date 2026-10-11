import graceShader from "./grace.wgsl";
import { component } from "./_shared.ts";

export const healWave = component({ shader: graceShader, colors: ["#147858", "#6febba", "#e4fff5"], outset: 40, duration: 1800 }, () => ({ variant: 0 }));
export default healWave;
