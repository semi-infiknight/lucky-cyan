import type { LayerStyle } from "./types";

const parameters = [
  { source: "inputBrightness", field: "picture_brightness", label: "Brightness", min: -1, max: 1 },
  { source: "inputContrast", field: "contrast", label: "Contrast", min: 0.25, max: 4 },
  { source: "inputAngle", field: "hue_degrees", label: "Hue angle", min: -180, max: 180 },
  { source: "inputSaturation", field: "saturation", label: "Saturation", min: 0, max: 2 },
] as const;

type ColorControls = Pick<LayerStyle, typeof parameters[number]["field"]>;

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    throw new Error("Paste the JSON argument from QLab’s Color Controls reply.");
  }
}

function asObject(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

export function parseQlabColorControls(text: string): ColorControls {
  if (text.length > 4096) throw new Error("Paste one cue’s Color Controls reply, up to 4,096 characters.");
  const decoded = parseJson(text);
  let controls = asObject(typeof decoded === "string" ? parseJson(decoded) : decoded);
  if (controls && Object.prototype.hasOwnProperty.call(controls, "status")) {
    if (controls.status !== "ok") throw new Error("QLab did not return Color Controls values for this cue.");
    if (typeof controls.address === "string" && !controls.address.endsWith("/videoEffect/ColorControls/parameters")) {
      throw new Error("Choose a reply to the Color Controls parameters query.");
    }
    controls = asObject(typeof controls.data === "string" ? parseJson(controls.data) : controls.data);
  }
  if (!controls || Object.keys(controls).length !== parameters.length) {
    throw new Error("The Color Controls reply must contain brightness, contrast, hue angle and saturation.");
  }

  const patch: ColorControls = {};
  for (const { source, field, label, min, max } of parameters) {
    const value = controls[source];
    if (typeof value !== "number" || !Number.isFinite(value) || value < min || value > max) {
      throw new Error(`${label} must be a number between ${min} and ${max}.`);
    }
    patch[field] = value;
  }
  return patch;
}
