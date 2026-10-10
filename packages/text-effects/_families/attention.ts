import attentionShader from "../attention.wgsl";
import { component, type Palette } from "../_shared.ts";

export const attention = (variant: number, colors: Palette, duration: number, pane = true) => component({
    shader: attentionShader, colors, outset: pane ? 400 : 36, duration, pane,
}, () => ({ variant }));
