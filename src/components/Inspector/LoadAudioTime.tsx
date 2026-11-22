import { useState } from "react";
import { loadAudioToTime } from "../../lib/commands";
import type { CueSummary } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { inputStyle, Section } from "./Field";

export function LoadAudioTime({ cue }: { cue: CueSummary }) {
  const workspaceId = useWorkspaceStore(s => s.workspaceInfo?.instance_id);
  const listId = useWorkspaceStore(s => s.activeCueListId);
  const topLevel = useWorkspaceStore(s => s.cues.some(row => row.id === cue.id));
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  if (!workspaceId || !listId || !topLevel || cue.state !== "standby") return null;

  const loadedTime = cue.loaded_time_ms;
  const offset = Math.round(Number(draft) * 1000);
  const canLoad = draft.trim().length > 0 && Number.isSafeInteger(offset)
    && !cue.is_disabled && !cue.is_loading && !cue.is_broken;

  async function changeTime(next: number | null) {
    if (!workspaceId || !listId || busy) return;
    setBusy(true);
    setError("");
    try {
      await loadAudioToTime(workspaceId, listId, cue.id, next);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Section title="Load to Time" hint="Enter seconds from the clip start. A negative time counts back from its end.">
      <form onSubmit={event => {
        event.preventDefault();
        if (canLoad) void changeTime(offset);
      }}>
        <div style={{ display: "flex", gap: 6 }}>
          <input aria-label="Load time in seconds" type="number" step="0.001"
            placeholder="0.000" style={inputStyle} value={draft}
            onChange={event => setDraft(event.target.value)} />
          <button type="submit" disabled={busy || !canLoad}>Load</button>
          {loadedTime !== undefined && (
            <button type="button" disabled={busy} onClick={() => void changeTime(null)}>Clear</button>
          )}
        </div>
      </form>
      {loadedTime !== undefined && (
        <p role="status">Next start at {(loadedTime / 1000).toFixed(3)} s</p>
      )}
      {error && <p role="alert" className="inspector-error">{error}</p>}
    </Section>
  );
}
