import teleportShader from "./teleport.wgsl";
import { component } from "./_shared.ts";

export const teleport = component({ shader: teleportShader, colors: ["#4846ab", "#8fdce8", "#e3e8ff"], outset: 64, duration: 2400, replace: true });
export default teleport;
