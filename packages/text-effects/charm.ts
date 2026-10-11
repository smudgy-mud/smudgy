import shader from "./charm.wgsl";
import { component } from "./_shared.ts";

export const charm = component({ shader, colors: ["#9b6470", "#eab8a3", "#fff0ce"],
    outset: 12, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default charm;
