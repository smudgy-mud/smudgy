import sigilShader from "./sigil.wgsl";
import { component } from "./_shared.ts";

export const dispel = component({ shader: sigilShader, colors: ["#63509e", "#b2a0ef", "#f6eeff"], outset: 44, duration: 1600 }, () => ({ variant: 0 }));
export default dispel;
