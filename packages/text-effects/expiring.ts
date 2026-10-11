import { attention } from "./_families/attention.ts";

export const expiring = attention(7, ["#5c6879", "#b1bccd", "#f2d6a4"], 1800);
export default expiring;
