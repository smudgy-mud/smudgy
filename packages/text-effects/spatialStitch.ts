import shader from "./spatial-stitch.wgsl";
import { component } from "./_shared.ts";

export const spatialStitch = component({ shader, colors: ["#446a71", "#b3d7cd", "#f2f0d7"],
    outset: 16, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default spatialStitch;
