import shader from "../attention-bursts.wgsl";
import { component, type Palette } from "../_shared.ts";

export const attentionBurst = (variant: number, colors: Palette) => component({
    shader, colors, duration: 2400, outset: 400, pane: true, fadeIn: 0, fadeOut: 0,
}, () => ({ variant }));
