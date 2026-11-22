import { useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { applyOutputSplit, exportProjectionChart, type OutputSplitDirection } from "../../lib/commands";
import type { VideoOutputInfo } from "../../lib/types";
import { Select } from "../common/Select";
import "./outputRegion.css";

interface SplitDraft {
  direction: OutputSplitDirection;
  pair: [string, string];
  percent: string;
  feather: boolean;
  gamma: string;
}

function presetForPair(outputs: VideoOutputInfo[], pair: [string, string]): SplitDraft | null {
  const [first, second] = pair.map(id => outputs.find(output => output.id === id)?.transform.regions?.[0]?.seam);
  if (first && second && !first.rising && second.rising && first.partner === pair[1] && second.partner === pair[0]
      && first.direction === second.direction && first.overlap === second.overlap && first.gamma === second.gamma) {
    return { pair, direction: first.direction, percent: String(first.overlap / 100), feather: true, gamma: (first.gamma / 100).toFixed(2) };
  }
  return null;
}

function initialDraft(outputs: VideoOutputInfo[]): SplitDraft {
  for (const output of outputs) {
    const seam = output.transform.regions?.[0]?.seam;
    if (seam && !seam.rising) {
      const draft = presetForPair(outputs, [output.id, seam.partner]);
      if (draft) return draft;
    }
  }
  return { direction: "horizontal", pair: [outputs[0].id, outputs[1].id], percent: "0", feather: false, gamma: "2.20" };
}

export function OutputSplitEditor({ document, outputs, onApplied, onRefresh }: {
  document: string;
  outputs: VideoOutputInfo[];
  onApplied: (outputs: VideoOutputInfo[]) => void;
  onRefresh: () => void;
}) {
  const [draft, setDraft] = useState(() => initialDraft(outputs));
  const { direction, pair, percent, feather, gamma } = draft;
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const first = outputs.find(output => output.id === pair[0]);
  const second = outputs.find(output => output.id === pair[1]);
  const sameScreen = first?.screen != null && first.screen === second?.screen;
  const overlap = Math.round(Number(percent) * 100);
  const validOverlap = percent.trim() !== "" && Number.isFinite(overlap) && overlap >= 0 && overlap <= 2_000;
  const useFeather = feather && validOverlap && overlap > 0;
  const seamGamma = Math.round(Number(gamma) * 100);
  const validGamma = !useFeather || (seamGamma >= 100 && seamGamma <= 300);
  const overlapSize = validOverlap ? overlap / 100 : 0;
  const firstEnd = (100 + overlapSize) / 2;
  const secondStart = (100 - overlapSize) / 2;
  const labels = direction === "horizontal" ? ["Left output", "Right output"] : ["Top output", "Bottom output"];
  const replacing = [first, second].some(output => Boolean(output?.transform.regions?.length));

  function clearFeedback() {
    setError("");
    setMessage("");
  }

  function edit(change: Partial<SplitDraft>) {
    clearFeedback();
    setDraft(current => ({ ...current, ...change }));
  }

  async function run(action: () => Promise<void>) {
    clearFeedback();
    setBusy(true);
    try { await action(); } catch (failure) { setError(String(failure)); } finally { setBusy(false); }
  }

  function apply() {
    if (!first || !second || sameScreen || !validOverlap || !validGamma) return;
    void run(async () => {
      onApplied(await applyOutputSplit(document, [first, second], direction, overlap, useFeather ? seamGamma : undefined));
      setMessage("Paired crops applied. Save the workspace to keep them.");
    });
  }

  function exportChart() {
    if (!validOverlap) return;
    void run(async () => {
      const path = await save({ defaultPath: "Inkue alignment chart.png", filters: [{ name: "PNG", extensions: ["png"] }] });
      if (!path) return;
      await exportProjectionChart(path, direction, overlap);
      setMessage("Chart saved. Target this PNG from an Image cue on each output and choose Stretch in Geometry.");
    });
  }

  return (
    <section className="output-region" aria-label="Paired projection">
      <h4>Paired projection</h4>
      <p>Map two parts of one picture to two projector rasters. Use the same still image on both outputs to line up the seam.</p>
      <fieldset disabled={busy} className="output-split-fields">
        <label>Arrangement
          <Select value={direction} onChange={event => edit({ direction: event.target.value === "vertical" ? "vertical" : "horizontal" })}>
            <option value="horizontal">Side by side</option>
            <option value="vertical">Stacked</option>
          </Select>
        </label>
        {labels.map((label, index) => (
          <label key={index}>{label}
            <Select value={pair[index]} onChange={event => {
              const next: [string, string] = [...pair];
              next[index] = event.target.value;
              edit(presetForPair(outputs, next) ?? { pair: next });
            }}>
              {!outputs.some(output => output.id === pair[index]) && <option value={pair[index]}>Output unavailable</option>}
              {outputs.map(output => <option key={output.id} value={output.id} disabled={output.id === pair[1 - index]}>
                {output.name} · {output.screen == null ? "Floating" : `Screen ${output.screen + 1}`}
              </option>)}
            </Select>
          </label>
        ))}
        <label>Overlap of full picture %
          <input type="number" min={0} max={20} step={0.01} value={percent} onChange={event => edit({ percent: event.target.value })} aria-invalid={!validOverlap} />
        </label>
        <label className="output-split-check">
          <input type="checkbox" checked={useFeather} disabled={!validOverlap || overlap === 0}
            onChange={event => edit({ feather: event.target.checked })} />
          Feather shared seam
        </label>
        {useFeather && <label>Projector gamma
          <input type="number" min={1} max={3} step={0.01} value={gamma} onChange={event => edit({ gamma: event.target.value })} aria-invalid={!validGamma} />
        </label>}
      </fieldset>
      <div className={`output-split-preview ${direction}`} aria-hidden="true">
        <span className="output-split-first" style={direction === "horizontal" ? { width: `${firstEnd}%` } : { height: `${firstEnd}%` }} />
        <span className="output-split-second" style={direction === "horizontal" ? { left: `${secondStart}%` } : { top: `${secondStart}%` }} />
      </div>
      {sameScreen && <p role="alert">Assign different screens or use floating windows.</p>}
      {!validOverlap && <p role="alert">Enter an overlap from 0% to 20%.</p>}
      {!validGamma && <p role="alert">Enter a projector gamma from 1.00 to 3.00.</p>}
      {useFeather && <p>Match both projectors’ brightness and gamma. Existing masks also affect seam brightness.</p>}
      {replacing && <p>Applying replaces regions on both selected outputs and preserves their alignment and masks.</p>}
      <div className="output-region-actions">
        <button type="button" disabled={busy} onClick={() => edit({ pair: [pair[1], pair[0]] })}>Swap outputs</button>
        <button type="button" disabled={busy || !validOverlap || !validGamma || !first || !second || sameScreen} onClick={apply}>Apply paired crops</button>
        <button type="button" disabled={busy || !validOverlap} onClick={exportChart}>Save alignment PNG…</button>
        {error && <button type="button" disabled={busy} onClick={() => { clearFeedback(); onRefresh(); }}>Reload outputs</button>}
      </div>
      {error && <p role="alert">{error}</p>}
      {message && <p role="status">{message}</p>}
    </section>
  );
}
