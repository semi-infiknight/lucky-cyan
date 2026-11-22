import { useEffect, useId, useRef, useState } from "react";
import { getCueReviewHistory } from "../../lib/commands";
import type { CueReviewHistory, CueReviewHistoryRow } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import "./cueReviewHistory.css";

const timeFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const reviewChanges = { note: "Note edited", flag_set: "Flag set", flag_cleared: "Flag cleared", name: "Cue renamed" };

function localTime(value: string) {
  const time = new Date(value);
  return Number.isNaN(time.getTime()) ? value : timeFormat.format(time);
}

export function CueReviewHistoryTable({ rows }: { rows: CueReviewHistoryRow[] }) {
  if (rows.length === 0) return <p>No recent cue review edits.</p>;
  return <table className="cue-review-history-table" aria-label="Recorded cue review edits">
    <thead><tr>
      <th scope="col">Time</th>
      <th scope="col">Cue list</th>
      <th scope="col">Cue</th>
      <th scope="col">Change</th>
    </tr></thead>
    <tbody>{rows.map(row => <tr key={row.event_index}>
      <td><time dateTime={row.time}>{localTime(row.time)}</time></td>
      <td>{row.list_name ?? "Removed cue list"}</td>
      <td>{row.cue_number && <b>{row.cue_number} </b>}
        {row.cue_name === null ? "Removed cue" : row.cue_name || "Untitled cue"}
      </td>
      <td>{reviewChanges[row.change ?? "note"]}</td>
    </tr>)}</tbody>
  </table>;
}

export function CueReviewHistoryPanel({ workspaceId, onClose }: {
  workspaceId: string;
  onClose: () => void;
}) {
  const titleId = useId();
  const close = useRef<HTMLButtonElement>(null);
  const [request, setRequest] = useState(0);
  const [history, setHistory] = useState<CueReviewHistory | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  useEffect(() => { close.current?.focus(); }, []);
  useEffect(() => {
    let active = true;
    const ownsRead = () => active && useWorkspaceStore.getState().workspaceInfo?.instance_id === workspaceId;
    setLoading(true);
    setError("");
    void getCueReviewHistory(workspaceId).then(next => {
      if (!ownsRead()) return;
      if (next.workspace_id !== workspaceId) throw new Error("The workspace changed.");
      setHistory(next);
    }).catch(cause => {
      if (ownsRead()) setError(String(cause));
    }).finally(() => {
      if (ownsRead()) setLoading(false);
    });
    return () => { active = false; };
  }, [request, workspaceId]);

  return <section className="cue-review-history" aria-labelledby={titleId} onKeyDown={event => {
    if (event.key !== "Escape") event.stopPropagation();
  }}>
    <header className="cue-review-history-heading">
      <div>
        <h2 id={titleId}>Cue review history</h2>
        {history && <p>{history.workspace_name}</p>}
      </div>
      <div className="cue-review-history-actions">
        <button type="button" disabled={loading} onClick={() => setRequest(value => value + 1)}>Refresh</button>
        <button type="button" ref={close} onClick={onClose}>Close</button>
      </div>
    </header>
    <div className="cue-review-history-body" tabIndex={0} aria-label="Recorded review edits">
      {loading && <p role="status">Reading cue review history…</p>}
      {error && <p role="alert">{error}</p>}
      {history && <>
        <p>Review edits appear here with current cue labels. Open a cue in the Inspector to see its current note and flag.</p>
        <CueReviewHistoryTable rows={history.rows} />
      </>}
    </div>
  </section>;
}
