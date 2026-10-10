import elderSignShader from "./elder-sign.wgsl";
import { component } from "./_shared.ts";

export const elderSign = component({ shader: elderSignShader, colors: ["#070b0b", "#658779", "#d5ead8"],
        outset: 400, duration: 2400, replace: true, pane: true, fadeIn: 0, fadeOut: 0 });
export default elderSign;
