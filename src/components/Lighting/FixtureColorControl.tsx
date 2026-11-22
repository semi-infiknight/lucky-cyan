import { useEffect, useState } from "react";
import { cmyToHex, hexToCmy, hsiToHex, hsvToHex } from "../../lib/fixtureColor";
import type { DisplayColorLevels } from "../../lib/fixtureColor";
import type { ColorLevels, FixtureColorModel } from "../../lib/types";

const controls = {
  cmy: { title: "Subtractive color (CMY)", labels: ["Cyan %", "Magenta %", "Yellow %"], scales: [100, 100, 100], preview: cmyToHex },
  hsi: { title: "Native color (HSI)", labels: ["Hue °", "Saturation %", "HSI intensity %"], scales: [360, 100, 100], preview: hsiToHex },
  hsv: { title: "Native color (HSV)", labels: ["Hue °", "Saturation %", "HSV value %"], scales: [360, 100, 100], preview: hsvToHex },
};

function displayLevels(levels: DisplayColorLevels, scales: number[]) {
  return levels.map((value, index) => value === null ? "" : String(Number((value * scales[index]).toFixed(6))));
}

export function FixtureColorControl({ model, levels, onApply, live = false }: {
  model: FixtureColorModel;
  levels: DisplayColorLevels;
  onApply: (levels: ColorLevels) => void | Promise<void>;
  live?: boolean;
}) {
  const { labels, scales, title, preview } = controls[model];
  const modelName = model.toUpperCase();
  const previewLabel = `Approximate ${modelName} preview`;
  const [first, second, third] = levels;
  const [draft, setDraft] = useState(() => displayLevels(levels, scales));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => setDraft(displayLevels([first, second, third], scales)), [first, second, third, scales]);

  const rangeValid = draft.every((value, index) => value.trim() && Number(value) >= 0 && Number(value) <= scales[index]);
  const selected: ColorLevels | null = rangeValid
    ? [Number(draft[0]) / scales[0], Number(draft[1]) / scales[1], Number(draft[2]) / scales[2]]
    : null;
  const previewLevels: ColorLevels | null = selected ?? (
    first === null || second === null || third === null ? null : [first, second, third]
  );

  async function commit() {
    if (busy || !selected) return;
    setBusy(true);
    setError("");
    try {
      await onApply(selected);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return <fieldset disabled={busy} style={{ display: "flex", flexWrap: "wrap", alignItems: "center", gap: 8 }}>
    <legend>{title}</legend>
    {previewLevels && (model === "cmy"
      ? <input type="color" aria-label={previewLabel} value={preview(previewLevels)}
          onChange={event => { setError(""); setDraft(displayLevels(hexToCmy(event.currentTarget.value), scales)); }} />
      : <span role="img" aria-label={previewLabel} title={previewLabel}
          style={{ backgroundColor: preview(previewLevels), width: 26, height: 20, border: "1px solid var(--wc-text)", borderRadius: 3 }} />)}
    {draft.map((value, index) => <label key={labels[index]}>{labels[index]}
      <input type="number" min={0} max={scales[index]} step="any" value={value} style={{ width: 74, marginLeft: 4 }}
        placeholder={levels[index] === null ? "Mixed" : undefined}
        onChange={event => {
          const updated = event.currentTarget.value;
          setError("");
          setDraft(existing => existing.map((item, position) => position === index ? updated : item));
        }} />
    </label>)}
    <button type="button" disabled={!selected || busy} onClick={() => void commit()}>
      {live ? "Apply to live output" : `Apply ${modelName} to cue`}
    </button>
    {error && <span role="alert">{error}</span>}
  </fieldset>;
}
