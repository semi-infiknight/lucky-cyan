import { useState } from "react";
import { setCueFlag } from "../../lib/commands";
import { findCue } from "../../lib/cueTree";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { ToggleRow } from "./Field";

export function CueFlagField({ cueId }: { cueId: string }) {
  const workspaceId = useWorkspaceStore(s => s.workspaceInfo?.instance_id);
  const listId = useWorkspaceStore(s => s.activeCueListId);
  const flagged = useWorkspaceStore(s => {
    const cue = findCue(s.cues, cueId);
    return cue && !!cue.flagged;
  });
  if (!workspaceId || !listId || flagged === undefined) return null;
  return <FlagCheckbox key={`${workspaceId}/${listId}/${cueId}`}
    workspaceId={workspaceId} listId={listId} cueId={cueId} flagged={flagged} />;
}

function FlagCheckbox({ workspaceId, listId, cueId, flagged }: {
  workspaceId: string; listId: string; cueId: string; flagged: boolean;
}) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  async function changeFlag(next: boolean) {
    setSaving(true);
    setError("");
    try {
      await setCueFlag(workspaceId, listId, cueId, next);
      await useWorkspaceStore.getState().refreshCues();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setSaving(false);
    }
  }

  return <>
    <ToggleRow label="Flag for review" checked={flagged} disabled={saving}
      onToggle={value => void changeFlag(value)} />
    {error && <p role="alert" className="inspector-error">{error}</p>}
  </>;
}
