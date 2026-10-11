import voidBloomShader from "./void-bloom.wgsl";
import { component } from "./_shared.ts";

export const voidBloom = component({ shader: voidBloomShader, colors: ["#05030b", "#593378", "#dcc9ec"],
        outset: 96, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0, captureScale: () => 2 });
export default voidBloom;
