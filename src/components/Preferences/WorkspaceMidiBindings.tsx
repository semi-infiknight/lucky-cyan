import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { clearMidiLearn, getWorkspaceMidiControls, learnWorkspaceMidiBinding, setWorkspaceMidiControls } from "../../lib/commands";
import type {
  MidiTransportAction, MidiTransportBinding, MidiTrigger, MidiTriggerType, MidiValueMatch,
  MidiWorkspaceControls, WorkspaceMidiSnapshot,
} from "../../lib/types";

const actions: Record<MidiTransportAction, string> = {
  go: "GO", pause_all: "Pause all", resume_all: "Resume all", stop_all: "Stop all", hard_stop: "Hard stop",
  next_standby: "Next standby", previous_standby: "Previous standby",
  next_sequence: "Next sequence", previous_sequence: "Previous sequence",
};
const kinds: Record<MidiTriggerType, string> = {
  note_on: "Note On", note_off: "Note Off", control_change: "Control Change", program_change: "Program Change",
};
const initial: MidiTransportBinding = {
  trigger: { message_type: "control_change", channel: 1, data1: 64, data2: 127 },
  value_match: "exactly",
};

export function WorkspaceMidiBindings({ voiceEnabled, suspended }: { voiceEnabled: boolean; suspended: boolean }) {
  const [snapshot, setSnapshot] = useState<WorkspaceMidiSnapshot>();
  const [action, setAction] = useState<MidiTransportAction>("go");
  const [draft, setDraft] = useState<MidiTransportBinding | null>(null);
  const [learning, setLearning] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const workspaceId = snapshot?.workspace_id;

  useEffect(() => {
    let active = true;
    let version = 0;
    let remove: UnlistenFn | undefined;
    async function refresh() {
      const request = ++version;
      try {
        const next = await getWorkspaceMidiControls();
        if (active && request === version) setSnapshot(next);
      } catch (cause) {
        if (active && request === version) setError(String(cause));
      }
    }
    void listen("cue-lists-changed", refresh).then(unlisten => {
      if (active) remove = unlisten;
      else unlisten();
    }).finally(() => { if (active) void refresh(); }).catch(() => {});
    return () => { active = false; remove?.(); };
  }, []);

  useEffect(() => { setDraft(null); setLearning(false); }, [workspaceId, action]);

  useEffect(() => {
    if (!learning) return;
    if (!suspended || !voiceEnabled || !workspaceId) { setLearning(false); return; }
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const fail = (cause: unknown) => {
      if (active) { setError(String(cause)); setLearning(false); }
    };
    async function poll() {
      try {
        const captured = await learnWorkspaceMidiBinding(workspaceId!);
        if (!active) return;
        if (captured) {
          setDraft(captured);
          setLearning(false);
        } else {
          timer = setTimeout(poll, 120);
        }
      } catch (cause) { fail(cause); }
    }
    void clearMidiLearn().then(() => { if (active) void poll(); }, fail);
    return () => { active = false; clearTimeout(timer); };
  }, [learning, suspended, voiceEnabled, workspaceId]);

  if (!snapshot) return error ? <p role="alert">{error}</p> : null;
  const { controls } = snapshot;
  const existing = controls.bindings[action];
  const binding = draft ?? existing ?? initial;
  const { trigger } = binding;
  const hasValue = trigger.message_type !== "program_change";
  const updateTrigger = (patch: Partial<MidiTrigger>) => setDraft({ ...binding, trigger: { ...trigger, ...patch } });

  async function save(next: MidiWorkspaceControls) {
    if (!snapshot) return;
    setSaving(true);
    setError("");
    try {
      await setWorkspaceMidiControls(snapshot.workspace_id, snapshot.controls, next);
      setSnapshot(current => current?.workspace_id === snapshot.workspace_id ? { ...current, controls: next } : current);
      setDraft(null);
    } catch (cause) {
      setError(String(cause));
      void getWorkspaceMidiControls().then(setSnapshot).catch(() => {});
    } finally {
      setSaving(false);
    }
  }

  function clearBinding() {
    const bindings = { ...controls.bindings };
    delete bindings[action];
    void save({ ...controls, bindings });
  }

  const disableEdit = saving || learning;
  return <fieldset style={{ display: "grid", gap: 8, minWidth: 0, fontSize: 12, margin: "12px 0 0" }}>
    <legend>Workspace MIDI transport</legend>
    <label><input type="checkbox" checked={controls.enabled} disabled={disableEdit}
      onChange={event => void save({ ...controls, enabled: event.target.checked })} /> Enable workspace transport bindings</label>
    <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
      <label>Action{" "}
        <select value={action} disabled={disableEdit} onChange={event => setAction(event.target.value as MidiTransportAction)}>
          {Object.entries(actions).map(([key, name]) => <option key={key} value={key}>
            {name}{controls.bindings[key as MidiTransportAction] ? " · assigned" : ""}
          </option>)}
        </select>
      </label>
      <label>Message{" "}
        <select value={trigger.message_type} disabled={disableEdit} onChange={event => {
          const message_type = event.target.value as MidiTriggerType;
          updateTrigger(message_type === "program_change" ? { message_type, data2: null } : { message_type });
        }}>
          {Object.entries(kinds).map(([key, name]) => <option key={key} value={key}>{name}</option>)}
        </select>
      </label>
      <label>Channel{" "}
        <select value={trigger.channel} disabled={disableEdit} onChange={event => updateTrigger({ channel: Number(event.target.value) })}>
          <option value={0}>Any</option>
          {Array.from({ length: 16 }, (_, index) => <option key={index + 1} value={index + 1}>{index + 1}</option>)}
        </select>
      </label>
      <label>Number{" "}
        <input type="number" min={0} max={127} value={trigger.data1} disabled={disableEdit}
          onChange={event => updateTrigger({ data1: Number(event.target.value) })} />
      </label>
      {hasValue && <label>Value{" "}
        <select disabled={disableEdit} value={trigger.data2 === null ? "any" : binding.value_match}
          onChange={event => {
            const rule = event.target.value;
            setDraft({
              trigger: { ...trigger, data2: rule === "any" ? null : trigger.data2 ?? 64 },
              value_match: rule === "any" ? "exactly" : rule as MidiValueMatch,
            });
          }}>
          <option value="any">Any</option>
          <option value="exactly">Exactly</option>
          <option value="above">Above</option>
          <option value="below">Below</option>
        </select>
      </label>}
      {hasValue && trigger.data2 !== null && <label>Value number{" "}
        <input type="number" min={0} max={127} value={trigger.data2} disabled={disableEdit}
          onChange={event => updateTrigger({ data2: Number(event.target.value) })} />
      </label>}
    </div>
    <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
      <button type="button" disabled={saving || !voiceEnabled || !suspended}
        onClick={() => { setError(""); setLearning(!learning); }}>
        {learning ? "Cancel capture" : "Capture MIDI"}
      </button>
      <button type="button" disabled={disableEdit || (!!existing && !draft)}
        onClick={() => void save({ ...controls, bindings: { ...controls.bindings, [action]: binding } })}>Save binding</button>
      <button type="button" disabled={disableEdit || !existing} onClick={clearBinding}>Clear binding</button>
    </div>
    {learning && <p role="status" style={{ margin: 0 }}>Waiting for a MIDI message…</p>}
    {!voiceEnabled && <p style={{ margin: 0 }}>Enable MIDI voice input to receive transport and cue triggers.</p>}
    {voiceEnabled && !suspended && <p style={{ margin: 0 }}>Suspend incoming MIDI to capture a binding.</p>}
    {error && <p role="alert" style={{ margin: 0 }}>{error}</p>}
  </fieldset>;
}
