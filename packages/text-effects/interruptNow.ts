import shader from "./interrupt-now.wgsl";
import { component } from "./_shared.ts";

export const interruptNow = component({ shader, colors: ["#b44742", "#ecaa72", "#fff0cf"],
    outset: 8, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default interruptNow;
