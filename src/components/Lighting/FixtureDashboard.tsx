// Live lighting Dashboard (QLab-style): one row per patched fixture with an
// intensity slider + a colour picker (for RGB fixtures) + sliders for any other
// parameter. Editing drives the DMX engine live; the Light Cue inspector then
// captures this state with "Capture live state".
//
// The Dashboard owns its working values (seeded from the live engine snapshot),
// so dragging a slider never fights the ~20 fps monitor feed. "↻ Live" reseeds
// from the engine (e.g. after a cue GO), "Clear" zeroes every fixture.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import {
  dmxClearFixtures, dmxSetFixtureCmy, dmxSetFixtureHsi, dmxSetFixtureHsv, dmxSetGroupColor,
  dmxSetFixtureParam, loadFixtureDashboard,
} from "../../lib/commands";
import type { DmxUniverseSnapshot, FixtureGroup, FixtureParam, PatchedFixture } from "../../lib/types";
import { channelSlots, fixtureChannelFits, fixtureParameterAddress } from "../../lib/fixtureChannels";
import { fixtureColorBindings, groupColorBindings, groupColorLevels, paramIndexOfKind, rgbChannelChanges, rgbToHex } from "../../lib/fixtureColor";
import { FixtureColorControl } from "./FixtureColorControl";

const applyFixtureColor = { cmy: dmxSetFixtureCmy, hsi: dmxSetFixtureHsi, hsv: dmxSetFixtureHsv };

/** Read a parameter's current normalised value (0–1) from a universe snapshot. */
function valueFromSnapshot(fixture: PatchedFixture, param: FixtureParam, snap: DmxUniverseSnapshot[]): number {
  const u = snap.find((s) => s.universe === fixture.universe);
  if (!u) return 0;
  const address = fixtureParameterAddress(fixture, param);
  if (fixture.base_address < 1 || !fixtureChannelFits(address)) return 0;
  const [coarse, fine] = channelSlots(address);
  const hi = u.channels[coarse] ?? 0;
  return fine === undefined ? hi / 255 : ((hi << 8) | (u.channels[fine] ?? 0)) / 65535;
}

