import { useState } from "react";
import { setSecondCueColor } from "../../lib/commands";
import { COLOR_OPTIONS } from "../../lib/cueColors";
import { findCue } from "../../lib/cueTree";
import type { CueColor } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { Select } from "../common/Select";
import { Field, inputStyle } from "./Field";

export function SecondCueColorField({ cueId }: { cueId: string }) {
  const workspaceId = useWorkspaceStore(s => s.workspaceInfo?.instance_id);
  const listId = useWorkspaceStore(s => s.activeCueListId);
  const cue = useWorkspaceStore(s => findCue(s.cues, cueId));
  if (!workspaceId || !listId || !cue) return null;

  return <SecondColorPicker key={`${workspaceId}/${listId}/${cueId}`}
    workspaceId={workspaceId} listId={listId} cueId={cueId} value={cue.second_color ?? null} />;
}

function SecondColorPicker({ workspaceId, listId, cueId, value }: {
  workspaceId: string; listId: string; cueId: string; value: CueColor | null;
}) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  async function changeColor(next: CueColor | null) {
    if (next === value) return;
    setSaving(true);
    setError("");
    try {
      await setSecondCueColor(workspaceId, listId, cueId, next);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setSaving(false);
    }
  }

  return <>
    <Field label="Second color">
      <Select ariaLabel="Second color after first start" style={inputStyle} value={value ?? ""}
        disabled={saving} onChange={({ target }) => {
          const option = COLOR_OPTIONS.find(color => color.value === target.value);
          void changeColor(option?.value ?? null);
        }}>
        <option value="">Keep original color</option>
        {COLOR_OPTIONS.map(color => <option key={color.value} value={color.value}>
          {color.value === "none" ? "No highlight" : <>
            <span aria-hidden="true" style={{ display: "inline-block", width: 10, height: 10,
              borderRadius: 2, marginInlineEnd: 7, background: color.hex }} />
            {color.label}
          </>}
        </option>)}
      </Select>
    </Field>
    {error && <p role="alert" className="inspector-error">{error}</p>}
  </>;
}
