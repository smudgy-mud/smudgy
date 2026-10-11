import shader from "./geas.wgsl";
import { component } from "./_shared.ts";

export const geas = component({ shader, colors: ["#594539", "#b8a282", "#f4dbab"],
    outset: 16, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default geas;
