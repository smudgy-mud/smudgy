import soulDrainShader from "./soul-drain.wgsl";
import { component } from "./_shared.ts";

export const soulDrain = component({ shader: soulDrainShader, colors: ["#214e54", "#77c8ca", "#d9fff0"],
        outset: 96, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default soulDrain;
