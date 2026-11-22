// Shared helpers for fixture colour mixing (Dashboard + Light Cue inspector).
import type { ColorLevels, FixtureColorChannels, FixtureColorModel, FixtureGroup, PatchedChannel, PatchedFixture } from "./types";
import { channelSlots, fixtureChannelFits, fixtureParameterAddress } from "./fixtureChannels";

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));
const to255 = (v: number) => Math.round(clamp01(v) * 255);
const hex2 = (n: number) => n.toString(16).padStart(2, "0");

/** Normalised r,g,b (0–1) → "#rrggbb". */
export function rgbToHex(r: number, g: number, b: number): string {
  return `#${hex2(to255(r))}${hex2(to255(g))}${hex2(to255(b))}`;
}

/** "#rrggbb" → normalised [r, g, b] (0–1). */
export function hexToRgb(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16);
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

/** Index of the first parameter of a given kind, or -1. */
export function paramIndexOfKind(params: { kind: string }[], kind: string): number {
  return params.findIndex((p) => p.kind === kind);
}

/** A picker edits all channels assigned an RGB role, including repeated emitters. */
export function rgbChannelChanges(params: { kind: string }[], hex: string) {
  const [red, green, blue] = hexToRgb(hex);
  const values: Record<string, number> = { red, green, blue };
  return params.flatMap((p, paramIndex) =>
    Object.prototype.hasOwnProperty.call(values, p.kind) ? [{ paramIndex, value: values[p.kind] }] : []);
}

const colorModels = ["cmy", "hsi", "hsv"] as const;
const colorRoles = {
  cmy: ["cyan", "magenta", "yellow"],
  hsi: ["hue", "saturation", "hsi_intensity"],
  hsv: ["hue", "saturation", "hsv_value"],
} as const;

type ColorMember = FixtureColorChannels & { indices: [number, number, number] };
export type DisplayColorLevels = [number | null, number | null, number | null];

export function cmyToHex([cyan, magenta, yellow]: ColorLevels): string {
  return rgbToHex(1 - cyan, 1 - magenta, 1 - yellow);
}

export function hexToCmy(hex: string): ColorLevels {
  const [red, green, blue] = hexToRgb(hex);
  return [1 - red, 1 - green, 1 - blue];
}

/** Approximate an HSI swatch. The physical fixture receives HSI levels directly. */
export function hsiToHex([hue, saturation, intensity]: ColorLevels): string {
  const phase = (hue % 1) * 3;
  const sector = Math.floor(phase);
  const angle = (phase - sector) * 2 * Math.PI / 3;
  const low = intensity * (1 - saturation);
  const primary = intensity * (1 + saturation * Math.cos(angle) / Math.cos(Math.PI / 3 - angle));
  const secondary = 3 * intensity - primary - low;
  if (sector === 0) return rgbToHex(primary, secondary, low);
  if (sector === 1) return rgbToHex(low, primary, secondary);
  return rgbToHex(secondary, low, primary);
}

/** Approximate an HSV swatch. The physical fixture receives HSV levels directly. */
export function hsvToHex([hue, saturation, value]: ColorLevels): string {
  const phase = (clamp01(hue) % 1) * 6;
  const chroma = value * saturation;
  const secondary = chroma * (1 - Math.abs(phase % 2 - 1));
  const offset = value - chroma;
  const sectors = [
    [chroma, secondary, 0], [secondary, chroma, 0], [0, chroma, secondary],
    [0, secondary, chroma], [secondary, 0, chroma], [chroma, 0, secondary],
  ];
  const [red, green, blue] = sectors[Math.floor(phase)] ?? [0, 0, 0];
  return rgbToHex(red + offset, green + offset, blue + offset);
}

/** Resolve a complete, unambiguous native color group by explicit parameter roles. */
export function fixtureColorBinding(fixture: PatchedFixture, model: FixtureColorModel): {
  indices: [number, number, number];
  channels: [PatchedChannel, PatchedChannel, PatchedChannel];
} | null {
  const params = fixture.fixture_type.parameters;
  const otherHueRole = model === "hsi" ? "hsv_value" : "hsi_intensity";
  if (model !== "cmy" && params.some(param => param.kind === otherHueRole)) return null;
  const roles = colorRoles[model];
  const indices = roles.map(role => params.flatMap((param, index) => param.kind === role ? [index] : []));
  if (indices.some(matches => matches.length !== 1)) return null;
  const [first, second, third] = indices.map(matches => matches[0]);
  const selected: [number, number, number] = [first, second, third];
  const addresses = params.map(param => fixtureParameterAddress(fixture, param));

  if (!Number.isInteger(fixture.base_address) || fixture.base_address < 1 ||
      selected.some(index => {
        const address = addresses[index];
        const slots = channelSlots(address);
        return !fixtureChannelFits(address) || addresses.some((other, otherIndex) =>
          otherIndex !== index && channelSlots(other).some(slot => slots.includes(slot)));
      })) return null;
  return { indices: selected, channels: [addresses[first], addresses[second], addresses[third]] };
}

export function fixtureColorBindings(fixture: PatchedFixture) {
  return colorModels.flatMap(model => {
    const binding = fixtureColorBinding(fixture, model);
    return binding ? [{ model, ...binding }] : [];
  });
}

function slots(address: PatchedChannel): string[] {
  return channelSlots(address).map(slot => `${address.universe}/${slot}`);
}

/** Each member must support the same mix. Color slots must be exclusive to their fixture. */
export function groupColorBindings(group: FixtureGroup, fixtures: PatchedFixture[]) {
  const members = group.fixture_ids.map(id => fixtures.find(fixture => fixture.id === id));
  if (!members.length || new Set(group.fixture_ids).size !== members.length) return [];

  return colorModels.flatMap(model => {
    const occupied = new Map<string, string>();
    const mapping: ColorMember[] = [];
    for (const fixture of members) {
      if (!fixture) return [];
      const binding = fixtureColorBinding(fixture, model);
      if (!binding) return [];
      mapping.push({ id: fixture.id, ...binding });
      for (const slot of binding.channels.flatMap(slots)) {
        if (occupied.has(slot)) return [];
        occupied.set(slot, fixture.id);
      }
    }
    for (const fixture of fixtures) {
      for (const param of fixture.fixture_type.parameters) {
        const address = fixtureParameterAddress(fixture, param);
        if (fixture.base_address < 1 || !fixtureChannelFits(address)) continue;
        const conflicts = slots(address).some(slot => {
          const owner = occupied.get(slot);
          return owner !== undefined && owner !== fixture.id;
        });
        if (conflicts) return [];
      }
    }
    return [{ model, mapping }];
  });
}

/** A component is displayed only when every member currently has that exact level. */
export function groupColorLevels(mapping: ColorMember[], values: Record<string, number[]>): DisplayColorLevels {
  const first = mapping[0];
  if (!first) return [null, null, null];
  const common = (component: 0 | 1 | 2) => {
    const level = values[first.id]?.[first.indices[component]] ?? 0;
    return mapping.every(member => (values[member.id]?.[member.indices[component]] ?? 0) === level) ? level : null;
  };
  return [common(0), common(1), common(2)];
}
