import { useEffect, useRef, useState } from "react";
import { captureStageSnapshot, readStagePreview, setStagePreview } from "../../lib/commands";
import { MAIN_OUTPUT_ID, type StageSnapshot } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { Select } from "../common/Select";
import { decodeStagePreview } from "./stagePreviewFrame";

// A capture queued for an old selection may still be completing. Do not
// start another render readback if the operator changes or reopens the view.
let captureInFlight: Promise<StageSnapshot> | null = null;
const PREVIEW_INTERVAL_MS = 1000 / 24;
let previewControl = Promise.resolve();

function changePreview(workspaceId: string, outputId: string, sessionId: string, enabled: boolean) {
  const request = previewControl.then(() => setStagePreview(workspaceId, outputId, sessionId, enabled));
  previewControl = request.catch(() => {});
  return request;
}

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
  const [liveSize, setLiveSize] = useState<{ width: number; height: number } | null>(null);
  const [failure, setFailure] = useState("");
  const previewCanvas = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    if (!live) return;
    if (document.hidden) { setLive(false); return; }
    const session = crypto.randomUUID();
    let active = true;
    let after = 0;
    let timer: number | undefined;
    const started = changePreview(workspaceId, outputId, session, true);
    const report = (error: unknown) => {
      if (!active) return;
      setFailure(String(error));
      setLive(false);
    };

    const readFrame = async () => {
      if (!active || document.hidden) return;
      const startedAt = performance.now();
      try {
        const reply = await readStagePreview(workspaceId, outputId, session, after);
        if (!active || document.hidden) return;
        const frame = decodeStagePreview(reply);
        if (frame) {
          const canvas = previewCanvas.current;
          const context = canvas?.getContext("2d", { alpha: false });
          if (!canvas || !context) throw new Error("The live preview canvas is unavailable.");
          if (canvas.width !== frame.width || canvas.height !== frame.height) {
            canvas.width = frame.width;
            canvas.height = frame.height;
          }
          context.putImageData(new ImageData(frame.pixels, frame.width, frame.height), 0, 0);
          after = frame.serial;
          setLiveSize(prior => prior?.width === frame.sourceWidth && prior.height === frame.sourceHeight
            ? prior : { width: frame.sourceWidth, height: frame.sourceHeight });
        }
      } catch (error) {
        report(error);
        return;
      }
      if (active) timer = window.setTimeout(readFrame, Math.max(0, PREVIEW_INTERVAL_MS - (performance.now() - startedAt)));
    };

    const stopIfHidden = () => {
      if (document.hidden) {
        active = false;
        window.clearTimeout(timer);
        setLive(false);
      }
    };
    document.addEventListener("visibilitychange", stopIfHidden);
    void started.then(readFrame, report);
    return () => {
      active = false;
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", stopIfHidden);
      void changePreview(workspaceId, outputId, session, false).catch(() => {});
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
      setLiveSize(null);
      setFailure("");
    } catch (error) {
      setFailure(String(error));
    } finally {
      setWaiting(false);
    }
  };

  const keepReference = () => {
    const canvas = previewCanvas.current;
    if (liveSize && canvas) {
      const png = canvas.toDataURL("image/png").split(",", 2)[1];
      if (png) setReference({ source_width: liveSize.width, source_height: liveSize.height, png_base64: png });
    } else if (latest) {
      setReference(latest);
    }
  };

  return <>
    <button type="button" onClick={() => void takeSnapshot()} disabled={waiting || live}>
      {latest || liveSize ? "Retake frame" : "Capture frame"}
    </button>
    <button type="button" disabled={waiting} onClick={() => {
      setFailure("");
      if (!live) setLiveSize(null);
      setLive(!live);
    }}>{live ? "Stop live preview" : "Start live preview"}</button>
    {(liveSize || (!live && latest)) && <button type="button" onClick={keepReference}
      disabled={waiting || (!liveSize && reference === latest)}>
      {reference ? "Replace reference" : "Set reference"}
    </button>}
    {reference && <button type="button" onClick={() => setReference(null)}>Clear reference</button>}
    <div className="stage-confidence-result" aria-busy={waiting}>
      {!latest && !liveSize && !waiting && !live && !failure && <p>Show the selected output, then capture a frame from the desk.</p>}
      {waiting && <p role="status">Reading the output…</p>}
      {live && <p role="status">{liveSize ? "Live preview · up to 24 frames per second" : "Waiting for a stage frame…"}</p>}
      {failure && <p role="alert">{failure}</p>}
      {(live || liveSize) ? <div className="stage-confidence-stills" data-comparing={reference !== null}>
        {reference && <StageStill label="Reference frame" frame={reference} />}
        <figure hidden={!liveSize}>
          <canvas ref={previewCanvas} role="img" aria-label="Stage preview" />
          {liveSize && <figcaption>{live ? "Live preview" : "Held preview frame"} · {liveSize.width} × {liveSize.height} output</figcaption>}
        </figure>
      </div> : latest && <StageStillComparison latest={latest} reference={reference} />}
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
