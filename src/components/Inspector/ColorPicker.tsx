import type { CueColor } from "../../lib/types";

import { COLOR_OPTIONS } from "../../lib/cueColors";

export function ColorPicker({
  value,
  onChange,
}: {
  value: CueColor;
  onChange: (c: CueColor) => void;
}) {
  return (
    <div style={{ display: "flex", gap: 5, flexWrap: "wrap" }}>
      {COLOR_OPTIONS.map(({ value: v, hex, label }) => (
        <button
          key={v}
          title={label}
          type="button"
          aria-label={label}
          aria-pressed={v === value}
          onClick={() => onChange(v)}
          style={{
            width: 20,
            height: 20,
            borderRadius: 4,
            border: v === value ? "2px solid var(--wc-text-bright)" : "2px solid var(--wc-text-faint)",
            background: hex === "transparent" ? "var(--wc-bg-surface)" : hex,
            cursor: "pointer",
            padding: 0,
            flexShrink: 0,
            outline: v === value ? "1px solid var(--wc-text-secondary)" : "none",
            outlineOffset: 1,
          }}
        />
      ))}
    </div>
  );
}
