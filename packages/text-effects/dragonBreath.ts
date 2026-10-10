import shader from "./dragon-breath.wgsl";
import { component } from "./_shared.ts";

/** A curling fire front sweeps across the pane, scorching then releasing the ink. */
export const dragonBreath = component({ shader, colors: ["#7d241d", "#ff751c", "#fff2c1"],
    outset: 512, duration: 2400, replace: true, pane: true, fadeIn: 0, fadeOut: 0,
    captureScale: () => 2 });
export default dragonBreath;
