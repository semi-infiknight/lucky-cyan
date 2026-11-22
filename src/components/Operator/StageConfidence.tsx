import { useEffect, useState } from "react";
import { captureStageSnapshot } from "../../lib/commands";
import { MAIN_OUTPUT_ID, type StageSnapshot } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { Select } from "../common/Select";

// A capture queued for an old selection may still be completing. Do not
// start another render readback if the operator changes or reopens the view.
let captureInFlight: Promise<StageSnapshot> | null = null;
const LIVE_CAPTURE_INTERVAL_MS = 500;

export function requestStageStill(workspaceId: string, outputId: string): Promise<StageSnapshot> | null {
  if (captureInFlight) return null;
  captureInFlight = Promise.resolve()
    .then(() => captureStageSnapshot(workspaceId, outputId))
    .finally(() => { captureInFlight = null; });
  return captureInFlight;
}

function StageStill({ frame, label }: { frame: StageSnapshot; label: string }) {
  return <figure>
    <img src={`data:image/png;base64,${frame.png_base64}`} alt={label} />
    <figcaption>{label} · {frame.source_width} × {frame.source_height} output</figcaption>
  </figure>;
}

export function StageStillComparison({ latest, reference }: {
  latest: StageSnapshot;
  reference: StageSnapshot | null;
}) {
  return <div className="stage-confidence-stills" data-comparing={reference !== null}>
    {reference && <StageStill label="Reference frame" frame={reference} />}
    <StageStill label="Latest capture" frame={latest} />
  </div>;
}

function StagePicture({ workspaceId, outputId }: { workspaceId: string; outputId: string }) {
  const [latest, setLatest] = useState<StageSnapshot | null>(null);
  const [reference, setReference] = useState<StageSnapshot | null>(null);
  const [waiting, setWaiting] = useState(false);
  const [live, setLive] = useState(false);
  const [failure, setFailure] = useState("");

  useEffect(() => {
    if (!live) return;
    let active = true;
    let timer: number | undefined;

    const readFrame = async () => {
      if (!active || document.hidden) return;
      const request = requestStageStill(workspaceId, outputId);
      if (request) {
        try {
          const frame = await request;
          if (active && !document.hidden) setLatest(frame);
        } catch (error) {
          if (active && !document.hidden) setFailure(String(error));
          if (active) setLive(false);
          return;
        }
      }
      if (active && !document.hidden) timer = window.setTimeout(readFrame, LIVE_CAPTURE_INTERVAL_MS);
    };

    const stopIfHidden = () => {
      if (document.hidden) {
        active = false;
        window.clearTimeout(timer);
        setLive(false);
      }
    };
    document.addEventListener("visibilitychange", stopIfHidden);
    stopIfHidden();
    void readFrame();
    return () => {
      active = false;
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", stopIfHidden);
    };
  }, [live, workspaceId, outputId]);

  const takeSnapshot = async () => {
    const request = requestStageStill(workspaceId, outputId);
    if (!request) {
      if (!waiting) setFailure("A stage capture is still finishing. Try again shortly.");
      return;
    }
    setFailure("");
    setWaiting(true);
    try {
      setLatest(await request);
      setFailure("");
    } catch (error) {
      setFailure(String(error));
    } finally {
      setWaiting(false);
    }
  };

  return <>
    <button type="button" onClick={() => void takeSnapshot()} disabled={waiting || live}>
      {latest ? "Retake frame" : "Capture frame"}
    </button>
    <button type="button" disabled={waiting} onClick={() => {
      setFailure("");
      setLive(!live);
    }}>{live ? "Stop live preview" : "Start live preview"}</button>
    {latest && <button type="button" onClick={() => setReference(latest)} disabled={waiting || reference === latest}>
      {reference ? "Replace reference" : "Set reference"}
    </button>}
    {reference && <button type="button" onClick={() => setReference(null)}>Clear reference</button>}
    <div className="stage-confidence-result" aria-busy={waiting}>
      {!latest && !waiting && !live && !failure && <p>Show the selected output, then capture a frame from the desk.</p>}
      {waiting && <p role="status">Reading the output…</p>}
      {live && <p role="status">Live preview · up to 2 frames per second</p>}
      {failure && <p role="alert">{failure}</p>}
      {latest && <StageStillComparison latest={latest} reference={reference} />}
    </div>
  </>;
}

export function StageConfidence({ workspaceId }: { workspaceId: string }) {
  const outputs = useWorkspaceStore(state => state.videoOutputs);
  const [selection, setSelection] = useState(MAIN_OUTPUT_ID);
  const selected = outputs.find(output => output.id === selection);
  const outputId = selected?.id ?? MAIN_OUTPUT_ID;

  return <section className="stage-confidence" aria-label="Stage snapshot">
    <Select value={outputId} onChange={event => setSelection(event.target.value)}
      ariaLabel={`Video output, ${selected?.name ?? "Main"}`}
      style={{ width: "min(15rem, 100%)" }}>
      {!outputs.some(output => output.id === MAIN_OUTPUT_ID) && <option value={MAIN_OUTPUT_ID}>Main</option>}
      {outputs.map(output => <option key={output.id} value={output.id}>{output.name}</option>)}
    </Select>
    <StagePicture key={`${workspaceId}:${outputId}`} workspaceId={workspaceId} outputId={outputId} />
  </section>;
}
