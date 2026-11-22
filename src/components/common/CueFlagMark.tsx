import type { CueSummary } from "../../lib/types";

export function cueFlagLabel(cue: CueSummary): string {
  if (cue.flagged) return "Flagged for review";
  if (cue.has_flagged_child) return "Contains flagged cues";
  return "";
}

export function CueFlagMark({ cue }: { cue: CueSummary }) {
  const label = cueFlagLabel(cue);
  return label ? <span title={label} aria-hidden="true"
    style={{ color: "var(--wc-accent)", fontSize: 13, flexShrink: 0 }}>
    {cue.flagged ? "⚑" : "⚐"}
  </span> : null;
}
