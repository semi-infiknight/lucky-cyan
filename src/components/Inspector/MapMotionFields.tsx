import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCue, listAudioMaps } from "../../lib/commands";
import type { AudioCueData, AudioMap, MapMotion } from "../../lib/types";
import { Select } from "../common/Select";
import { Grid2, MiniField, Section, inputStyle } from "./Field";

export function MapMotionFields({ targetId, motion, onApply }: {
  targetId?: string;
  motion?: MapMotion | null;
  onApply: (motion: MapMotion | null) => void;
}) {
  const [calibration, setCalibration] = useState<{ cueId: string; map: AudioMap; appliedStart?: string }>();
  const [draft, setDraft] = useState<MapMotion>();
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (!targetId) return;
    let mounted = true;
    let request = 0;
    const refresh = async () => {
      const ticket = ++request;
      setError(undefined);
      try {
        const [raw, library] = await Promise.all([getCue(targetId), listAudioMaps()]);
        if (!mounted || ticket !== request) return;
        const cue = raw as AudioCueData;
        const point = cue.point_mix;
        const map = point?.map ? library.maps.find(map =>
          map.id === point.map?.id && map.revision === point.map.revision
        ) : undefined;
        if (!map || !point || !map.patch.device_id || map.patch.id !== cue.output_patch_id) {
          setCalibration(undefined);
          return;
        }
        const appliedStart = map.marks.find(mark => mark.position.x === point.source.x &&
          mark.position.y === point.source.y)?.id;
        setCalibration({ cueId: targetId, map, appliedStart });
      } catch (failure) {
        if (mounted && ticket === request) {
          setCalibration(undefined);
          setError(String(failure));
        }
      }
    };
    void refresh();
    const subscription = listen("audio-maps-changed", refresh);
    return () => {
      mounted = false;
      void subscription.then(unlisten => unlisten()).catch(console.error);
    };
  }, [targetId]);

  const map = calibration?.cueId === targetId ? calibration?.map : undefined;
  const matches = (candidate?: MapMotion | null) =>
    candidate && map && candidate.map_id === map.id && candidate.revision === map.revision;
  const selected = matches(draft) ? draft : matches(motion) ? motion : undefined;
  const start = selected?.start_mark ?? calibration?.appliedStart ?? "";
  const end = selected?.end_mark ?? "";
  const startPosition = map?.marks.find(mark => mark.id === start)?.position;
  const endPosition = map?.marks.find(mark => mark.id === end)?.position;
  const canApply = !!(startPosition && endPosition && (startPosition.x !== endPosition.x || startPosition.y !== endPosition.y));
  const edit = (changed: "start_mark" | "end_mark", id: string) => {
    if (map) setDraft({ map_id: map.id, revision: map.revision, start_mark: start, end_mark: end, [changed]: id });
  };

  return (
    <Section title="Audio map movement">
      {!map && <p>Choose one Audio cue with a current calibrated map applied to an explicitly assigned output patch.</p>}
      {error && <p role="alert">{error}</p>}
      {map && <>
        <p>{map.name}. Start the target Audio cue at the chosen mark before calling this Fade.</p>
        <Grid2>
          {(["start_mark", "end_mark"] as const).map((field) => (
            <MiniField key={field} label={field === "start_mark" ? "Start mark" : "Destination mark"}>
              <Select style={inputStyle} value={field === "start_mark" ? start : end}
                onChange={(event) => edit(field, event.target.value)}>
                <option value="">Choose a mark</option>
                {map.marks.map(mark => <option key={mark.id} value={mark.id}>{mark.name}</option>)}
              </Select>
            </MiniField>
          ))}
        </Grid2>
        <p>Applying uses this Fade for movement only and sets it to manual continuation.</p>
        <button type="button" disabled={!canApply} onClick={() => onApply({
          map_id: map.id, revision: map.revision, start_mark: start, end_mark: end,
        })}>Apply movement</button>
      </>}
      {motion && <button type="button" onClick={() => {
        setDraft(undefined);
        onApply(null);
      }}>Clear movement</button>}
    </Section>
  );
}
