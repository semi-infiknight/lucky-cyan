import { useEffect, useRef, useState } from "react";

import { previewRenumberSelectedCues, renumberSelectedCues, type RenumberSelectionPreview } from "../../lib/commands";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { DragNumber } from "../common/DragNumber";
import { DialogRow, DialogShell, dialogInputStyle } from "../common/DialogShell";

interface Props {
  targetIds: string[];
  onClose: () => void;
  onRefresh: () => void;
}

export function RenumberDialog({ targetIds, onClose, onRefresh }: Props) {
  const [selection] = useState(() => {
    const { workspaceInfo, activeCueListId } = useWorkspaceStore.getState();
    return { ids: [...targetIds], workspaceId: workspaceInfo?.instance_id, cueListId: activeCueListId };
  });
  const [start, setStart] = useState("1");
  const [increment, setIncrement] = useState("1");
  const [preview, setPreview] = useState<{ start: number; increment: number; result: RenumberSelectionPreview }>();
  const [previewError, setPreviewError] = useState("");
  const [applyError, setApplyError] = useState("");
  const [refresh, setRefresh] = useState(0);
  const [busy, setBusy] = useState(false);
  const applying = useRef(false);

  const startValue = start.trim() ? Number(start) : NaN;
  const incrementValue = increment.trim() ? Number(increment) : NaN;
  const valid = Number.isFinite(startValue) && Number.isFinite(incrementValue) && incrementValue !== 0;
  const currentPreview = preview?.start === startValue && preview.increment === incrementValue ? preview.result : undefined;

  useEffect(() => {
    setPreview(undefined);
    setPreviewError("");
    if (!valid) return;
    const { ids, workspaceId, cueListId } = selection;
    if (!workspaceId || !cueListId) {
      setPreviewError("Open a workspace and select the cues to renumber.");
      return;
    }
    let current = true;
    const delay = window.setTimeout(() => {
      void previewRenumberSelectedCues(workspaceId, cueListId, ids, startValue, incrementValue)
        .then(result => { if (current) setPreview({ start: startValue, increment: incrementValue, result }); })
        .catch(error => { if (current) setPreviewError(String(error)); });
    }, 120);
    return () => {
      current = false;
      window.clearTimeout(delay);
    };
  }, [selection, valid, startValue, incrementValue, refresh]);

  const apply = async () => {
    if (!currentPreview || applying.current) return;
    applying.current = true;
    setBusy(true);
    setApplyError("");
    try {
      await renumberSelectedCues(currentPreview);
    } catch (error) {
      setApplyError(String(error));
      setPreview(undefined);
      setRefresh(value => value + 1);
      applying.current = false;
      setBusy(false);
      return;
    }
    onRefresh();
    onClose();
  };

  const cueCount = selection.ids.length;

  return (
    <DialogShell
      title="Renumber Selected Cues"
      subtitle={`${cueCount} cue${cueCount === 1 ? "" : "s"} selected. Occupied numbers are skipped.`}
      confirmLabel={busy ? "Renumbering…" : "Renumber"}
      canConfirm={!!currentPreview && !busy}
      onCancel={() => { if (!applying.current) onClose(); }}
      onConfirm={() => { void apply(); }}
    >
      <DialogRow label="Start at">
        <DragNumber
          autoFocus
          ariaLabel="Start at"
          step="any"
          disabled={busy}
          value={start}
          onChange={(e) => { setStart(e.target.value); setApplyError(""); }}
          style={dialogInputStyle}
        />
      </DialogRow>
      <DialogRow label="Increment">
        <DragNumber
          ariaLabel="Increment"
          step="any"
          disabled={busy}
          value={increment}
          onChange={(e) => { setIncrement(e.target.value); setApplyError(""); }}
          style={dialogInputStyle}
        />
      </DialogRow>

      <div style={{ fontSize: 12, color: "var(--wc-text-secondary)", margin: "12px 0 18px" }}>
        {applyError && <p role="alert">{applyError}</p>}
        {previewError && <p role="alert">{previewError}</p>}
        {!valid && <p role="status">Enter a starting number and a nonzero increment.</p>}
        {valid && !currentPreview && !previewError && <p role="status">Checking cue numbers…</p>}
        {currentPreview && (
          <div style={{ maxHeight: 180, overflow: "auto" }}>
            <table aria-label="Cue number preview" style={{ width: "100%", textAlign: "left" }}>
              <thead><tr><th scope="col">Current</th><th scope="col">New</th></tr></thead>
              <tbody>{currentPreview.changes.map(change => (
                <tr key={change.cue_id}>
                  <td>{change.previous ?? "Unnumbered"}</td><td>{change.number}</td>
                </tr>
              ))}</tbody>
            </table>
          </div>
        )}
      </div>
    </DialogShell>
  );
}
