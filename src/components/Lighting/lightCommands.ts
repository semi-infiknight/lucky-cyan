import type { FixtureGroup, ParamKind, ParamTarget, PatchedFixture } from "../../lib/types";

export type LevelUnit = "percent" | "dmx8" | "dmx16";
type FixtureTarget = Extract<ParamTarget, { kind: "fixture" }>;

export interface CommandPreview {
  targets: FixtureTarget[];
  errors: string[];
  commands: number;
}

const MAX_COMMANDS = 4096;
const key = (text: string) => text.trim().toLowerCase();

function expandSelector(text: string): string[] {
  const bracketed = text.startsWith("[") && text.endsWith("]");
  const portions = (bracketed ? text.slice(1, -1) : text).split(",");
  const names: string[] = [];
  for (const part of portions) {
    if (names.length >= MAX_COMMANDS) throw new Error("Select up to 4096 instruments on one line");
    const token = part.trim();
    const range = /^(\d+)\s*-\s*(\d+)$/.exec(token);
    if (!range) {
      if (!token || /[[\]]/.test(token)) throw new Error("Invalid instrument or group selector");
      names.push(token);
      continue;
    }
    const first = Number(range[1]);
    const last = Number(range[2]);
    if (!Number.isSafeInteger(first) || !Number.isSafeInteger(last) || last < first || last - first >= MAX_COMMANDS) {
      throw new Error("Use an ascending range of up to 4096 instrument numbers");
    }
    if (names.length + last - first + 1 > MAX_COMMANDS) throw new Error("Select up to 4096 instruments on one line");
    for (let i = first; i <= last; i++) names.push(String(i));
  }
  return names;
}

function numericLevel(text: string, unit: LevelUnit): number {
  const match = /^(\d+(?:\.\d+)?|\.\d+)\s*(%)?$/.exec(text);
  if (!match) throw new Error("Use a numeric level, home, or pass. Cue pulls and expressions require manual mapping");
  const amount = Number(match[1]);
  const usingPercent = Boolean(match[2]) || unit === "percent";
  const maximum = usingPercent ? 100 : unit === "dmx8" ? 255 : 65535;
  if (amount > maximum || (!usingPercent && !Number.isInteger(amount))) {
    throw new Error(`Level must be ${usingPercent ? "a percentage" : "an integer"} from 0 to ${maximum}`);
  }
  return amount / maximum;
}

/** Resolve the operator's text entirely against the workspace patch. Nothing here sends DMX. */
export function previewLightCommands(
  source: string, fixtures: PatchedFixture[], groups: FixtureGroup[],
  defaultParameter: string, unit: LevelUnit,
): CommandPreview {
  const updates = new Map<string, FixtureTarget>();
  const errors: string[] = [];
  const byId = new Map(fixtures.map(f => [f.id, f]));
  const selectors = new Map<string, (FixtureGroup | PatchedFixture)[]>();
  for (const entry of [...fixtures, ...groups]) {
    const label = key(entry.label);
    selectors.set(label, [...(selectors.get(label) ?? []), entry]);
  }
  let count = 0;
  let expanded = 0;
  if (source.length > 256_000) return { errors: ["Command sheet exceeds 256,000 characters"], targets: [], commands: 0 };

  for (const [index, raw] of source.split(/\r\n?|\n/).entries()) {
    const line = raw.trim();
    if (!line) continue;
    if (++count > MAX_COMMANDS) {
      errors.push("Use up to 4096 commands in one cue");
      break;
    }
    try {
      const assignment = /^([^=]+)=([^=]+)$/.exec(line);
      if (!assignment) throw new Error("Expected instrument.parameter = value");
      const selector = assignment[1].trim();
      // A fixture name may contain a period. Prefer a complete matching name.
      const dot = selectors.has(key(selector)) ? -1 : selector.lastIndexOf(".");
      const subject = dot < 0 ? selector : selector.slice(0, dot).trim();
      const parameter = key(dot < 0 ? defaultParameter : selector.slice(dot + 1));
      const value = key(assignment[2]);
      const level = value === "pass" || value === "home" ? null : numericLevel(value, unit);
      const names = selectors.has(key(subject)) ? [subject] : expandSelector(subject);
      for (const name of names) {
        const members = selectors.get(key(name)) ?? [];
        if (members.length !== 1) throw new Error(members.length
          ? `More than one patch entry is named “${name}”`
          : `“${name}” is missing from the fixture patch. Patch it or edit the name`);
        const entry = members[0];
        const isGroup = "fixture_ids" in entry;
        const selected = isGroup ? entry.fixture_ids.map(id => byId.get(id)) : [entry];
        if (selected.some(f => !f)) throw new Error(`Group “${name}” includes a missing fixture`);
        let matched = 0;
        expanded += selected.length;
        if (expanded > 32768) {
          return { targets: [], commands: count, errors: [`Line ${index + 1} · Cue exceeds 32,768 expanded fixture assignments`] };
        }
        for (const fixture of selected) {
          if (!fixture) continue;
          const params = fixture.fixture_type.parameters;
          const indices = params.flatMap((p, i) => key(p.name) === parameter || p.kind === parameter ? [i] : []);
          if (indices.length > 1) throw new Error(`Use the unique parameter name on “${fixture.label}”`);
          if (!indices.length) {
            if (isGroup) continue;
            throw new Error(`“${fixture.label}” has no “${parameter}” parameter`);
          }
          const param = indices[0];
          const id = `${fixture.id}:${param}`;
          matched++;
          if (value === "pass") {
            updates.delete(id);
          } else {
            const normalized = level ?? params[param].default;
            updates.set(id, { kind: "fixture", fixture_id: fixture.id, param_index: param, value: normalized });
          }
        }
        if (!matched) throw new Error(`“${name}” has no patched “${parameter}” parameter`);
      }
    } catch (error) {
      errors.push(`Line ${index + 1} · ${error instanceof Error ? error.message : String(error)}`);
      if (errors.length >= 30) { errors.push("Fix these lines and preview again"); break; }
    }
  }
  if (!count) errors.push("Enter at least one light command");
  return { targets: errors.length ? [] : Array.from(updates.values()), errors, commands: count };
}

export const defaultParameters: ParamKind[] = [
  "intensity", "red", "green", "blue", "cyan", "magenta", "yellow", "hue", "saturation",
  "hsi_intensity", "hsv_value", "white", "amber", "uv", "pan", "tilt",
];
