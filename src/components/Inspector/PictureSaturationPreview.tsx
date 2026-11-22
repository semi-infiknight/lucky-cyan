import { useEffect, useRef, useState } from "react";
import { cancelPictureSaturationPreview, previewPictureSaturation } from "../../lib/commands";
import type { CueSummary } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";

export function PictureSaturationPreview({ fadeId, target, saturation, fadeRunning }: {
  fadeId: string; target: CueSummary; saturation: number; fadeRunning: boolean;
}) {
  const workspaceId = useWorkspaceStore(store => store.workspaceInfo?.instance_id);
  const listId = useWorkspaceStore(store => store.activeCueListId);
  const refreshCues = useWorkspaceStore(store => store.refreshCues);
  const voice = target.picture_voice_id;
  useEffect(() => {
    if (workspaceId && listId && target.state === "running") void refreshCues();
  }, [workspaceId, listId, target.id, target.state, refreshCues]);
  if (!workspaceId || !listId || !voice || target.state !== "running" || target.is_disabled || fadeRunning) {
    return <button type="button" disabled title="Start the armed picture target to preview the Fade">Preview saturation</button>;
  }
  return <LivePreview key={`${workspaceId}/${listId}/${fadeId}/${target.id}/${voice}/${saturation}`} request={{
    workspace_id: workspaceId, list_id: listId, fade_id: fadeId, target_id: target.id, voice_id: voice, saturation,
  }} />;
}

function LivePreview({ request }: { request: Parameters<typeof previewPictureSaturation>[0] }) {
  const [previewing, setPreviewing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const alive = useRef(false);
  const token = useRef<string>();
  const expiry = useRef<ReturnType<typeof setTimeout>>();
  const { voice_id: voice } = request;

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      clearTimeout(expiry.current);
      if (token.current) void cancelPictureSaturationPreview(voice, token.current).catch(() => {});
    };
  }, [voice]);

  async function togglePreview() {
    setBusy(true);
    setError("");
    try {
      if (token.current) {
        await cancelPictureSaturationPreview(voice, token.current);
        clearTimeout(expiry.current);
        token.current = undefined;
        if (alive.current) setPreviewing(false);
      } else {
        const [started, duration] = await previewPictureSaturation(request);
        if (!alive.current) {
          void cancelPictureSaturationPreview(voice, started).catch(() => {});
          return;
        }
        token.current = started;
        setPreviewing(true);
        expiry.current = setTimeout(() => {
          token.current = undefined;
          setPreviewing(false);
        }, duration);
      }
    } catch (cause) {
      if (alive.current) {
        setError(String(cause));
        void useWorkspaceStore.getState().refreshCues();
      }
    } finally {
      if (alive.current) setBusy(false);
    }
  }

  return <>
    <button type="button" disabled={busy} onClick={() => void togglePreview()}>
      {previewing ? "End saturation preview" : "Preview saturation"}
    </button>
    {error && <p className="inspector-error" role="alert">{error}</p>}
    {previewing && <span role="status"> Previewing live picture</span>}
  </>;
}