export function FixtureDashboard({ workspaceId }: { workspaceId: string }) {
  const [fixtures, setFixtures] = useState<PatchedFixture[]>([]);
  const [groups, setGroups] = useState<FixtureGroup[]>([]);
  // fixtureId -> value per parameter index.
  const [values, setValues] = useState<Record<string, number[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const loadSequence = useRef(0);
  const groupColors = useMemo(() => groups.flatMap(group => {
    const mixes = groupColorBindings(group, fixtures);
    return mixes.length ? [{ group, mixes }] : [];
  }), [groups, fixtures]);

  const refresh = useCallback(async () => {
    const sequence = ++loadSequence.current;
    setLoading(true);
    setError("");
    try {
      const { fixtures: next, groups: nextGroups, snapshot } = await loadFixtureDashboard(workspaceId);
      if (sequence !== loadSequence.current) return;
      setFixtures(next);
      setGroups(nextGroups);
      setValues(Object.fromEntries(next.map(f => [
        f.id, f.fixture_type.parameters.map(p => valueFromSnapshot(f, p, snapshot)),
      ])));
    } catch (e) {
      if (sequence === loadSequence.current) setError(String(e));
    } finally {
      if (sequence === loadSequence.current) setLoading(false);
    }
  }, [workspaceId]);

  useEffect(() => {
    void refresh();
    return () => { ++loadSequence.current; };
  }, [refresh]);

  const setParam = (fixtureId: string, paramIndex: number, value: number) => {
    setValues((v) => {
      const row = v[fixtureId] ? [...v[fixtureId]] : [];
      row[paramIndex] = value;
      return { ...v, [fixtureId]: row };
    });
    void dmxSetFixtureParam(workspaceId, fixtureId, paramIndex, value).catch(e => setError(String(e)));
  };

  const clearAll = async () => {
    try {
      await dmxClearFixtures(workspaceId);
      setValues(Object.fromEntries(fixtures.map(f => [f.id, f.fixture_type.parameters.map(() => 0)])));
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      <div style={{ display: "flex", gap: 6, marginBottom: 2 }}>
        <button style={smallBtn} disabled={loading} onClick={() => void refresh()} title="Refresh the fixture patch and live engine values">↻ Live</button>
        <button style={smallBtn} disabled={loading || !fixtures.length} onClick={clearAll} title="Set every fixture to zero">Clear</button>
      </div>

      {error && <div role="alert">{error}</div>}
      {loading && <div role="status">Loading live fixtures…</div>}
      {!loading && !error && !fixtures.length && (
        <div style={{ color: "var(--wc-text-faint)", fontSize: 12 }}>Patch a fixture to sculpt it here.</div>
      )}
      {groupColors.length > 0 && (
        <details style={rowStyle}>
          <summary>Fixture group colors</summary>
          {!loading && groupColors.map(({ group, mixes }) => (
            <div key={group.id} style={{ marginTop: 6 }}>
              <strong>{group.label}</strong> · {group.fixture_ids.length} fixtures
              {mixes.map(({ model, mapping }) => (
                <FixtureColorControl key={`${model}-${JSON.stringify(mapping)}`} model={model} live
                  levels={groupColorLevels(mapping, values)}
                  onApply={async levels => {
                    const revision = loadSequence.current;
                    const expected = mapping.map(({ id, channels }) => ({ id, channels }));
                    await dmxSetGroupColor(workspaceId, group.id, model, expected, levels);
                    if (revision !== loadSequence.current) return;
                    setValues(previous => {
                      const next = { ...previous };
                      for (const { id, indices: [first, second, third] } of mapping) {
                        const row = [...(next[id] ?? [])];
                        row[first] = levels[0];
                        row[second] = levels[1];
                        row[third] = levels[2];
                        next[id] = row;
                      }
                      return next;
                    });
                  }} />
              ))}
            </div>
          ))}
        </details>
      )}
      {!loading && fixtures.map((f) => {
        const vals = values[f.id] ?? f.fixture_type.parameters.map(() => 0);
        const params = f.fixture_type.parameters;
        const colorMixes = fixtureColorBindings(f);
        const intIdx = paramIndexOfKind(params, "intensity");
        const rIdx = paramIndexOfKind(params, "red");
        const gIdx = paramIndexOfKind(params, "green");
        const bIdx = paramIndexOfKind(params, "blue");
        const hasRgb = rIdx >= 0 && gIdx >= 0 && bIdx >= 0;
        const colorIdx = new Set([...(hasRgb ? [rIdx, gIdx, bIdx] : []), ...colorMixes.flatMap(mix => mix.indices)]);

        return (
          <div key={f.id} style={rowStyle}>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span style={{ color: "var(--wc-text)", flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {f.label}
              </span>
              {hasRgb && (
                <input
                  type="color"
                  value={rgbToHex(vals[rIdx] ?? 0, vals[gIdx] ?? 0, vals[bIdx] ?? 0)}
                  onChange={(e) => {
                    for (const { paramIndex, value } of rgbChannelChanges(params, e.target.value)) {
                      setParam(f.id, paramIndex, value);
                    }
                  }}
                  style={{ width: 28, height: 22, padding: 0, border: "1px solid var(--wc-border-strong)", borderRadius: 4, background: "none", cursor: "pointer" }}
                  title="Colour"
                />
              )}
            </div>

            {colorMixes.map(({ model, indices: [first, second, third], channels }) =>
              <FixtureColorControl key={`${model}-${JSON.stringify(channels)}`} model={model} live
                levels={[vals[first] ?? 0, vals[second] ?? 0, vals[third] ?? 0]}
                onApply={async levels => {
                  await applyFixtureColor[model](workspaceId, f.id, channels, levels);
                  setValues(previous => {
                    const row = [...(previous[f.id] ?? [])];
                    row[first] = levels[0];
                    row[second] = levels[1];
                    row[third] = levels[2];
                    return { ...previous, [f.id]: row };
                  });
                }} />)}

            {intIdx >= 0 && (
              <Slider
                label="Dimmer"
                pct={Math.round((vals[intIdx] ?? 0) * 100)}
                onPct={(p) => setParam(f.id, intIdx, p / 100)}
              />
            )}

            {/* Independent controls outside the dimmer and color pickers. */}
            {f.fixture_type.parameters.map((p, i) =>
              i === intIdx || colorIdx.has(i) ? null : (
                <Slider
                  key={i}
                  label={p.name}
                  pct={Math.round((vals[i] ?? 0) * 100)}
                  onPct={(pp) => setParam(f.id, i, pp / 100)}
                />
              ),
            )}
          </div>
        );
      })}
    </div>
  );
}

function Slider({ label, pct, onPct }: { label: string; pct: number; onPct: (p: number) => void }) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
      <span style={{ color: "var(--wc-text-muted)", fontSize: 10, width: 52, flexShrink: 0 }}>{label}</span>
      <input type="range" min={0} max={100} value={pct} style={{ flex: 1 }} onChange={(e) => onPct(Number(e.target.value))} />
      <span style={{ color: "var(--wc-text)", width: 34, textAlign: "right", fontSize: 11 }}>{pct}%</span>
    </div>
  );
}

const rowStyle: CSSProperties = {
  display: "flex", flexDirection: "column", gap: 4,
  border: "1px solid var(--wc-border)", borderRadius: 4, padding: "5px 7px",
};
const smallBtn: CSSProperties = {
  background: "none", border: "1px solid var(--wc-border-strong)", borderRadius: 4, color: "var(--wc-text-secondary)", fontSize: 11, padding: "2px 8px", cursor: "pointer",
};
