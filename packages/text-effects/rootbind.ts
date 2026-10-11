import rootbindShader from "./rootbind.wgsl";
import { component } from "./_shared.ts";

export const rootbind = component({ shader: rootbindShader, colors: ["#493b25", "#8b9953", "#ddd1a1"],
        outset: 28, duration: 2400, hold: true, replace: true, fadeIn: 0, captureScale: () => 2 });
export default rootbind;
