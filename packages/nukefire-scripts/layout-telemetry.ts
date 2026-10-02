/** Missing worker samples carry no information about its memory use. */
export function memorySampleText(bytes: unknown): string {
  return typeof bytes === "number" && Number.isFinite(bytes) && bytes >= 0
    ? `${bytes} B`
    : "unavailable";
}
