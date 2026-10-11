import sigilShader from "./sigil.wgsl";
import { component } from "./_shared.ts";

export const divineShield = component({ shader: sigilShader, colors: ["#9c7224", "#f5d176", "#fff7dd"], outset: 18, duration: 2200 }, () => ({ variant: 1 }));
export default divineShield;
