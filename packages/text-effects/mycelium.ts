import livingInkShader from "./living-ink.wgsl";
import { component } from "./_shared.ts";

export const mycelium = component({ shader: livingInkShader, colors: ["#304e40", "#84ba98", "#ede8c8"],
        outset: 36, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
    }, () => ({ variant: 0 }));
export default mycelium;
