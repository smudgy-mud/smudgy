import shader from "./gilding.wgsl";
import { component } from "./_shared.ts";

export const gilding = component({ shader, colors: ["#76501d", "#ffd878", "#fff8dc"],
    outset: 2, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default gilding;
