import silenceShader from "./silence.wgsl";
import { component } from "./_shared.ts";

export const silence = component({ shader: silenceShader, colors: ["#475363", "#9aaabb", "#dbe2eb"],
        outset: 24, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default silence;
