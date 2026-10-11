import blackTideShader from "./black-tide.wgsl";
import { component } from "./_shared.ts";

export const blackTide = component({ shader: blackTideShader, colors: ["#050710", "#484459", "#c9b9db"],
        outset: 28, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0 });
export default blackTide;
