import swarmShader from "./swarm.wgsl";
import { component, range } from "./_shared.ts";
import type { SwarmProps } from "./types.ts";

export const swarm = component<SwarmProps>({ shader: swarmShader, colors: ["#866321", "#ffcc55", "#f4f3df"], outset: 40 }, props => ({ particles: range(props.particles ?? 1, "particles", 0, 2) }));
export default swarm;
