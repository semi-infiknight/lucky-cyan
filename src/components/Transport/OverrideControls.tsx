import { OVERRIDE_LABELS } from "../../hooks/useGlobalOverrides";
import type { GlobalOverrides } from "../../lib/commands";

export function OverrideControls({ overrides, open, setOpen, toggle, pending, error }: {
  overrides: GlobalOverrides | null;
  open: boolean;
  setOpen: (open: boolean) => void;
  toggle: (key: string) => void;
  pending: string;
  error: string;
}) {
  return <div className="desk-override-controls">
    <button type="button" aria-expanded={open} onClick={() => setOpen(!open)}
      title="Disable or enable whole input/output paths (DMX, MIDI, MSC, network, timecode)">
      Overrides
    </button>
    {open && <div className="desk-override-panel" role="group" aria-label="Global overrides">
      {OVERRIDE_LABELS.map(([key, label]) => {
        const enabled = overrides?.[key] ?? true;
        return <label key={key} className="desk-override-row" data-off={!enabled}>
          <input type="checkbox" checked={enabled} disabled={pending === key}
            onChange={() => toggle(key)} />
          <span>{label}</span>
          {!enabled && <strong role="status">off</strong>}
        </label>;
      })}
      {error && <span role="alert">{error}</span>}
    </div>}
  </div>;
}
