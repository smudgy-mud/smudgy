import shader from "./transmuted-ink.wgsl";
import { component } from "./_shared.ts";

/** Translucent fractured ink, with travelling prism reflections. */
export const crystal = component({ shader, colors: ["#294f89", "#90d9ed", "#fff7ff"],
    outset: 4, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0,
    captureScale: () => 2 }, () => ({ variant: 0 }));
export default crystal;
