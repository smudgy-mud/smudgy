import cloudShader from "../cloud.wgsl";
import { component, range, type Palette } from "../_shared.ts";
import type { CloudProps } from "../types.ts";

export const cloud = (symbol: number, colors: Palette) => component<CloudProps>({ shader: cloudShader, colors, outset: 112 }, props => ({
    particles: range(props.particles ?? 1, "particles", 0, 2), swirl: range(props.swirl ?? 1, "swirl", 0, 2), symbol,
}));
