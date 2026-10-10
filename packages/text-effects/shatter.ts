import ribbonsShader from "./ribbons.wgsl";
import { material } from "./_shared.ts";

export const shatter = material(ribbonsShader, 0, "cycle");
export default shatter;
