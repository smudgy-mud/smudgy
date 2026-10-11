import firefliesShader from "./fireflies.wgsl";
import { component, range } from "./_shared.ts";
import type { FirefliesProps } from "./types.ts";

export const fireflies = component<FirefliesProps>({ shader: firefliesShader, colors: ["#637b1d", "#d6e66b", "#ffffd8"], outset: 32 }, props => ({ particles: range(props.particles ?? 1, "particles", 0, 2) }));
export default fireflies;
