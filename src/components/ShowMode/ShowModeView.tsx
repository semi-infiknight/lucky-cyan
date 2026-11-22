import { cartGrid, cellAt, focusGridCell, gridColumns } from "../CueList/cartGrid";
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { openCartWindow, goCue, setActiveCueList, stopCue } from "../../lib/commands";
import { formatClock } from "../../lib/timeline";
import { cueAccent } from "../../lib/cueColors";
import type { CueSummary } from "../../lib/types";
import { useTimingStore } from "../../stores/timingStore";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { CUE_TYPES } from "../CueList/cueCatalog";
import { nextGoCue, runSheetRows } from "./runSheet";
import "./desk.css";

const typeNames = new Map(CUE_TYPES.map(type => [type.type, type.label]));
const continuation = {
  do_not_continue: "Manual",
  auto_continue: "Auto-continue",
  auto_follow: "Auto-follow",
};

function typeName(cue: CueSummary) {
  return typeNames.get(cue.cue_type) ?? cue.cue_type;
}

function cueName(cue: CueSummary) {
  return cue.name || typeName(cue);
}

function timingText(cue: CueSummary, elapsed: number, remaining?: number | null) {
  if (cue.duration_ms == null) return `+${formatClock(elapsed)}`;
  return `−${formatClock(remaining ?? Math.max(0, cue.duration_ms - elapsed))}`;
}

function ActiveCue({ cue }: { cue: CueSummary }) {
  const timing = useTimingStore(s => s.timings[cue.id]);
  return <li className="desk-active-cue" data-state={cue.state}>
    <span className="desk-active-state">{cue.state === "paused" ? "Paused" : "Running"}</span>
    <div className="desk-active-title desk-cue-accent" style={{ "--cue-color": cueAccent(cue) } as CSSProperties}>
      {cue.number && <b>{cue.number}</b>}
      <span title={cueName(cue)}>{cueName(cue)}</span>
    </div>
    <time title={cue.duration_ms == null ? "Elapsed" : "Remaining"}>
      {timingText(cue, timing?.action_elapsed_ms ?? 0, timing?.remaining_ms)}
    </time>
  </li>;
}

function RunRow({ cue, depth, next, broken }: {
  cue: CueSummary; depth: number; next: boolean; broken: boolean;
}) {
  const status = cue.is_disabled ? "Disarmed" : broken ? "Check cue" : cue.is_loading ? "Loading"
    : cue.state === "completed" ? "Completed" : cue.state === "paused" ? "Paused"
    : cue.state === "running" ? "Running" : next ? "Next GO" : "";
  return <tr data-next={next || undefined} data-state={cue.state}
    data-disabled={cue.is_disabled || undefined} aria-current={next ? "step" : undefined}>
    <td className="desk-row-number"><span aria-hidden="true">{next ? "▶︎" : ""}</span>{cue.number || "—"}</td>
    <th scope="row" style={{ "--depth": Math.min(depth, 8), "--cue-color": cueAccent(cue) } as CSSProperties}>
      <span className="desk-row-name desk-cue-accent">{cueName(cue)}</span>
    </th>
    <td className="desk-row-type">{typeName(cue)}</td>
    <td className={broken ? "desk-issue" : ""}>{status}</td>
    <td className="desk-row-duration">{cue.duration_ms != null ? formatClock(cue.duration_ms) : "—"}</td>
  </tr>;
}

