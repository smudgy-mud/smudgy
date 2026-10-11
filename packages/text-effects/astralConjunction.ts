import shader from "./astral-conjunction.wgsl";
import { component } from "./_shared.ts";

export const astralConjunction = component({ shader, colors: ["#8887bd", "#bfd9eb", "#fff8e3"],
    outset: 256, pane: true, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 4 });
export default astralConjunction;
