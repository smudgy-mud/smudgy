import animeShader from "./anime-impact.wgsl";
import { component } from "./_shared.ts";

export const animeImpact = component({ shader: animeShader, colors: ["#dce6ef", "#ffffff", "#ffffff"], outset: 400, duration: 2600, pane: true });
export default animeImpact;
