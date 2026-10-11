import shader from "./phoenix-rebirth.wgsl";
import { component } from "./_shared.ts";

export const phoenixRebirth = component({ shader, colors: ["#803c31", "#f3a33b", "#fff4ce"],
    outset: 256, pane: true, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 4 });
export default phoenixRebirth;
