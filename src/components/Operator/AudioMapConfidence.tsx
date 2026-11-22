import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getOutputPatchTable, listAudioMaps } from "../../lib/commands";
import type { AudioMap, OutputPatch } from "../../lib/types";
import { markerPosition, PointProjection, type PointView } from "../Inspector/AudioMapEditor";
import { AudioMapOutputMeters } from "../Inspector/AudioMapOutputMeters";
import { Select } from "../common/Select";
import "../Inspector/pointPanner.css";

type AudioMapReadout = { maps: AudioMap[]; patches: OutputPatch[] } | { error: string };

export function AudioMapConfidence({ workspaceId }: { workspaceId: string }) {
  const [readout, setReadout] = useState<AudioMapReadout>();
  const [mapId, setMapId] = useState("");
  const [view, setView] = useState<PointView>("plan");

  useEffect(() => {
    let disposed = false;
    let revision = 0;
    const listeners: UnlistenFn[] = [];
    async function refresh() {
      const request = ++revision;
      try {
        const [library, outputs] = await Promise.all([listAudioMaps(), getOutputPatchTable()]);
        if (disposed || request !== revision) return;
        if (library.workspace_id !== workspaceId || outputs.workspace_id !== workspaceId) {
          throw new Error("The open workspace changed. Reopen Cue monitor.");
        }
        setReadout({ maps: library.maps, patches: outputs.patches });
      } catch (error) {
        if (!disposed && request === revision) setReadout({ error: String(error) });
      }
    }
    const registrations = ["audio-maps-changed", "output-patches-changed"].map(async event => {
      const remove = await listen(event, refresh);
      if (disposed) remove();
      else listeners.push(remove);
    });
    void Promise.allSettled(registrations).then(() => { if (!disposed) void refresh(); });
    return () => { disposed = true; listeners.forEach(remove => remove()); };
  }, [workspaceId]);

  if (!readout) return <p className="cue-monitor-notice" role="status">Reading audio maps…</p>;
  if ("error" in readout) return <p className="cue-monitor-notice" role="alert">{readout.error}</p>;
  const map = readout.maps.find(map => map.id === mapId) ?? readout.maps[0];
  if (!map) return <p className="cue-monitor-notice">Save an audio map in the Point panner to monitor it here.</p>;

  const patch = readout.patches.find(patch => patch.id === map.patch.id && patch.device_id === map.patch.device_id
    && patch.channels.length === map.patch.channels.length
    && patch.channels.every((channel, index) => channel === map.patch.channels[index]));
  const labels = map.patch.channels.map((_, index) => patch?.channel_names?.[index] || `Output ${index + 1}`);

  return <section className="audio-map-confidence" aria-label="Audio map confidence">
    <div className="audio-map-confidence-controls">
      <Select value={map.id} onChange={event => setMapId(event.target.value)} ariaLabel="Audio map"
        style={{ width: "min(100%, 16rem)" }}>
        {readout.maps.map(map => <option key={map.id} value={map.id}>{map.name}</option>)}
      </Select>
      <PointProjection label="Calibration view" view={view} onChange={setView} />
    </div>
    <div className="audio-map-confidence-marks">
      <div className="point-plane" aria-hidden="true">
        {map.marks.map((mark, index) => <span key={mark.id} className="audio-map-mark"
          style={markerPosition(mark.position, view)}>{index + 1}</span>)}
      </div>
      <ol aria-label="Saved calibration marks">
        {map.marks.map(mark => <li key={mark.id}>
          <strong>{mark.name}</strong>
          <span>X {mark.position.x} · {view === "plan" ? `Y ${mark.position.y}` : `Z ${mark.position.z ?? 0}`}</span>
        </li>)}
      </ol>
    </div>
    {patch && <p className="audio-map-confidence-patch">Output patch {patch.name}</p>}
    {map.patch.id && !patch
      ? <p className="audio-map-confidence-patch" role="status">The assigned output patch changed. Open the Point panner to review this map.</p>
      : <AudioMapOutputMeters key={`${workspaceId}:${map.id}:${map.revision}`} workspaceId={workspaceId}
        patch={map.patch} outputLabels={labels} />}
  </section>;
}
