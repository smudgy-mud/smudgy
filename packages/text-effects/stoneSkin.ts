import shader from "./stone-skin.wgsl";
import { component } from "./_shared.ts";

export const stoneSkin = component({ shader, colors: ["#3e4448", "#b2b9b1", "#ebe7ce"],
    outset: 2, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default stoneSkin;
