import { useState } from "react";
import type { OutputRegion } from "../../lib/types";
import "./outputRegion.css";

const MAX_REGIONS = 8;
const FULL_RECT = ["0", "0", "100", "100"] as const;
const axes = ["Left", "Top", "Width", "Height"] as const;
const planes = ["source", "destination"] as const;
const labels = { source: "Source frame", destination: "Projector raster" };

type RectDraft = [string, string, string, string];
type RegionDraft = Record<typeof planes[number], RectDraft>;

function toDraft(regions: OutputRegion[]): RegionDraft[] {
  return regions.map(({ source, destination }) => ({
    source: source.map(value => String(value / 100)) as RectDraft,
    destination: destination.map(value => String(value / 100)) as RectDraft,
  }));
}

function parseRectangle(fields: RectDraft): OutputRegion["source"] | null {
  const values = fields.map(value => value.trim() ? Math.round(Number(value) * 100) : NaN);
  const [x, y, width, height] = values;
  if (!values.every(Number.isFinite) || x < 0 || y < 0 || width <= 0 || height <= 0 ||
      x + width > 10_000 || y + height > 10_000) {
    return null;
  }
  return [x, y, width, height];
}

export function OutputRegionEditor({ regions, onApply }: {
  regions: OutputRegion[];
  onApply: (regions: OutputRegion[]) => void;
}) {
  const [draft, setDraft] = useState(() => toDraft(regions));
  const [selected, setSelected] = useState(0);
  const [error, setError] = useState("");
  const changed = JSON.stringify(draft) !== JSON.stringify(toDraft(regions));

  function edit(next: RegionDraft[], selection = selected) {
    setDraft(next);
    setSelected(selection);
    setError("");
  }

  function update(plane: typeof planes[number], axis: number, value: string) {
    const next = [...draft];
    const rect: RectDraft = [...next[selected][plane]];
    rect[axis] = value;
    next[selected] = { ...next[selected], [plane]: rect };
    edit(next);
  }

  function move(offset: number) {
    const next = [...draft];
    const index = selected + offset;
    [next[selected], next[index]] = [next[index], next[selected]];
    edit(next, index);
  }

  function apply() {
    const layout: OutputRegion[] = [];
    for (const [index, region] of draft.entries()) {
      const source = parseRectangle(region.source);
      const destination = parseRectangle(region.destination);
      if (!source || !destination) {
        setSelected(index);
        setError(`Region ${index + 1}: Give both rectangles a positive size and keep them within the 100% bounds.`);
        return;
      }
      layout.push({ source, destination });
    }
    edit(toDraft(layout));
    onApply(layout);
  }

  const current = draft[selected];
  return (
    <section className="output-region" aria-label="Stage regions">
      <h4>Stage regions</h4>
      <p>Map parts of this output’s picture to rectangles on the same projector. Uncovered space stays black. Later regions cover earlier ones where they overlap.</p>
      {regions.some(region => region.seam) && <p>Applying these region edits releases the paired seam.</p>}
      <div className="output-region-actions">
        {draft.map((_, index) => (
          <button key={index} type="button" aria-pressed={selected === index} onClick={() => setSelected(index)}>
            Region {index + 1}
          </button>
        ))}
        <button type="button" disabled={draft.length >= MAX_REGIONS}
          onClick={() => edit([...draft, { source: [...FULL_RECT], destination: [...FULL_RECT] }], draft.length)}>
          Add region
        </button>
      </div>
      {current && (
        <>
          <p>Enter percentages from the top-left. Changes take effect when you apply the layout.</p>
          <div className="output-region-layouts">
            {planes.map(plane => (
              <fieldset key={plane}>
                <legend>{labels[plane]}</legend>
                <div className="output-region-map" aria-hidden="true">
                  {draft.map((region, index) => {
                    const rect = parseRectangle(region[plane]);
                    if (!rect) return null;
                    const [x, y, width, height] = rect.map(value => `${value / 100}%`);
                    return <span key={index} className={index === selected ? "selected" : ""}
                      style={{ left: x, top: y, width, height }}>{index + 1}</span>;
                  })}
                </div>
                <div className="output-region-fields">
                  {axes.map((name, axis) => (
                    <label key={name}>{name} %
                      <input type="number" min={0} max={100} step={0.01} value={current[plane][axis]}
                        onChange={event => update(plane, axis, event.currentTarget.value)} />
                    </label>
                  ))}
                </div>
              </fieldset>
            ))}
          </div>
          <div className="output-region-actions">
            <button type="button" disabled={selected === 0} onClick={() => move(-1)}>Move earlier</button>
            <button type="button" disabled={selected === draft.length - 1} onClick={() => move(1)}>Move later</button>
            <button type="button" onClick={() => edit(draft.filter((_, index) => index !== selected), Math.max(0, selected - 1))}>
              Remove region
            </button>
          </div>
        </>
      )}
      {error && <p role="alert">{error}</p>}
      {changed && (
        <div className="output-region-actions">
          <button type="button" onClick={apply}>Apply region layout</button>
          <button type="button" onClick={() => edit(toDraft(regions), 0)}>Revert changes</button>
        </div>
      )}
    </section>
  );
}
