import { useEffect, useState } from "react";
import { pasteCueProperties, previewPasteCueProperties, type CuePropertiesPreview, type CuePropertyGroup } from "../../lib/commands";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { OperatorDialog } from "../Operator/OperatorDialog";

const labels: Record<CuePropertyGroup, string> = {
  color: "Cue color",
  waits: "Pre-wait and post-wait",
  audio_level: "Main audio level",
  audio_matrix: "Audio crosspoint routing",
  audio_fades: "Audio fade-in and fade-out",
  audio_clip_window: "Audio clip start and end",
  pan: "Stereo pan",
  rate: "Audio playback rate",
  picture_geometry: "Picture geometry",
  picture_layer: "Picture layer, opacity and color",
  picture_fades: "Picture fade-in and fade-out",
  fade_duration: "Fade duration",
  fade_curve_shapes: "Fade curve shapes",
};

export function PasteCuePropertiesDialog({ targets, onClose, onRefresh }: {
  targets: string[];
  onClose: () => void;
  onRefresh: () => void;
}) {
  const [selection] = useState(() => {
    const { workspaceInfo, activeCueListId } = useWorkspaceStore.getState();
    return { targets: [...targets], workspaceId: workspaceInfo?.instance_id, cueListId: activeCueListId };
  });
  const [preview, setPreview] = useState<CuePropertiesPreview>();
  const [groups, setGroups] = useState<CuePropertyGroup[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const { targets, workspaceId, cueListId } = selection;
    if (!workspaceId || !cueListId) {
      setError("Open a workspace and select the destination cues first.");
      return;
    }
    let current = true;
    void previewPasteCueProperties(workspaceId, cueListId, targets).then(result => {
      if (current) setPreview(result);
    }).catch(error => { if (current) setError(String(error)); });
    return () => { current = false; };
  }, [selection]);

  const apply = async () => {
    if (!preview || busy || groups.length === 0) return;
    setBusy(true);
    setError("");
    try {
      await pasteCueProperties(preview, selection.targets, groups);
      onRefresh();
      onClose();
    } catch (error) {
      setError(String(error));
      setBusy(false);
    }
  };

  return <OperatorDialog title="Paste Cue Properties" onClose={() => { if (!busy) onClose(); }}>
    {!preview && !error && <p role="status">Checking copied cue and selection…</p>}
    {error && <p role="alert">{error}</p>}
    {preview && <form onSubmit={event => { event.preventDefault(); void apply(); }}>
      <p>From <strong>{[preview.source_number, preview.source_name].filter(Boolean).join(" · ")}</strong> to {selection.targets.length} selected {selection.targets.length === 1 ? "cue" : "cues"}.</p>
      <fieldset className="operator-choices" disabled={busy}>
        <legend>Properties to paste</legend>
        {preview.groups.map(group => <label key={group}>
          <input type="checkbox" checked={groups.includes(group)} onChange={event => {
            const checked = event.currentTarget.checked;
            setGroups(previous => checked ? [...previous, group] : previous.filter(item => item !== group));
          }} /> {labels[group]}
        </label>)}
      </fieldset>
      <footer><button className="primary" type="submit" disabled={busy || groups.length === 0}>{busy ? "Applying…" : "Apply"}</button></footer>
    </form>}
  </OperatorDialog>;
}
