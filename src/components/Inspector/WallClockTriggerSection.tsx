import { useEffect, useState } from "react";
import { armWallClockTrigger, setWallClockTrigger } from "../../lib/commands";
import type { CueSummary, WallClockTrigger } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";

const weekdays = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export function WallClockTriggerSection({ cue, onSave }: { cue: CueSummary; onSave?: () => void }) {
  const { cues, cueLists, activeCueListId, workspaceInfo } = useWorkspaceStore();
  const saved = cue.wall_clock_trigger;
  const savedTime = saved?.time ?? "12:00:00";
  const savedDays = saved?.weekdays ?? 127;
  const [time, setTime] = useState(savedTime);
  const [days, setDays] = useState(savedDays);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const list = cueLists.find(item => item.id === activeCueListId);
  const independentPad = list?.mode === "cart" && !list.chain_cart_cues;
  const eligible = cues.some(item => item.id === cue.id) &&
    (independentPad || cue.continue_mode === "do_not_continue") &&
    cue.group_mode !== "sequential" && cue.group_mode !== "playlist" && cue.group_mode !== "start_first_and_enter" &&
    cue.cue_type !== "goto" && cue.cue_type !== "start";
  const armed = Boolean(cue.wall_clock_armed);
  const normalizedTime = time.length === 5 ? time + ":00" : time;
  const isValid = /^([01]\d|2[0-3]):[0-5]\d:[0-5]\d$/.test(normalizedTime) && days !== 0;
  const changed = normalizedTime !== savedTime || days !== savedDays;

  useEffect(() => {
    setTime(savedTime);
    setDays(savedDays);
  }, [savedTime, savedDays]);

  async function save(trigger: WallClockTrigger | null) {
    if (!workspaceInfo || !activeCueListId) return;
    setBusy(true);
    setError("");
    try {
      await setWallClockTrigger(workspaceInfo.instance_id, activeCueListId, cue.id, trigger);
      await onSave?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function arm() {
    if (!workspaceInfo || !activeCueListId) return;
    setBusy(true);
    setError("");
    try {
      await armWallClockTrigger(workspaceInfo.instance_id, activeCueListId, cue.id, !armed);
      await onSave?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return <fieldset disabled={busy} style={{ marginBottom: 16 }}>
    <legend>Wall clock trigger</legend>
    <p>{eligible
      ? "Use the desk’s local time. Arming lasts until this show closes or you use Hard Stop All."
      : "Choose a top-level cue that runs independently of the playhead."}</p>
    {(eligible || saved) && <>
      <label>Time{" "}<input type="time" step={1} value={time} disabled={!eligible}
        onChange={e => setTime(e.currentTarget.value)} /></label>
      <div role="group" aria-label="Trigger days">
        {weekdays.map((day, index) => <label key={day} style={{ marginRight: 10 }}>
          <input type="checkbox" disabled={!eligible} checked={(days & (1 << index)) !== 0}
            onChange={() => setDays(value => value ^ (1 << index))} /> {day}
        </label>)}
      </div>
      <p role="status">{armed ? "Armed for this session" : saved ? "Saved, disarmed" : "No schedule"}</p>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button type="button" disabled={!eligible || !isValid || (!!saved && !changed)}
          onClick={() => void save({ time: normalizedTime, weekdays: days })}>Save schedule</button>
        {saved && <>
          <button type="button" disabled={!armed && (!eligible || changed || cue.is_disabled)}
            onClick={() => void arm()}>{armed ? "Disarm clock" : "Arm for this session"}</button>
          <button type="button" onClick={() => void save(null)}>Remove schedule</button>
        </>}
      </div>
    </>}
    {error && <p role="alert">{error}</p>}
  </fieldset>;
}
