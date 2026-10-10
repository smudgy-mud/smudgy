import watchersShader from "./watchers.wgsl";
import { component } from "./_shared.ts";

export const watchers = component({ shader: watchersShader, colors: ["#0c1015", "#6a9195", "#d8e7db"],
        outset: 24, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default watchers;
