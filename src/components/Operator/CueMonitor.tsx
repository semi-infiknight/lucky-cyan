import { useEffect, useId, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { formatClock } from "../../lib/timeline";
import type { CueMonitorSnapshot, HealthAlert, MonitorCue, MonitorRun } from "../../lib/types";
import { useTimingStore, type CueTiming } from "../../stores/timingStore";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { watchCueMonitor } from "./cueMonitorRefresh";
import { StageConfidence } from "./StageConfidence";
import "./cueMonitor.css";

export function monitorTimes(timing: CueTiming | undefined, workspaceId: string): [string, string] {
  if (timing?.workspace_id !== workspaceId) return ["—", "—"];
  return [
    formatClock(timing.action_elapsed_ms),
    timing.remaining_ms === null ? "—" : formatClock(timing.remaining_ms),
  ];
}

function MonitorCueName({ cue, onInspect }: { cue: MonitorCue; onInspect?: () => void }) {
  const name = <>
    {cue.number && <b>{cue.number}</b>}
    <span>{cue.name || cue.kind}</span>
  </>;
  return onInspect ? <button type="button" className="cue-monitor-cue-name cue-monitor-inspect"
    aria-label={`Inspect cue ${[cue.number, cue.name || cue.kind].filter(Boolean).join(" ")}`}
    onClick={onInspect}>{name}</button> : <span className="cue-monitor-cue-name">{name}</span>;
}

function MonitorRunRow({ run, workspaceId, onInspect }: {
  run: MonitorRun; workspaceId: string; onInspect?: () => void;
}) {
  const [elapsed, remaining] = useTimingStore(useShallow(s => monitorTimes(s.timings[run.cue.id], workspaceId)));
  return <tr data-state={run.state}>
    <th scope="row" data-nested={Boolean(run.parent_id)}><MonitorCueName cue={run.cue} onInspect={onInspect} /></th>
    <td className="cue-monitor-run-state">{run.state === "paused" ? "Paused" : "Running"}</td>
    <td className="cue-monitor-time">{elapsed}</td>
    <td className="cue-monitor-time">{remaining}</td>
  </tr>;
}

export function CueMonitorContents({ status, alerts, inspection }: {
  status: CueMonitorSnapshot;
  alerts: HealthAlert[];
  inspection?: { listId: string; select: (cueId: string) => void };
}) {
  return <>
    <div className="cue-monitor-lists">
      {status.lists.map(list => {
        const inspect = inspection?.listId === list.id && list.id === status.active_cue_list_id
          ? inspection.select : undefined;
        const standby = list.standby;
        return <section key={list.id} className="cue-monitor-list">
          <header>
            <h3>{list.name}</h3>
            {list.id === status.active_cue_list_id && <span>Selected on desk</span>}
          </header>
          <div className="cue-monitor-standby">
            <span>Standby</span>
            {standby ? <>
              <MonitorCueName cue={standby} onInspect={inspect && (() => inspect(standby.id))} />
              {standby.is_disabled && <span className="cue-monitor-disarmed">Disarmed</span>}
            </> : <span>No cue at playhead</span>}
          </div>
          {list.active.length ? <table aria-label={`${list.name} active cues`}>
            <thead><tr>
              <th scope="col">Active cue</th><th scope="col">State</th><th scope="col">Elapsed</th><th scope="col">Remaining</th>
            </tr></thead>
            <tbody>{list.active.map(run => <MonitorRunRow key={run.cue.id} run={run}
              workspaceId={status.workspace_id} onInspect={inspect && (() => inspect(run.cue.id))} />)}</tbody>
          </table> : <p className="cue-monitor-idle">Idle</p>}
        </section>;
      })}
    </div>
    <section className="cue-monitor-health">
      <h3>Runtime alerts</h3>
      {alerts.length ? <ul>{alerts.map(alert => <li key={alert.key} data-level={alert.level}>{alert.message}</li>)}</ul>
        : <p>No runtime alerts</p>}
    </section>
  </>;
}

export function CueMonitor({ workspaceId, onClose, onInspect }: {
  workspaceId: string;
  onClose: () => void;
  onInspect?: (workspaceId: string, listId: string, cueId: string) => boolean;
}) {
  const [showStage, setShowStage] = useState(false);
  const [snapshot, setSnapshot] = useState<CueMonitorSnapshot | null>(null);
  const [notice, setNotice] = useState("Reading the open show…");
  const alerts = useWorkspaceStore(s => s.healthAlerts);
  const activeListId = useWorkspaceStore(s => s.activeCueListId);
  const closeButton = useRef<HTMLButtonElement>(null);
  const refresh = useRef<() => void>(() => {});
  const titleId = useId();

  useEffect(() => { closeButton.current?.focus(); }, []);

  useEffect(() => {
    const monitor = watchCueMonitor(workspaceId, setSnapshot, setNotice);
    refresh.current = monitor.refresh;
    return () => { monitor.dispose(); refresh.current = () => {}; };
  }, [workspaceId]);

  return (
    <section id="cue-monitor" className="cue-monitor" aria-labelledby={titleId}
      onKeyDown={event => {
        // Escape is the desk's Stop and Hard Stop key even with the monitor focused.
        if (event.key !== "Escape") event.stopPropagation();
      }}>
      <header className="cue-monitor-heading">
        <div>
          <h2 id={titleId}>Cue monitor</h2>
          {snapshot && <p>{snapshot.name}</p>}
        </div>
        <div className="cue-monitor-actions">
          <button type="button" aria-pressed={showStage} onClick={() => setShowStage(show => !show)}>
            Stage snapshot
          </button>
          {!showStage && <button type="button" onClick={() => refresh.current()}>Refresh</button>}
          <button type="button" ref={closeButton} onClick={onClose}>Close</button>
        </div>
      </header>
      <div className="cue-monitor-sheet" tabIndex={0} aria-label={showStage ? "Stage confidence" : "Workspace playback status"}>
        {showStage ? <StageConfidence workspaceId={workspaceId} /> : <>
          {notice && <p className="cue-monitor-notice" role="status">{notice}</p>}
          {snapshot && <CueMonitorContents status={snapshot} alerts={alerts}
            inspection={onInspect && activeListId ? { listId: activeListId, select: cueId => {
              if (!onInspect(snapshot.workspace_id, activeListId, cueId)) {
                setNotice("This cue is no longer available in the selected list.");
                refresh.current();
              }
            } } : undefined} />}
        </>}
      </div>
    </section>
  );
}
