import { useMemo, useRef, useState } from "react";
import { formatClock } from "../lib/timeline";
import type { CueListWindowSnapshot, CueSummary, SequentialWindowAction } from "../lib/types";
import { useTimingStore } from "../stores/timingStore";
import "../components/CueList/cueDesk.css";

interface CueRow {
  cue: CueSummary;
  depth: number;
}

export function flattenCueRows(cues: CueSummary[]): CueRow[] {
  const rows: CueRow[] = [];
  function add(cues: CueSummary[], depth: number) {
    for (const cue of cues) {
      rows.push({ cue, depth });
      if (cue.children) add(cue.children, depth + 1);
    }
  }
  add(cues, 0);
  return rows;
}

function cueTitle(cue: CueSummary): string {
  return [cue.number, cue.name || cue.cue_type].filter(Boolean).join(" ");
}

function PlaybackRow({ row, workspace, standby, pending, setStandby }: {
  row: CueRow;
  workspace: string;
  standby: boolean;
  pending: boolean;
  setStandby?: () => void;
}) {
  const { cue, depth } = row;
  const timing = useTimingStore(store => store.timings[cue.id]);
  const remaining = timing?.workspace_id === workspace ? timing.remaining_ms : null;
  const active = cue.state === "running" || (cue.state === "paused" && !cue.is_prepared);
  const status = cue.is_prepared || cue.loaded_time_ms != null ? "Loaded"
    : active ? (cue.state === "running" ? "Running" : "Paused")
    : cue.is_disabled ? "Disarmed"
      : standby ? "Standby"
        : cue.is_loading ? "Loading"
          : "";
  return <tr data-standby={standby} data-state={cue.state}>
    <td className="list-window-number">{cue.number || "—"}</td>
    <th scope="row" style={{ paddingInlineStart: `${8 + Math.min(depth, 6) * 12}px` }}>
      {cue.name || cue.cue_type}
    </th>
    <td>{status}</td>
    <td className="list-window-number">{active && remaining != null ? formatClock(remaining) : "—"}</td>
    <td>{setStandby && <button type="button" onClick={setStandby} disabled={pending}
      aria-label={`Set standby to ${cueTitle(cue)}`}>Set standby</button>}</td>
  </tr>;
}

export function SequentialListPlayback({ snapshot, onAction }: {
  snapshot: CueListWindowSnapshot;
  onAction: (action: SequentialWindowAction) => Promise<void>;
}) {
  const rows = useMemo(() => flattenCueRows(snapshot.cues), [snapshot.cues]);
  const [pending, setPending] = useState(false);
  const pendingAction = useRef(false);
  const standbyId = snapshot.standby_child_id ?? snapshot.standby_id;
  const standby = rows.find(({ cue }) => cue.id === standbyId)?.cue;
  const canGo = Boolean(snapshot.standby_id && standby && !standby.is_disabled && !standby.is_loading && !standby.is_broken);
  const canPause = snapshot.cues.some(cue => cue.state === "running");
  const canResume = snapshot.cues.some(cue => cue.state === "paused" && !cue.is_disabled && !cue.is_prepared);

  async function dispatch(action: SequentialWindowAction) {
    if (pendingAction.current) return;
    pendingAction.current = true;
    setPending(true);
    try {
      await onAction(action);
    } finally {
      pendingAction.current = false;
      setPending(false);
    }
  }

  const go = () => {
    if (snapshot.standby_id && canGo) void dispatch({
      kind: "go", cue_id: snapshot.standby_id, child_id: snapshot.standby_child_id,
    });
  };
  const stop = () => { void onAction({ kind: "stop_list" }); };

  return <section className="list-window-playback" aria-label="Cue list playback" onKeyDown={event => {
    if (event.repeat) {
      if (event.code === "Space" || event.key === "Enter") event.preventDefault();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      stop();
    } else if (event.code === "Space" && !(event.target as HTMLElement).closest("button")) {
      event.preventDefault();
      go();
    }
  }}>
    <div className="list-window-transport">
      <div><span>Next standby</span><strong>{standby ? cueTitle(standby) : "No cue at playhead"}</strong></div>
      <button type="button" className="list-window-go" disabled={pending || !canGo} onClick={go}>GO</button>
      <button type="button" disabled={pending || !canPause} onClick={() => void dispatch({ kind: "pause_list" })}>Pause list</button>
      <button type="button" disabled={pending || !canResume} onClick={() => void dispatch({ kind: "resume_list" })}>Resume list</button>
      <button type="button" onClick={stop}>Stop list</button>
    </div>
    <div className="list-window-rows" role="region" aria-label="Sequential cues" tabIndex={0}>
      <table>
        <thead><tr><th scope="col">Number</th><th scope="col">Cue</th><th scope="col">State</th>
          <th scope="col">Remaining</th><th scope="col">Standby</th></tr></thead>
        <tbody>{rows.map(row => <PlaybackRow key={row.cue.id} row={row} workspace={snapshot.workspace_id}
          standby={row.cue.id === standbyId}
          pending={pending}
          setStandby={row.depth === 0 && !row.cue.is_disabled
            ? () => void dispatch({ kind: "set_standby", cue_id: row.cue.id })
            : undefined} />)}</tbody>
      </table>
    </div>
  </section>;
}
