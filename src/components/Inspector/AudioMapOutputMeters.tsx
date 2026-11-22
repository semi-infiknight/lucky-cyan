import { useEffect, useState } from "react";
import { readAudioMapPeaks } from "../../lib/commands";
import type { AudioMapPatch } from "../../lib/types";

type Readout = { peaks: number[] } | { message: string } | null;

export function AudioMapOutputMeters({ workspaceId, patch, outputLabels }: {
  workspaceId?: string;
  patch: AudioMapPatch | null;
  outputLabels: string[];
}) {
  const [readout, setReadout] = useState<Readout>(null);
  const device = patch?.device_id;
  const patchId = patch?.id;
  const channels = patch?.channels.join(",");

  useEffect(() => {
    if (!workspaceId || !device || !patchId || !channels) return;
    const assignment = { id: patchId, device_id: device, channels: channels.split(",").map(Number) };
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const sample = async () => {
      let interval = 125;
      try {
        const peaks = await readAudioMapPeaks(workspaceId, assignment);
        if (active) setReadout(peaks ? { peaks } : null);
      } catch (error) {
        if (active) setReadout({ message: String(error) });
        interval = 500;
      }
      if (active) timer = setTimeout(sample, interval);
    };
    void sample();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [workspaceId, device, patchId, channels]);

  return <fieldset className="audio-map-calibration audio-map-meters">
    <legend>Live device outputs</legend>
    {!device || !patchId || !channels ? <p>Assign a named output patch to see live meters.</p>
      : !readout ? <p role="status">Waiting for audio output…</p>
        : "message" in readout ? <p role="status">{readout.message}</p>
          : <>
            <p>Combined device signal after master gain</p>
            {readout.peaks.map((peak, index) => {
              const label = `${outputLabels[index] ?? `Output ${index + 1}`} · device ${(patch?.channels[index] ?? index) + 1}`;
              const db = peak > 0 ? 20 * Math.log10(peak) : -Infinity;
              const level = db < -60 ? "Below −60 dBFS" : `${db.toFixed(1)} dBFS`;
              return <div className="audio-map-meter" key={index}>
                <span>{label}</span>
                <meter aria-label={label} aria-valuetext={level} min={-60} max={0} value={Math.min(0, Math.max(-60, db))} />
                <small aria-hidden="true" className={db >= 0 ? "audio-map-clip" : undefined}>{db >= 0 ? `${level} · Full scale` : level}</small>
              </div>;
            })}
          </>}
  </fieldset>;
}
