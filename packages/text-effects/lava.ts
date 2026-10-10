import shader from "./transmuted-ink.wgsl";
import { component } from "./_shared.ts";

/** Molten strokes cool into fractured crust, then release the original lettering. */
export const lava = component({ shader, colors: ["#4c4545", "#ff4d14", "#fff0a1"],
    outset: 4, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0,
    captureScale: () => 2 }, () => ({ variant: 1 }));
export default lava;
