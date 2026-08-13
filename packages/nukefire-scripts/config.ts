import { get } from "smudgy:params";

export type PanelKey =
  | "hud"
  | "affects"
  | "comms"
  | "map"
  | "radar"
  | "atlas"
  | "deck"
  | "codex";

export type ChatRendering = "full-ansi" | "plain";
export type SessionLayout = "stacked-right" | "tabbed";
export type SessionVitalsLayout = "compact" | "wide";

const MIN_FONT_SIZE = 8;
const MAX_FONT_SIZE = 40;
const DEFAULT_WIDGET_FONT_SIZE = 12;

function boolParam(key: string, fallback: boolean): boolean {
  const value = get(key);
  return typeof value === "boolean" ? value : fallback;
}

function numberParam(key: string, fallback: number): number {
  const value = get(key);
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, Math.round(value)))
    : fallback;
}

function stringParam(key: string): string | undefined {
  const value = get(key);
  return typeof value === "string" ? value : undefined;
}

/** Panels mounted by the elected primary NukeFire session. */
export const panelVisibility: Readonly<Record<PanelKey, boolean>> = {
  hud: boolParam("showHud", true),
  affects: boolParam("showAffects", true),
  comms: boolParam("showComms", true),
  map: boolParam("showMap", true),
  radar: boolParam("showRadar", false),
  atlas: boolParam("showAtlas", true),
  deck: boolParam("showDeck", true),
  codex: boolParam("showCodex", false),
};

/** Terminal scrollback size for the Comms pane. */
export const chatFontSize = numberParam("chatFontSize", 12);

/** Baseline used to scale the package's existing widget type hierarchy. */
export const widgetFontSize = numberParam("widgetFontSize", DEFAULT_WIDGET_FONT_SIZE);

/** Scale an old 12px-based widget size while preserving headings and captions. */
export function widgetTextSize(previousSize: number): number {
  return Math.min(
    MAX_FONT_SIZE,
    Math.max(MIN_FONT_SIZE, Math.round(previousSize * widgetFontSize / DEFAULT_WIDGET_FONT_SIZE)),
  );
}

/** Scale fixed widget metrics which must grow with the configured text. */
export function widgetMetric(previousSize: number): number {
  return Math.max(1, Math.round(previousSize * widgetFontSize / DEFAULT_WIDGET_FONT_SIZE));
}

export const chatRendering: ChatRendering = stringParam("chatRendering") === "plain"
  ? "plain"
  : "full-ansi";

export const sessionLayout: SessionLayout = stringParam("sessionLayout") === "stacked-right"
  ? "stacked-right"
  : "tabbed";

export const sessionVitalsLayout: SessionVitalsLayout = stringParam("sessionVitalsLayout") === "compact"
  ? "compact"
  : "wide";
