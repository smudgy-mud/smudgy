import shader from "./transmuted-ink.wgsl";
import { component } from "./_shared.ts";

/** Corrosion pits exposed stroke edges and sheds captured fragments of ink. */
export const decay = component({ shader, colors: ["#625846", "#c99a61", "#dfc49a"],
    outset: 14, duration: 2400, replace: true, fadeIn: 0, fadeOut: 0,
    captureScale: () => 2 }, () => ({ variant: 2 }));
export default decay;
