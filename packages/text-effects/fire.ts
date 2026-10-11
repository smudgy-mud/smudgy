import fireShader from "./fire.wgsl";
import { component, range } from "./_shared.ts";
import type { FireProps } from "./types.ts";

export const fire = component<FireProps>({ shader: fireShader, colors: ["#ed360a", "#ffb31a", "#fff3bf"], outset: 88 }, props => ({ embers: range(props.embers ?? 1, "embers", 0, 2) }));
export default fire;
