import type { TranslationWaypoint } from "../../lib/types";
import { Grid2, MiniField, NumberInput } from "./Field";

const MAX_WAYPOINTS = 16;

export function PositionWaypoints({ points, destination, smooth, looping, canLoop, onSmoothChange, onLoopChange, onChange }: {
  points: TranslationWaypoint[];
  destination: TranslationWaypoint;
  smooth: boolean;
  looping: boolean;
  canLoop: boolean;
  onSmoothChange: (smooth: boolean) => void;
  onLoopChange: (looping: boolean) => void;
  onChange: (points: TranslationWaypoint[]) => void;
}) {
  const move = (index: number, offset: -1 | 1) => {
    const next = [...points];
    const adjacent = index + offset;
    [next[index], next[adjacent]] = [next[adjacent], next[index]];
    onChange(next);
  };

  const change = (index: number, axis: 0 | 1, value: number) => {
    if (points[index][axis] === value) return;
    onChange(points.map((point, row) => row === index
      ? (axis === 0 ? [value, point[1]] : [point[0], value]) : point));
  };

  const add = () => {
    const [x, y] = points[points.length - 1] ?? [0, 0];
    onChange([...points, [
      Math.round((x + destination[0]) * 50) / 100,
      Math.round((y + destination[1]) * 50) / 100,
    ]]);
  };

  return (
    <fieldset style={{ border: "1px solid var(--wc-border-strong)", borderRadius: 4, padding: 8, margin: "8px 0" }}>
      <legend>Waypoints</legend>
      {points.map(([x, y], index) => {
        const percent = Math.round(1000 * (index + 1) / (points.length + 1)) / 10;
        return (
          <div key={index} style={{ marginBottom: 8 }}>
            <div style={{ fontSize: 11, marginBottom: 4 }}>At {percent}% of the Fade</div>
            <Grid2>
              <MiniField label={`Waypoint ${index + 1} X`}>
                <NumberInput ariaLabel={`Waypoint ${index + 1} X`} value={x} min={-1} max={1} step={0.01}
                  onCommit={value => change(index, 0, value)} />
              </MiniField>
              <MiniField label={`Waypoint ${index + 1} Y`}>
                <NumberInput ariaLabel={`Waypoint ${index + 1} Y`} value={y} min={-1} max={1} step={0.01}
                  onCommit={value => change(index, 1, value)} />
              </MiniField>
            </Grid2>
            <div style={{ display: "flex", gap: 6 }}>
              <button type="button" aria-label={`Move waypoint ${index + 1} earlier`} disabled={index === 0}
                onClick={() => move(index, -1)}>Earlier</button>
              <button type="button" aria-label={`Move waypoint ${index + 1} later`} disabled={index === points.length - 1}
                onClick={() => move(index, 1)}>Later</button>
              <button type="button" aria-label={`Remove waypoint ${index + 1}`}
                onClick={() => onChange(points.filter((_, row) => row !== index))}>Remove</button>
            </div>
          </div>
        );
      })}
      <button type="button" onClick={add} disabled={points.length >= MAX_WAYPOINTS}>Add waypoint</button>
      {points.length > 0 && (
        <>
          <label style={{ display: "flex", alignItems: "center", gap: 6, marginTop: 6 }}>
            <input type="checkbox" checked={smooth} onChange={event => onSmoothChange(event.target.checked)} />
            Smooth path
          </label>
          <label style={{ display: "flex", alignItems: "center", gap: 6, marginTop: 6 }}>
            <input type="checkbox" checked={looping} disabled={!looping && !canLoop}
              onChange={event => onLoopChange(event.target.checked)} />
            Loop path
          </label>
          <div style={{ fontSize: 11, marginTop: 6 }}>
            {!canLoop && !looping ? "To loop, use only position, set Do Not Continue, and turn off Stop at end."
              : looping ? "Each pass jumps from the destination to the original live position."
              : "Each leg gets an equal share of the Fade time, starting from the live position."}
          </div>
        </>
      )}
    </fieldset>
  );
}