export function ShowModeView({ onGoto, onRefresh }: { onGoto: () => void; onRefresh: () => void }) {
  const cues = useWorkspaceStore(s => s.cues);
  const playhead = useWorkspaceStore(s => s.playheadCueId);
  const lists = useWorkspaceStore(s => s.cueLists);
  const activeId = useWorkspaceStore(s => s.activeCueListId);
  const brokenIds = useWorkspaceStore(s => s.brokenCueIds);
  const activeList = lists.find(list => list.id === activeId);
  const cart = activeList?.mode === "cart";
  const layout = cart ? activeList.cart_layout : null;
  const cartCells = useMemo(() => layout ? cartGrid(cues, layout).cells : cues, [cues, layout]);
  const rows = useMemo(() => runSheetRows(cues), [cues]);
  const active = rows.filter(({ cue }) => cue.state === "running" || cue.state === "paused");
  const next = useMemo(() => nextGoCue(cues, playhead), [cues, playhead]);
  const desk = useRef<HTMLElement>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const [error, setError] = useState("");
  const [switching, setSwitching] = useState(false);

  useEffect(() => { desk.current?.focus({ preventScroll: true }); }, []);

  useEffect(() => {
    scroll.current?.querySelector("[data-next]")?.scrollIntoView({ block: "nearest" });
  }, [next?.cue.id, activeId]);

  async function act(action: () => Promise<unknown>) {
    setError("");
    try { await action(); onRefresh(); }
    catch (cause) { setError(String(cause)); }
  }

  return <main data-cue-hotkeys ref={desk} tabIndex={-1} className="desk-show" aria-label="Show Mode">
    <section className="desk-overview" aria-label="Playback overview">
      <div className="desk-standby">
        <div className="desk-section-label">{cart ? "Cue cart" : "Next GO"}</div>
        {cart ? <h1>{activeList.name}</h1> : next ? <>
          {next.groups.length > 0 && <div className="desk-parentage">{next.groups.map(cueName).join(" › ")}</div>}
          <h1 className="desk-cue-accent" style={{ "--cue-color": cueAccent(next.cue) } as CSSProperties}>
            {next.cue.number && <span className="desk-standby-number">{next.cue.number}</span>}{cueName(next.cue)}
          </h1>
          <div className="desk-standby-details">
            <span>{typeName(next.cue)}</span>
            <span>{continuation[next.cue.continue_mode]}</span>
            {next.cue.pre_wait_ms > 0 && <span>Pre-wait {formatClock(next.cue.pre_wait_ms)}</span>}
            {next.cue.duration_ms != null && <span>{formatClock(next.cue.duration_ms)}</span>}
            {next.cue.output_patch_name && <span>{next.cue.output_patch_name}</span>}
            {(next.cue.is_broken || brokenIds.has(next.cue.id)) && <strong className="desk-issue">Check cue</strong>}
            {next.cue.is_loading && <strong>Loading media</strong>}
          </div>
          {(next.cue.notes || next.cue.memo_text) && <p className="desk-call-notes">{next.cue.notes || next.cue.memo_text}</p>}
        </> : <h1>{cues.length === 0 ? "Empty cue list" : "No cue at playhead"}</h1>}
      </div>
      <div className="desk-active">
        <h2 className="desk-section-label">Active in this list <span>{active.length}</span></h2>
        {active.length ? <ul>{active.map(({cue}) => <ActiveCue key={cue.id} cue={cue} />)}</ul>
          : <p className="desk-idle">Idle</p>}
      </div>
    </section>

    <section className="desk-run-sheet" aria-label={cart ? "Cart pads" : "Run sheet"}>
      <header className="desk-list-heading">
        {lists.length > 1 ? <select aria-label="Active cue list" value={activeId ?? ""} disabled={switching}
          onChange={async e => {
            const selected = e.currentTarget.value;
            setSwitching(true);
            await act(() => setActiveCueList(selected));
            setSwitching(false);
          }}>
          {lists.map(list => <option key={list.id} value={list.id}>{list.name}</option>)}
        </select> : <h2>{activeList?.name || "Cue list"}</h2>}
        <span>{rows.length} {rows.length === 1 ? "cue" : "cues"}</span>
        {cart && !activeList.chain_cart_cues && <button onClick={() => void act(() => openCartWindow(activeList.id))}>Open cart window</button>}
        {!cart && <button onClick={onGoto} disabled={!rows.length}>Go to cue <kbd>G</kbd></button>}
      </header>
      {error && <p role="alert" className="desk-error">{error}</p>}
      <div ref={scroll} className="desk-sheet-scroll">
        {cart ? <div className="desk-cart" style={layout ? { gridTemplateColumns: gridColumns(layout.columns) } : undefined}
          onKeyDown={event => { if (layout) focusGridCell(event, layout.columns); }}>
          {cartCells.map((cue, index) => {
            const cell = layout ? cellAt(index, layout.columns) : null;
            const location = cell ? `Row ${cell.row}, column ${cell.column}` : undefined;
            if (!cue) return <span key={index} role="img" data-cart-index={index} className="cart-empty-cell" aria-label={location + ", empty"} />;
            return <div className="desk-cart-slot" role="group" data-cart-index={index} aria-label={location} key={index}>
          <button className="desk-cart-pad" disabled={cue.is_disabled || cue.is_loading}
            style={{ "--cue-color": cueAccent(cue) } as CSSProperties}
            onClick={() => void act(() => goCue(cue.id))}>
            <small>{typeName(cue)} {cue.is_disabled ? "· Disarmed" : cue.is_loading ? "· Loading" : ""}</small>
            <strong>{cue.number && <span>{cue.number} </span>}{cueName(cue)}</strong>
          </button>
          {(cue.state === "running" || cue.state === "paused") &&
            <button className="desk-cart-stop" aria-label={`Stop ${cueName(cue)}`}
              onClick={() => void act(() => stopCue(cue.id))}>■ Stop</button>}
        </div>; })}</div> : <table className="desk-sheet">
          <thead><tr><th scope="col">Cue</th><th scope="col">Name</th><th scope="col">Type</th>
            <th scope="col">State</th><th scope="col">Duration</th></tr></thead>
          <tbody>{rows.map(({ cue, depth }) => <RunRow key={cue.id} cue={cue} depth={depth}
            next={next?.cue.id === cue.id} broken={cue.is_broken || brokenIds.has(cue.id)} />)}</tbody>
        </table>}
      </div>
    </section>
  </main>;
}
