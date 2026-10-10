import shader from "./poisoned.wgsl";
import { component } from "./_shared.ts";

export const poisoned = component({ shader, colors: ["#705276", "#bbce6c", "#eee1b5"],
    outset: 2, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default poisoned;
