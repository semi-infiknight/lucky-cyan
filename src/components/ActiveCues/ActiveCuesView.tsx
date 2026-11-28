// Panel showing all currently running or paused cues, always visible while active.

import { cueAccent } from "../../lib/cueColors";
import { useEffect, useState } from "react";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useTimingStore } from "../../stores/timingStore";
import type { CueSummary } from "../../lib/types";
import { cueLiveLevels, stopCue } from "../../lib/commands";
import type { CueLiveLevels } from "../../lib/types";
import { cueTimeline, formatClock, timelinePosition } from "../../lib/timeline";
import { ActiveCueProgress } from "./ActiveCueProgress";

const CUE_TYPE_ICONS: Record<string, string> = {
  audio: "🔊", memo: "📝", wait: "⏱", group: "📁",
  fade: "📉", stop: "⬛", video: "🎬", image: "🖼",
  osc: "📡", midi: "🎹", light: "💡", mic: "🎤", timecode: "🕐", text: "🔤",
};

function flattenActive(cues: CueSummary[]): CueSummary[] {
  const result: CueSummary[] = [];
  for (const cue of cues) {
    if (cue.state === "running" || cue.state === "paused") result.push(cue);
    if (cue.children?.length) result.push(...flattenActive(cue.children));
  }
  return result;
}

/** Poll the live per-voice levels of the running cues (RMS, dB) for the meter. */
function useCueLevels(ids: string[]): Record<string, CueLiveLevels> {
  const [levels, setLevels] = useState<Record<string, CueLiveLevels>>({});
  const key = ids.join(",");
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      const entries = await Promise.all(
        key.split(",").filter(Boolean).map(async (id) => [id, await cueLiveLevels(id).catch(() => null)] as const),
      );
      if (!cancelled) {
        const next: Record<string, CueLiveLevels> = {};
        for (const [id, v] of entries) if (v !== null) next[id] = v;
        setLevels(next);
      }
    };
    void tick();
    const timer = setInterval(() => void tick(), 300);
    return () => { cancelled = true; clearInterval(timer); };
  }, [key]);
  return levels;
}

function CueLevelMeter({ levels }: { levels?: CueLiveLevels }) {
  if (!levels) return null;
  const bar = (db: number) => Math.max(0, Math.min(1, (db + 60) / 60));
  const color = (db: number) => (db > -3 ? "#ef4444" : db > -18 ? "#facc15" : "#22c55e");
  return (
    <span
      title={`L ${levels.rms_db_l.toFixed(1)} dB · R ${levels.rms_db_r.toFixed(1)} dB (RMS)`}
      style={{ display: "inline-flex", gap: 2, flexShrink: 0, alignItems: "flex-end", height: 12 }}
    >
      {[levels.rms_db_l, levels.rms_db_r].map((db, i) => (
        <span key={i} style={{ width: 4, height: 12, background: "var(--wc-border)", position: "relative", overflow: "hidden", borderRadius: 1 }}>
          <span style={{ position: "absolute", bottom: 0, left: 0, right: 0, height: `${bar(db) * 100}%`, background: color(db) }} />
        </span>
      ))}
    </span>
  );
}

