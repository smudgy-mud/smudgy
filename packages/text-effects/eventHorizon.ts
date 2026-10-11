import shader from "./event-horizon.wgsl";
import { component } from "./_shared.ts";

/** A dark aperture stretches text into orbiting ribbons, then reconstructs it. */
export const eventHorizon = component({ shader, colors: ["#514578", "#dfa76b", "#fff2da"],
    outset: 512, duration: 2400, replace: true, pane: true, fadeIn: 0, fadeOut: 0,
    captureScale: () => 3 });
export default eventHorizon;
