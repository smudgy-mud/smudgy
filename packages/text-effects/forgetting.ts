import forgettingShader from "./forgetting.wgsl";
import { component } from "./_shared.ts";

export const forgetting = component({ shader: forgettingShader, colors: ["#4b5361", "#9aa8ba", "#dfe5ef"],
        outset: 24, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default forgetting;
