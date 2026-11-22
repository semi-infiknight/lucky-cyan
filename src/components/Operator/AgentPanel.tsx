import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  agentArm, agentConnect, agentDecide, agentDisconnect, agentReviewConnect, agentReviewDisconnect,
  agentStatus, type AgentStatus,
} from "../../lib/commands";
import { useWorkspaceStore } from "../../stores/workspaceStore";

export function AgentPanel({ performanceLocked }: { performanceLocked: boolean }) {
  const workspaceId = useWorkspaceStore(s => s.workspaceInfo?.instance_id);
  return <WorkspaceAgentPanel key={workspaceId} workspaceId={workspaceId} performanceLocked={performanceLocked} />;
}

function WorkspaceAgentPanel({ workspaceId, performanceLocked }: {
  workspaceId?: string;
  performanceLocked: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [state, setState] = useState<AgentStatus | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const statusRead = useRef(0);
  const acting = useRef(false);
  const locked = performanceLocked || Boolean(state?.performance_locked);

  useLayoutEffect(() => {
    ++statusRead.current;
    if (performanceLocked) {
      setState(previous => previous?.review_file ? { ...previous, review_file: null } : previous);
    }
  }, [performanceLocked]);

  useEffect(() => {
    let active = true;
    let refreshing = false;
    async function refresh() {
      if (acting.current || refreshing) return;
      refreshing = true;
      const read = ++statusRead.current;
      try {
        const next = await agentStatus();
        if (active && read === statusRead.current && ownsWorkspace(workspaceId)) setState(next);
      } catch (e) {
        if (active && read === statusRead.current && ownsWorkspace(workspaceId)) setError(String(e));
      } finally {
        refreshing = false;
      }
    }
    void refresh();
    const timer = window.setInterval(() => void refresh(), open ? 500 : 3000);
    return () => { active = false; window.clearInterval(timer); };
  }, [open, performanceLocked, workspaceId]);

  async function act(operation: () => Promise<AgentStatus | void>) {
    if (acting.current || !ownsWorkspace(workspaceId)) return;
    acting.current = true;
    const read = ++statusRead.current;
    setBusy(true);
    setError("");
    try {
      const next = (await operation()) ?? await agentStatus();
      if (read === statusRead.current && ownsWorkspace(workspaceId)) setState(next);
    } catch (e) {
      if (read === statusRead.current && ownsWorkspace(workspaceId)) setError(String(e));
    } finally {
      acting.current = false;
      if (ownsWorkspace(workspaceId)) setBusy(false);
    }
  }

  function closePanel() {
    if (acting.current) return;
    if (state?.session_file) void act(() => agentArm(false));
    setOpen(false);
  }

  return <div className="agent-access" data-agent-controls>
    <button aria-expanded={open} onClick={() => {
      if (open) closePanel();
      else setOpen(true);
    }}>{performanceLocked ? "Agent · Show Mode lock" : state?.armed ? "Agent · armed" : state?.session_file ? "Agent · connected" : "Agent access"}</button>
    {open && <aside aria-label="Agent access" className="agent-panel">
      <h3>Agent access</h3>
      {performanceLocked && <p>Show Mode locks playback approval. A connected assistant can still inspect the workspace.</p>}
      {!state?.session_file ? <>
        <p>Share this workspace with a local assistant. Requests appear in the show’s programming history.</p>
        <button disabled={busy} onClick={() => void act(agentConnect)}>Start read-only session</button>
      </> : <>
        <label>Session file for your local client<input readOnly value={state.session_file} onFocus={(e) => e.target.select()} /></label>
        {state.monitor_file && <label>Status-only file for a local display<input readOnly value={state.monitor_file} onFocus={(e) => e.target.select()} /></label>}
        <p>{state.armed ? "Armed for one operator-reviewed action for up to 60 seconds." : "Playback requests require arming and your approval here."}</p>
        {!performanceLocked && state.pending ? <div role="status">
          <strong>{state.pending.label}</strong>
          <div className="agent-actions">
            <button disabled={busy} onClick={() => void act(() => agentDecide(state.pending!.id, true))}>Confirm action</button>
            <button disabled={busy} onClick={() => void act(() => agentDecide(state.pending!.id, false))}>Decline</button>
          </div>
        </div> : <button disabled={busy} onClick={() => void act(() => agentArm(!state.armed))}>
          {state.armed ? "Disarm" : "Arm one request"}
        </button>}
        {state.last_result && <p role="status">{state.last_result.label} · {state.last_result.outcome}</p>}
        <button disabled={busy} onClick={() => void act(agentDisconnect)}>Disconnect</button>
      </>}
      <CueReviewAccess
        connected={Boolean(state?.session_file)} locked={locked} workspaceId={workspaceId}
        reviewFile={state?.review_file ?? null} busy={busy}
        onEnable={() => {
          if (workspaceId && state?.session_file && !locked) void act(() => agentReviewConnect(workspaceId));
        }}
        onRevoke={() => {
          if (workspaceId && state?.review_file && !locked) void act(() => agentReviewDisconnect(workspaceId));
        }}
      />
      {error && <p role="alert">{error}</p>}
    </aside>}
  </div>;
}

function ownsWorkspace(workspaceId?: string) {
  return useWorkspaceStore.getState().workspaceInfo?.instance_id === workspaceId;
}

export function CueReviewAccess({ connected, locked, workspaceId, reviewFile, busy, onEnable, onRevoke }: {
  connected: boolean;
  locked: boolean;
  workspaceId?: string;
  reviewFile: string | null;
  busy: boolean;
  onEnable: () => void;
  onRevoke: () => void;
}) {
  return <section className="agent-review" aria-label="Cue review">
    <h4>Cue review</h4>
    {locked ? <p>Cue review is unavailable in Show Mode.</p>
      : !connected ? <p>Start an Agent access session to enable Cue review.</p>
      : !workspaceId ? <p>Open a workspace to enable Cue review.</p>
      : <>
        <p>A reviewer can see cue labels and notes, edit a note if it still matches their starting text, and set or clear review flags. Accepted changes enter the desk’s undo history.</p>
        {reviewFile
          ? <ReviewCredential key={reviewFile} path={reviewFile} busy={busy} onRevoke={onRevoke} />
          : <button type="button" disabled={busy} onClick={onEnable}>Enable cue review</button>}
      </>}
  </section>;
}

function ReviewCredential({ path, busy, onRevoke }: { path: string; busy: boolean; onRevoke: () => void }) {
  const input = useRef<HTMLInputElement>(null);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);

  async function copyPath() {
    setCopied(false);
    setCopyError(false);
    try {
      await navigator.clipboard.writeText(path);
      setCopied(true);
    } catch {
      input.current?.focus();
      input.current?.select();
      setCopyError(true);
    }
  }

  return <>
    <label>Private cue review file<input ref={input} readOnly value={path} onFocus={event => event.currentTarget.select()} /></label>
    <p>Share this path only with the reviewer you want to authorize.</p>
    <div className="agent-review-actions">
      <button type="button" disabled={busy} onClick={() => void copyPath()}>Copy path</button>
      <button type="button" disabled={busy} onClick={onRevoke}>Revoke cue review</button>
    </div>
    {copied && <p role="status">Review file path copied.</p>}
    {copyError && <p role="alert">Clipboard unavailable. Copy the selected path.</p>}
  </>;
}
