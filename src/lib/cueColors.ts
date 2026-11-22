import type { CueColor, CueSummary } from "./types";

export const CUE_COLOR_SWATCHES: Record<CueColor, string> = {
  none: "transparent",
  crimson: "#a92438",
  red: "#ef4444",
  orange: "#f97316",
  peach: "#ffb086",
  yellow: "#eab308",
  olive: "#8a8c35",
  green: "#22c55e",
  forest: "#1a694d",
  cyan: "#06b6d4",
  sky_blue: "#79bce8",
  blue: "#3b82f6",
  indigo: "#5354ba",
  midnight: "#283d72",
  lavender: "#c5a6ed",
  purple: "#a855f7",
  plum: "#773974",
  magenta: "#c91e94",
  hot_pink: "#ff5c9f",
  pink: "#ec4899",
  berry: "#a83267",
  white: "#f1f5f9",
  gray: "#84909a",
  black: "#334155",
};

export const COLOR_OPTIONS = (Object.entries(CUE_COLOR_SWATCHES) as [CueColor, string][])
  .map(([value, hex]) => ({
    value,
    hex,
    label: value.split("_").map(word => word[0].toUpperCase() + word.slice(1)).join(" "),
  }));

export function cueAccent({ color, display_color }: Pick<CueSummary, "color" | "display_color">): string {
  return CUE_COLOR_SWATCHES[display_color ?? color] ?? "transparent";
}
