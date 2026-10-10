import possessionShader from "./possession.wgsl";
import { component } from "./_shared.ts";

export const possession = component({ shader: possessionShader, colors: ["#592d35", "#cc6558", "#ffe2b9"],
        outset: 24, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default possession;