function ActiveCueRow({ cue, levels }: { cue: CueSummary; levels?: CueLiveLevels }) {
  const timing = useTimingStore((s) => s.timings[cue.id]);
  const isPaused = cue.state === "paused";
  const colorAccent = cueAccent(cue);

  const timeline = cueTimeline(cue);
  const elapsed = timing?.action_elapsed_ms ?? 0;
  const remaining = timing?.remaining_ms ?? (cue.duration_ms != null ? Math.max(0, cue.duration_ms - elapsed) : null);

  return (
    <div
      style={{
        position: "relative",
        display: "flex",
        alignItems: "center",
        height: 26,
        gap: 6,
        padding: "0 8px 0 12px",
        borderBottom: "1px solid var(--wc-border)",
        background: isPaused ? "var(--wc-bg-paused)" : "var(--wc-bg-running)",
        fontSize: 12,
        flexShrink: 0,
      }}
    >
      {colorAccent !== "transparent" && (
        <div style={{
          position: "absolute", left: 0, top: 0, bottom: 0, width: 4,
          background: colorAccent, pointerEvents: "none",
        }} />
      )}
      {timeline !== null && (
        <ActiveCueProgress
          cueId={cue.id}
          timeline={timeline}
          positionMs={timelinePosition(timeline, elapsed)}
          seekable={cue.seekable}
          color={isPaused ? "#fb923c" : "#22c55e"}
        />
      )}

      <span style={{ flexShrink: 0, fontSize: 11 }}>
        {CUE_TYPE_ICONS[cue.cue_type] ?? "?"}
      </span>

      {cue.number && (
        <span style={{ flexShrink: 0, color: "var(--wc-text-secondary)", fontFamily: "monospace", fontSize: 11, minWidth: 28, textAlign: "right" }}>
          {cue.number}
        </span>
      )}

      <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", color: "var(--wc-text)" }}>
        {cue.name || "(untitled)"}
      </span>

      <CueLevelMeter levels={levels} />

      {cue.output_patch_name && (
        <span style={{
          flexShrink: 0, fontSize: 10, fontWeight: 700,
          color: isPaused ? "#fb923c" : "#4ade80",
          letterSpacing: "0.04em",
          maxWidth: 120, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap",
        }}>
          OUTPUT: {cue.output_patch_name}
        </span>
      )}

      <span style={{
        flexShrink: 0, fontSize: 10, fontWeight: 700,
        color: isPaused ? "#fb923c" : "#4ade80",
        letterSpacing: "0.04em",
      }}>
        {isPaused ? "PAUSED" : "RUNNING"}
      </span>

      <span style={{ flexShrink: 0, fontFamily: "monospace", fontSize: 11, color: "var(--wc-text-secondary)", minWidth: 36, textAlign: "right" }}>
        {remaining !== null
          ? formatClock(remaining)
          : elapsed > 0
          ? `+${formatClock(elapsed)}`
          : ""}
      </span>

      <button
        onClick={() => stopCue(cue.id).catch(console.error)}
        title="Stop"
        style={{
          flexShrink: 0, position: "relative", zIndex: 3,
          display: "flex", alignItems: "center", justifyContent: "center",
          width: 18, height: 18,
          background: "rgba(239,68,68,0.15)",
          border: "1px solid rgba(239,68,68,0.4)",
          borderRadius: 3,
          cursor: "pointer", padding: 0,
        }}
      >
        <div style={{ width: 6, height: 6, background: "#ef4444", borderRadius: 1 }} />
      </button>
    </div>
  );
}

export function ActiveCuesView() {
  const cues = useWorkspaceStore((s) => s.cues);
  const activeCues = flattenActive(cues);
  const levels = useCueLevels(activeCues.map((cue) => cue.id));

  if (activeCues.length === 0) return null;

  return (
    <div style={{
      flexShrink: 0,
      maxHeight: 180,
      overflowY: "auto",
      background: "var(--wc-bg-surface)",
      borderBottom: "1px solid var(--wc-border-strong)",
    }}>
      <div style={{
        display: "flex", alignItems: "center", gap: 6,
        padding: "3px 10px",
        borderBottom: "1px solid var(--wc-border)",
        position: "sticky", top: 0, zIndex: 1,
        background: "var(--wc-bg-surface)",
        flexShrink: 0,
      }}>
        <span style={{ fontSize: 10, fontWeight: 700, letterSpacing: "0.08em", color: "var(--wc-text-muted)", textTransform: "uppercase" }}>
          Active
        </span>
        <span style={{
          fontSize: 9, fontWeight: 700,
          background: "#22c55e", color: "#052e16",
          borderRadius: 8, padding: "0 5px", lineHeight: "14px",
        }}>
          {activeCues.length}
        </span>
      </div>
      {activeCues.map((cue) => (
        <ActiveCueRow key={cue.id} cue={cue} levels={levels[cue.id]} />
      ))}
    </div>
  );
}
