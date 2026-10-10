import shader from "./verdant.wgsl";
import { component } from "./_shared.ts";

export const verdant = component({ shader, colors: ["#25543c", "#8ac66b", "#e9edb0"],
    outset: 24, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default verdant;
