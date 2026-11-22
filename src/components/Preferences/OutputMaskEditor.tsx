import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { getOutputMaskImage, setOutputMaskImage, type OutputMaskImagePreview } from "../../lib/commands";
import { CLEAR_OUTPUT_MASK, type OutputMask } from "../../lib/types";

const sides = ["Left", "Right", "Top", "Bottom"] as const;
const directions = ["right", "left", "bottom", "top"] as const;
const percent = (fraction: number) => Number((fraction * 100).toFixed(2));

/** Stacking black gradients multiplies the edge ramps, matching the output shader. */
export function maskPreview(mask: OutputMask): string {
  return directions.map((direction, index) => {
    const trim = percent(mask.trim[index]);
    const end = percent(mask.trim[index] + mask.feather[index]);
    return `linear-gradient(to ${direction}, black ${trim}%, transparent ${end}%)`;
  }).join(", ");
}

export function OutputMaskEditor({ mask, onChange, outputId = null }: {
  mask: OutputMask; onChange: (mask: OutputMask) => void; outputId?: string | null;
}) {
  const [image, setImage] = useState<OutputMaskImagePreview | null>(null);
  const [document, setDocument] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  useEffect(() => {
    let mounted = true;
    setBusy(true);
    setDocument(null);
    getOutputMaskImage(outputId).then(value => { if (mounted) { setDocument(value.document); setImage(value.image); } })
      .catch(e => { if (mounted) setError(String(e)); })
      .finally(() => { if (mounted) setBusy(false); });
    return () => { mounted = false; };
  }, [outputId]);

  async function changeImage(clear: boolean) {
    if (!document) return;
    setError("");
    setBusy(true);
    try {
      const file = clear ? null : await open({ multiple: false, filters: [{ name: "PNG image mask", extensions: ["png"] }] });
      if (!clear && typeof file !== "string") return;
      setImage(await setOutputMaskImage(outputId, document, file));
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  function setWidth(kind: keyof OutputMask, index: number, value: number) {
    if (!Number.isFinite(value)) return;
    const widths: OutputMask[typeof kind] = [...mask[kind]];
    widths[index] = Math.max(0, Math.min(50, value)) / 100;
    onChange({ ...mask, [kind]: widths });
  }

  return <section className="output-mask" aria-label="Output mask and feather">
    <h4>Output mask and feather</h4>
    <p>Trim blocks spill beyond an edge. Feather softens the picture inward from that trim. Widths use a percentage of the unwarped picture.</p>
    <div className="output-mask-controls">
      <table><thead><tr><th>Edge</th><th>Trim %</th><th>Feather %</th></tr></thead>
        <tbody>{sides.map((side, index) => <tr key={side}><th scope="row">{side}</th>
          {(["trim", "feather"] as const).map(kind => <td key={kind}>
            <input aria-label={`${side} ${kind} percent`} type="number" min={0} max={50} step={0.1}
              key={mask[kind][index]} defaultValue={percent(mask[kind][index])}
              onBlur={e => {
                const input = e.currentTarget;
                const value = input.valueAsNumber;
                if (!input.validity.valid || !Number.isFinite(value)) {
                  input.value = String(percent(mask[kind][index]));
                } else if (value !== percent(mask[kind][index])) {
                  setWidth(kind, index, value);
                }
              }}
              onKeyDown={e => { if (e.key === "Enter") e.currentTarget.blur(); }} />
          </td>)}</tr>)}</tbody></table>
      <figure><div role="img" aria-label="Unwarped white field with this mask"
        style={{backgroundImage: image ? `${maskPreview(mask)}, url("data:image/png;base64,${image.png}")` : maskPreview(mask), backgroundSize: "100% 100%"}} />
        <figcaption>Unwarped mask preview</figcaption></figure>
    </div>
    <button type="button" disabled={![...mask.trim, ...mask.feather].some(Boolean)}
      onClick={() => onChange(CLEAR_OUTPUT_MASK)}>Clear edges</button>
    <h5>Image mask</h5>
    <p>Black hides the picture. White passes it. The mask is embedded in the show.</p>
    {image && <p>{image.name} · {image.width} × {image.height}</p>}
    <button type="button" disabled={busy || !document} onClick={() => void changeImage(false)}>{image ? "Replace PNG…" : "Load PNG…"}</button>{" "}
    {image && <button type="button" disabled={busy || !document} onClick={() => void changeImage(true)}>Remove image mask</button>}
    {error && <p role="alert">{error}</p>}
  </section>;
}
