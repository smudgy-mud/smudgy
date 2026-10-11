import trueSightShader from "./true-sight.wgsl";
import { component } from "./_shared.ts";

export const trueSight = component({ shader: trueSightShader, colors: ["#66557c", "#8bbfca", "#efffff"],
        outset: 32, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default trueSight;
