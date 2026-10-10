import livingInkShader from "./living-ink.wgsl";
import { component } from "./_shared.ts";

export const barkMend = component({ shader: livingInkShader, colors: ["#58402a", "#9bbb6b", "#e0f1ad"],
        outset: 12, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2,
    }, () => ({ variant: 1 }));
export default barkMend;
