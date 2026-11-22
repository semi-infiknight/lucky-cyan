import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { dmxGetBlackout, dmxSetBlackout, go, hardStopAll, hardStopBlackout, stopAll, pauseCue, resumeCue, openMixerWindow } from "../../lib/commands";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useTransportStore } from "../../stores/transportStore";
import { useTriggerOverride } from "../../hooks/useTriggerOverride";
import { OscMonitor } from "../Osc/OscMonitor";
import { CueMonitor } from "../Operator/CueMonitor";
import { LightingPanel } from "../Lighting/LightingPanel";
import { TcStatusIndicator } from "../Timecode/TcStatusIndicator";
import { BlackoutStatus, HardStopBlackoutButton } from "./BlackoutControls";
import { MasterOutput } from "./MasterOutput";
import { nextGoCue } from "../ShowMode/runSheet";
import "../ShowMode/desk.css";

export function TransportBar({ onRefresh, showMode, onInspectCue }: {
  onRefresh: () => void;
  showMode: boolean;
  onInspectCue: (workspaceId: string, listId: string, cueId: string) => boolean;
}) {
  const cues = useWorkspaceStore(s => s.cues);
  const playhead = useWorkspaceStore(s => s.playheadCueId);
  const oscActivityAt = useTransportStore(s => s.oscActivityAt);
  const [oscActive, setOscActive] = useState(false);
  const [oscMonitorOpen, setOscMonitorOpen] = useState(false);
  const [cueMonitorOpen, setCueMonitorOpen] = useState(false);
  const cueMonitorButton = useRef<HTMLButtonElement>(null);
  const [lightingOpen, setLightingOpen] = useState(false);
  const [error, setError] = useState("");
  const workspaceId = useWorkspaceStore(s => s.workspaceInfo?.instance_id);
  const combinedEnabled = useWorkspaceStore(s => s.generalPrefs.show_hard_stop_blackout);
  const midiOverride = useTriggerOverride("midi");
  const tcOverride = useTriggerOverride("timecode");
  const [blackout, setBlackout] = useState<boolean | null>(null);
  const blackoutRead = useRef(0);
  const readBlackout = useCallback(async () => {
    const read = ++blackoutRead.current;
    try {
      const active = await dmxGetBlackout();
      if (read === blackoutRead.current) setBlackout(active);
    } catch {
      if (read === blackoutRead.current) setBlackout(null);
    }
  }, []);

  useEffect(() => {
    let mounted = true;
    const unlisten = listen("dmx-blackout-changed", () => { if (mounted) void readBlackout(); });
    void unlisten.then(() => { if (mounted) void readBlackout(); }).catch(() => { if (mounted) void readBlackout(); });
    window.addEventListener("focus", readBlackout);
    return () => {
      mounted = false;
      ++blackoutRead.current;
      window.removeEventListener("focus", readBlackout);
      void unlisten.then(fn => fn()).catch(console.error);
    };
  }, [readBlackout]);

  useEffect(() => {
    if (oscActivityAt == null) return;
    setOscActive(true);
    const timer = setTimeout(() => setOscActive(false), 300);
    return () => clearTimeout(timer);
  }, [oscActivityAt]);

  const running = cues.filter(c => c.state === "running");
  const paused = cues.filter(c => c.state === "paused");
  const resume = running.length === 0 && paused.length > 0;
  const next = nextGoCue(cues, playhead)?.cue;

  async function act(action: () => Promise<unknown>) {
    setError("");
    try { await action(); }
    catch (cause) { setError(String(cause)); }
    onRefresh();
  }

  async function changeBlackout(command: () => Promise<void>) {
    await act(async () => {
      try { await command(); }
      finally { await readBlackout(); }
    });
  }

  const setDmxBlackout = (on: boolean) => void changeBlackout(() => dmxSetBlackout(on));

  return <footer className="desk-transport" aria-label="Playback controls">
    <div className="desk-playback-buttons">
      <button className="desk-go" onClick={() => void act(go)}
        title={next ? `GO (Space) · ${next.number ?? ""} ${next.name}` : "GO (Space)"} aria-keyshortcuts="Space">
        <span aria-hidden="true">▶︎</span> GO <kbd>Space</kbd>
      </button>
      <button className="desk-stop" aria-keyshortcuts="Escape"
        onMouseDown={e => {
          if (showMode || e.button !== 0) return;
          document.dispatchEvent(new CustomEvent("inkue:cue-drag-start", {
            detail: { cueType: "stop", startX: e.clientX, startY: e.clientY },
          }));
        }}
        onClick={() => void act(stopAll)}
        title={showMode ? "Stop all cues (Escape)" : "Stop all cues (Escape) · Drag into list to insert a Stop cue"}>
        <span aria-hidden="true">■</span> Stop all
      </button>
      <button className="desk-pause" disabled={!running.length && !paused.length}
        onClick={() => void act(async () => {
          for (const cue of resume ? paused : running) await (resume ? resumeCue : pauseCue)(cue.id);
        })}
        title={resume ? "Resume paused cues in this list" : "Pause running cues in this list"}>
        <span aria-hidden="true">{resume ? "▶︎" : "Ⅱ"}</span> {resume ? "Resume" : "Pause"}
      </button>
      <button className="desk-hardstop" onClick={() => void act(hardStopAll)}
        title="Cut audio and video immediately (Escape twice)">Hard stop</button>
      <HardStopBlackoutButton enabled={Boolean(workspaceId && combinedEnabled)}
        run={() => { if (workspaceId) void changeBlackout(() => hardStopBlackout(workspaceId)); }} />
    </div>
    <div className="desk-transport-readout">
      {error ? <strong className="desk-issue" role="alert">{error}</strong>
        : (running.length || paused.length) ? <>
          <span>{running.length ? "Running" : "Paused"} · this list</span>
          <strong>{[...running, ...paused].map(cue => [cue.number, cue.name].filter(Boolean).join(" ")).join(" · ")}</strong>
        </> : <>
          <span>{next ? "Next GO" : "Playback"}</span>
          <strong>{next ? [next.number, next.name || next.cue_type].filter(Boolean).join(" ") : "Idle"}</strong>
        </>}
    </div>

    <div className="desk-output-tools">
      <BlackoutStatus blackout={blackout} restore={() => setDmxBlackout(false)} />
      {midiOverride.suspended && <div className="desk-trigger-status">
        <strong role="status">MIDI cue triggers suspended</strong>
        <button type="button" disabled={midiOverride.pending}
          onClick={() => void midiOverride.change(false)}>Restore MIDI triggers</button>
        {midiOverride.error && <span role="alert">{midiOverride.error}</span>}
      </div>}
      {tcOverride.suspended && <div className="desk-trigger-status">
        <strong role="status">Timecode cue triggers suspended</strong>
        <button type="button" disabled={tcOverride.pending}
          onClick={() => void tcOverride.change(false)}>Restore timecode triggers</button>
        {tcOverride.error && <span role="alert">{tcOverride.error}</span>}
      </div>}
      <TcStatusIndicator />
      <div className="desk-tool-buttons">
        <button type="button" ref={cueMonitorButton} aria-pressed={cueMonitorOpen}
          aria-controls={cueMonitorOpen ? "cue-monitor" : undefined}
          disabled={!workspaceId} onClick={() => setCueMonitorOpen(v => !v)}>Cue monitor</button>
        <button aria-pressed={oscMonitorOpen} title="OSC monitor" onClick={() => setOscMonitorOpen(v => !v)}>
          <i className="desk-osc-led" data-active={oscActive} />OSC
        </button>
        <button aria-pressed={lightingOpen} title="DMX lighting"
          onClick={() => setLightingOpen(v => !v)}>DMX</button>
        <button title="Open output mixer" onClick={() => void openMixerWindow().catch(e => setError(String(e)))}>Mixer</button>
      </div>
    </div>
    {cueMonitorOpen && workspaceId && <CueMonitor key={workspaceId} workspaceId={workspaceId}
      onClose={() => { setCueMonitorOpen(false); cueMonitorButton.current?.focus(); }}
      onInspect={showMode ? undefined : (owner, listId, cueId) => {
        if (!onInspectCue(owner, listId, cueId)) return false;
        setCueMonitorOpen(false);
        return true;
      }} />}
    {oscMonitorOpen && <OscMonitor onClose={() => setOscMonitorOpen(false)} />}
    {lightingOpen && <LightingPanel workspaceId={workspaceId} blackout={blackout} onBlackoutChange={setDmxBlackout} showMode={showMode} onClose={() => setLightingOpen(false)} />}
    <MasterOutput />
  </footer>;
}
