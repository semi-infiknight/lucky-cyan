// Inspector tab for MIDI cues — manages the list of MIDI messages to send on GO.

import { useEffect, useState } from "react";
import type { CurveShape, MidiCueData, MidiFade, MidiMessage, MidiMessageType, MscMessage } from "../../lib/types";
import { listMidiOutputPorts, sendMidiTest } from "../../lib/commands";
import { Select } from "../common/Select";
import { DragNumber } from "../common/DragNumber";
import { CurvePanel } from "../Curve/CurveEditor";

interface Props {
  cue: MidiCueData;
  onSave: (partial: Partial<MidiCueData>) => void;
  copiedCurve?: CurveShape | null;
  onCopyCurve: (curve: CurveShape | null) => void;
}

const inputStyle: React.CSSProperties = {
  background: "var(--wc-bg-app)",
  border: "1px solid var(--wc-border-strong)",
  borderRadius: 4,
  color: "var(--wc-text)",
  fontSize: 12,
  padding: "3px 6px",
};

const selectStyle: React.CSSProperties = { ...inputStyle, cursor: "pointer" };

const btnStyle: React.CSSProperties = {
  padding: "2px 8px",
  background: "var(--wc-bg-surface)",
  border: "1px solid var(--wc-border-strong)",
  borderRadius: 4,
  color: "var(--wc-text-secondary)",
  fontSize: 11,
  cursor: "pointer",
};

const MESSAGE_TYPE_LABELS: Record<MidiMessageType, string> = {
  note_on:        "Note On",
  note_off:       "Note Off",
  control_change: "Control Change",
  program_change: "Program Change",
  key_pressure: "Key Pressure (Aftertouch)",
  channel_pressure: "Channel Pressure",
  pitch_bend: "Pitch Bend",
  msc: "MIDI Show Control",
  sysex: "MIDI SysEx",
};

const mscDefaults: MscMessage = {
  device_id: 0, command_format: 16, command: "go", q_number: "1", q_list: "", q_path: "",
};

const mscCommands = {
  go: ["GO", "cue"],
  stop: ["STOP", "cue"],
  resume: ["RESUME", "cue"],
  load: ["LOAD", "cue"],
  all_off: ["ALL_OFF", "none"],
  restore: ["RESTORE", "none"],
  reset: ["RESET", "none"],
  go_off: ["GO_OFF", "cue"],
  standby_next: ["STANDBY_+", "list"],
  standby_previous: ["STANDBY_−", "list"],
  sequence_next: ["SEQUENCE_+", "list"],
  sequence_previous: ["SEQUENCE_−", "list"],
} as const satisfies Record<MscMessage["command"], [string, "cue" | "list" | "none"]>;

const formatFamilies: [number, number, string][] = [
  [1, 6, "Lighting"], [16, 24, "Sound"], [32, 42, "Machinery"], [48, 56, "Video"],
  [64, 69, "Projection"], [80, 88, "Process control"], [96, 100, "Pyrotechnics"], [127, 127, "All types"],
];

function MscFields({ settings, change }: { settings: MscMessage; change: (patch: Partial<MscMessage>) => void }) {
  const [, layout] = mscCommands[settings.command];
  const fields = [["q_number", "Q number"], ["q_list", "Q list"], ["q_path", "Q path"]] as const;

  function changeCommand(command: MscMessage["command"]) {
    const [, nextLayout] = mscCommands[command];
    if (nextLayout === "none") change({ command, q_number: "", q_list: "", q_path: "" });
    else if (nextLayout === "list") change({ command, q_number: "", q_path: "" });
    else if (!settings.q_number && (command === "load" || settings.q_list)) change({ command, q_number: "1" });
    else change({ command });
  }

  return <div style={{ display: "grid", width: "100%", gridTemplateColumns: "repeat(3, minmax(0,1fr))", gap: 6 }}>
    <label>Device ID
      <select style={{ ...inputStyle, width: "100%" }} aria-label="MSC individual device ID" value={settings.device_id}
        onChange={e => change({ device_id: Number(e.target.value) })}>
        {Array.from({ length: 112 }, (_, id) => <option key={id} value={id}>{id}</option>)}
      </select>
    </label>
    <label>Format
      <select style={{ ...inputStyle, width: "100%" }} aria-label="MSC command format" value={settings.command_format}
        onChange={e => change({ command_format: Number(e.target.value) })}>
        {formatFamilies.flatMap(([first,last,label]) => Array.from({ length: last - first + 1 }, (_, i) =>
          <option key={first+i} value={first+i}>{first+i} · {label}{i === 0 && first !== 127 ? " (general)" : ""}</option>))}
      </select>
    </label>
    <label>Command
      <select style={{ ...inputStyle, width: "100%" }} aria-label="MSC command" value={settings.command}
        onChange={e => changeCommand(e.target.value as MscMessage["command"])}>
        {Object.entries(mscCommands).map(([value, [label]]) => <option key={value} value={value}>{label}</option>)}
      </select>
    </label>
    {fields.filter(([key]) => layout === "cue" || (layout === "list" && key === "q_list")).map(([key, label]) =>
      <label key={key}>{label}
        <input style={{ ...inputStyle, width: "100%" }} aria-label={label} value={settings[key]}
          onChange={e => change({ [key]: e.target.value })} />
      </label>)}
    {layout === "cue" && !settings.q_number && <small style={{ gridColumn: "1 / -1" }}>
      {settings.command === "load" ? "Enter a Q number for LOAD."
        : settings.command === "go" ? "An empty Q number starts the receiver’s next cue."
          : "An empty Q number uses the receiver’s default scope."}
    </small>}
  </div>;
}

function hasData2(type: MidiMessageType): boolean {
  return type !== "program_change" && type !== "channel_pressure" && type !== "pitch_bend";
}

function data1Label(type: MidiMessageType): string {
  if (type === "pitch_bend") return "Bend (0 = center)";
  if (type === "channel_pressure") return "Pressure";
  if (type === "control_change") return "CC#";
  if (type === "program_change") return "Prog";
  return "Note";
}

function canFade(message: MidiMessage): boolean {
  switch (message.message_type) {
    case "control_change": return message.data1 <= 119;
    case "key_pressure":
    case "channel_pressure":
    case "pitch_bend": return true;
    default: return false;
  }
}

function fadeStartValue(message: MidiMessage): number {
  if (message.message_type === "pitch_bend") return message.data2 * 128 + message.data1 - 8192;
  return message.message_type === "channel_pressure" ? message.data1 : message.data2;
}

function fadeEndBytes(message: MidiMessage, endValue: number): [number, number] {
  if (message.message_type === "pitch_bend") {
    const wire = endValue + 8192;
    return [wire & 127, wire >> 7];
  }
  return message.message_type === "channel_pressure" ? [endValue, 0] : [message.data1, endValue];
}

const straightCurve: CurveShape = { kind: "linear", intensity: 0, points: [], bends: [] };
const MAX_CURVE_POINTS = 64;
const curveModes = {
  linear: "Linear", custom: "Custom", s_curve: "S-Curve", exponential: "Exponential", parametric: "Parametric",
};
type CurveMode = keyof typeof curveModes;

function curveMode(curve?: CurveShape): CurveMode {
  return !curve ? "linear" : curve.kind === "linear" ? "custom" : curve.kind;
}

function MidiCurveFields({ curve, descending, copiedCurve, onCopyCurve, onApply }: {
  curve?: CurveShape;
  descending: boolean;
  copiedCurve?: CurveShape | null;
  onCopyCurve: (shape: CurveShape | null) => void;
  onApply: (curve?: CurveShape) => void;
}) {
  const [draft, setDraft] = useState(curve);
  useEffect(() => setDraft(curve), [curve]);
  const mode = curveMode(draft);
  return (
    <details style={{ marginBottom: 6 }}>
      <summary style={{ fontSize: 12, cursor: "pointer" }}>Fade curve · {curveModes[curveMode(curve)]}</summary>
      <div style={{ display: "flex", gap: 8, alignItems: "flex-end", flexWrap: "wrap", marginBlock: 6 }}>
        <label style={{ fontSize: 11, color: "var(--wc-text-muted)" }}>
          Shape
          <Select style={{ ...selectStyle, display: "block" }} value={mode} onChange={(event) => {
            const selected = event.target.value as CurveMode;
            setDraft(selected === "linear" ? undefined : { ...straightCurve, kind: selected === "custom" ? "linear" : selected });
          }}>
            {Object.entries(curveModes).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
          </Select>
        </label>
        {draft?.kind === "parametric" && <label style={{ fontSize: 11, color: "var(--wc-text-muted)" }}>
          Intensity
          <DragNumber style={{ ...inputStyle, display: "block", width: 70 }} min={-10} max={10}
            step={0.25} value={draft.intensity}
            onChange={(e) => setDraft({ ...draft, intensity: Math.max(-10, Math.min(10, Number(e.target.value) || 0)) })} />
        </label>}
      </div>
      <CurvePanel key={mode} shape={draft ?? straightCurve} size={180} label="MIDI value over time"
        disabled={mode === "linear"} descending={descending} pointLimit={MAX_CURVE_POINTS} onChange={setDraft} />
      <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginTop: 6 }}>
        <button type="button" style={btnStyle} disabled={draft === curve} onClick={() => onApply(draft)}>Apply curve</button>
        <button type="button" style={btnStyle} onClick={() => setDraft(undefined)}>Reset to linear</button>
        <button type="button" style={btnStyle} onClick={() => onCopyCurve(draft ?? null)}>Copy curve</button>
        <button type="button" style={btnStyle} disabled={copiedCurve === undefined}
          onClick={() => setDraft(copiedCurve ?? undefined)}>Paste curve</button>
      </div>
    </details>
  );
}

function FadeFields({ fade, pitch, change }: {
  fade: MidiFade;
  pitch: boolean;
  change: (patch: Partial<MidiFade>) => void;
}) {
  const min = pitch ? -8192 : 0;
  const max = pitch ? 8191 : 127;
  return (
    <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 6 }}>
      <label style={{ fontSize: 11, color: "var(--wc-text-muted)" }}>
        End value
        <input
          key={fade.end_value}
          type="number"
          style={{ ...inputStyle, width: 84, display: "block" }}
          defaultValue={fade.end_value}
          min={min}
          max={max}
          onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
          onBlur={(e) => {
            const value = Number.isFinite(e.currentTarget.valueAsNumber)
              ? Math.min(max, Math.max(min, Math.round(e.currentTarget.valueAsNumber)))
              : fade.end_value;
            e.currentTarget.value = String(value);
            if (value !== fade.end_value) change({ end_value: value });
          }}
        />
      </label>
      <label style={{ fontSize: 11, color: "var(--wc-text-muted)" }}>
        Duration (seconds)
        <input
          key={fade.duration_ms}
          type="number"
          style={{ ...inputStyle, width: 96, display: "block" }}
          defaultValue={fade.duration_ms / 1000}
          min={0.001}
          max={86400}
          step={0.001}
          onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
          onBlur={(e) => {
            const ms = Number.isFinite(e.currentTarget.valueAsNumber)
              ? Math.min(86400000, Math.max(1, Math.round(e.currentTarget.valueAsNumber * 1000)))
              : fade.duration_ms;
            e.currentTarget.value = String(ms / 1000);
            if (ms !== fade.duration_ms) change({ duration_ms: ms });
          }}
        />
      </label>
    </div>
  );
}

function defaultMessage(): MidiMessage {
  return {
    port_name: "",
    message_type: "note_on",
    channel: 1,
    data1: 60,
    data2: 100,
  };
}

export function MidiTab({ cue, onSave, copiedCurve, onCopyCurve }: Props) {
  const [ports, setPorts] = useState<string[]>([]);
  const [messages, setMessages] = useState<MidiMessage[]>(cue.messages ?? []);
  const [testResults, setTestResults] = useState<Record<number, string>>({});
  const [removedRows, setRemovedRows] = useState(0);

  useEffect(() => {
    listMidiOutputPorts().then(setPorts).catch(console.error);
  }, []);

  // Sync local state when cue changes from outside (e.g. undo)
  useEffect(() => {
    setMessages(cue.messages ?? []);
  }, [cue.id, cue.messages]);

  const commit = (updated: MidiMessage[]) => {
    setMessages(updated);
    onSave({ messages: updated });
  };

  const addMessage = () => commit([...messages, defaultMessage()]);

  const removeMessage = (idx: number) => {
    setRemovedRows(previous => previous + 1);
    commit(messages.filter((_, i) => i !== idx));
  };

  const updateMessage = (idx: number, patch: Partial<MidiMessage>) => {
    const updated = messages.map((m, i) => (i === idx ? { ...m, ...patch } : m));
    commit(updated);
  };

  const testSend = async (idx: number, endpoint: "start" | "end" = "start") => {
    const msg = messages[idx];
    if (!msg) return;
    const [data1, data2] = endpoint === "end" && msg.fade
      ? fadeEndBytes(msg, msg.fade.end_value)
      : [msg.data1, msg.data2];
    try {
      await sendMidiTest(msg.port_name, msg.message_type, msg.channel, data1, data2,
        msg.msc, Boolean(msg.fade), msg.sysex);
      setTestResults((r) => ({ ...r, [idx]: "✓" }));
    } catch (e) {
      setTestResults((r) => ({ ...r, [idx]: "✗ " + String(e) }));
    }
    setTimeout(() => setTestResults((r) => { const next = { ...r }; delete next[idx]; return next; }), 2000);
  };

  return (
    <div>
      {messages.length === 0 && (
        <div style={{ color: "var(--wc-text-faint)", fontSize: 12, marginBottom: 12 }}>
          No messages. Click + Add to configure.
        </div>
      )}

      {messages.map((msg, idx) => (
        <div
          key={idx}
          style={{
            border: "1px solid var(--wc-border)",
            borderRadius: 6,
            padding: 10,
            marginBottom: 8,
            background: "var(--wc-bg-app)",
          }}
        >
          {/* Row 1: Port + Type */}
          <div style={{ display: "flex", gap: 6, marginBottom: 6 }}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ fontSize: 10, color: "var(--wc-text-muted)", marginBottom: 2 }}>Port</div>
              {ports.length > 0 ? (
                <Select
                  style={{ ...selectStyle, width: "100%" }}
                  value={msg.port_name}
                  onChange={(e) => updateMessage(idx, { port_name: e.target.value })}
                >
                  <option value="">Select output port</option>
                  {ports.map((p) => (
                    <option key={p} value={p}>{p}</option>
                  ))}
                  {!ports.includes(msg.port_name) && msg.port_name && (
                    <option value={msg.port_name}>{msg.port_name} (not found)</option>
                  )}
                </Select>
              ) : (
                <input
                  style={{ ...inputStyle, width: "100%" }}
                  placeholder="Port name"
                  value={msg.port_name}
                  onChange={(e) => updateMessage(idx, { port_name: e.target.value })}
                />
              )}
            </div>
            <div>
              <div style={{ fontSize: 10, color: "var(--wc-text-muted)", marginBottom: 2 }}>Type</div>
              <Select
                style={selectStyle}
                value={msg.message_type}
                onChange={(e) => {
                  const type = e.target.value as MidiMessageType;
                  const cleared = { message_type: type, msc: undefined, sysex: undefined, fade: undefined };
                  if (type === "msc") {
                    updateMessage(idx, { ...cleared, port_name: "", msc: { ...mscDefaults } });
                  } else if (type === "sysex") {
                    updateMessage(idx, { ...cleared, port_name: "", sysex: "" });
                  } else {
                    updateMessage(idx, type === "pitch_bend"
                      ? { ...cleared, data1: 0, data2: 64 }
                      : cleared);
                  }
                }}
              >
                {(Object.keys(MESSAGE_TYPE_LABELS) as MidiMessageType[]).map((t) => (
                  <option key={t} value={t}>{MESSAGE_TYPE_LABELS[t]}</option>
                ))}
              </Select>
            </div>
          </div>

          {/* Row 2: Channel + Data1 + Data2 */}
          <div style={{ display: "flex", gap: 6, alignItems: "flex-end", flexWrap: "wrap", marginBottom: 6 }}>
            {msg.message_type === "msc" ? (
              <MscFields settings={msg.msc ?? mscDefaults}
                change={patch => updateMessage(idx, { msc: { ...(msg.msc ?? mscDefaults), ...patch } })} />
            ) : msg.message_type === "sysex" ? (
              <label style={{ display: "block", flexBasis: "100%", fontSize: 11, color: "var(--wc-text-muted)" }}>
                SysEx bytes (hex)
                <textarea style={{ ...inputStyle, display: "block", width: "100%", boxSizing: "border-box", fontFamily: "monospace" }}
                  rows={3} spellCheck={false} value={msg.sysex ?? ""}
                  onChange={(event) => {
                    const sysex = event.target.value;
                    setMessages(previous => previous.map((message, i) => i === idx ? { ...message, sysex } : message));
                  }}
                  onBlur={(event) => {
                    const sysex = event.target.value;
                    if (sysex !== (cue.messages?.[idx]?.sysex ?? "")) updateMessage(idx, { sysex });
                  }} />
                <small>Include both F0 and F7 or omit both. The complete message can use up to 4,096 bytes.</small>
              </label>
            ) : <>
            <div>
              <div style={{ fontSize: 10, color: "var(--wc-text-muted)", marginBottom: 2 }}>Ch</div>
              <DragNumber
                style={{ ...inputStyle, width: 44 }}
                min={1}
                max={16}
                value={msg.channel}
                onChange={(e) => updateMessage(idx, { channel: Math.max(1, Math.min(16, parseInt(e.target.value, 10) || 1)) })}
              />
            </div>
            <div>
              <div style={{ fontSize: 10, color: "var(--wc-text-muted)", marginBottom: 2 }}>
                {msg.fade && (msg.message_type === "pitch_bend" || msg.message_type === "channel_pressure")
                  ? "Start value" : data1Label(msg.message_type)}
              </div>
              {msg.message_type === "pitch_bend" ? (
                <input
                  key={`${cue.id}-${idx}-${msg.data1}-${msg.data2}`}
                  aria-label="Pitch bend"
                  type="number"
                  style={{ ...inputStyle, width: 85 }}
                  min={-8192}
                  max={8191}
                  defaultValue={fadeStartValue(msg)}
                  onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
                  onBlur={(e) => {
                    const bend = Math.max(-8192, Math.min(8191, parseInt(e.currentTarget.value, 10) || 0));
                    e.currentTarget.value = String(bend);
                    const wireValue = bend + 8192;
                    updateMessage(idx, { data1: wireValue & 127, data2: wireValue >> 7 });
                  }}
                />
              ) : (
                <DragNumber
                  style={{ ...inputStyle, width: 52 }}
                  min={0}
                  max={msg.fade && msg.message_type === "control_change" ? 119 : 127}
                  value={msg.data1}
                  onChange={(e) => {
                    const max = msg.fade && msg.message_type === "control_change" ? 119 : 127;
                    updateMessage(idx, { data1: Math.max(0, Math.min(max, parseInt(e.target.value, 10) || 0)) });
                  }}
                />
              )}
            </div>
            {hasData2(msg.message_type) && (
              <div>
                <div style={{ fontSize: 10, color: "var(--wc-text-muted)", marginBottom: 2 }}>
                  {msg.fade ? "Start value" : msg.message_type === "key_pressure" ? "Pressure" : msg.message_type === "control_change" ? "Value" : "Velocity"}
                </div>
                <DragNumber
                  style={{ ...inputStyle, width: 52 }}
                  min={0}
                  max={127}
                  value={msg.data2}
                  onChange={(e) => updateMessage(idx, { data2: Math.max(0, Math.min(127, parseInt(e.target.value, 10) || 0)) })}
                />
              </div>
            )}
            </>}
            <div style={{ marginLeft: "auto", display: "flex", gap: 4, alignItems: "center" }}>
              {testResults[idx] && (
                <span style={{ fontSize: 11, color: testResults[idx].startsWith("✓") ? "#4ade80" : "#f87171" }}>
                  {testResults[idx]}
                </span>
              )}
              <button style={btnStyle} disabled={!msg.port_name.trim() || Boolean(msg.fade && !canFade(msg))
                || (msg.message_type === "sysex" && !msg.sysex?.trim())} onClick={() => testSend(idx)}>
                {msg.fade ? "Test start" : "▶ Test"}
              </button>
              {msg.fade && (
                <button style={btnStyle} disabled={!msg.port_name.trim() || !canFade(msg)} onClick={() => testSend(idx, "end")}>
                  Test end
                </button>
              )}
              <button
                style={{ ...btnStyle, color: "#ef4444" }}
                onClick={() => removeMessage(idx)}
              >
                ✕
              </button>
            </div>
          </div>
          {(canFade(msg) || msg.fade) && (
            <label style={{ display: "inline-flex", gap: 6, alignItems: "center", fontSize: 12, marginBottom: 6 }}>
              <input
                type="checkbox"
                checked={Boolean(msg.fade)}
                onChange={(e) => updateMessage(idx, { fade: e.target.checked
                  ? { duration_ms: 1000, end_value: fadeStartValue(msg) }
                  : undefined })}
              />
              Fade over duration
            </label>
          )}
          {msg.fade && (
            <>
              <FadeFields
                fade={msg.fade}
                pitch={msg.message_type === "pitch_bend"}
                change={(patch) => {
                  if (msg.fade) updateMessage(idx, { fade: { ...msg.fade, ...patch } });
                }}
              />
              <MidiCurveFields key={`${cue.id}:${removedRows}`} curve={msg.fade.curve}
                descending={msg.fade.end_value < fadeStartValue(msg)} copiedCurve={copiedCurve}
                onCopyCurve={onCopyCurve} onApply={(curve) => {
                  if (msg.fade) updateMessage(idx, { fade: { ...msg.fade, curve } });
                }} />
            </>
          )}
        </div>
      ))}

      <button
        style={{ ...btnStyle, color: "#86efac", marginTop: 4 }}
        onClick={addMessage}
      >
        + Add Message
      </button>

      {ports.length === 0 && (
        <div style={{ marginTop: 10, fontSize: 11, color: "var(--wc-text-muted)" }}>
          No MIDI output ports detected. Connect a MIDI device or install a virtual MIDI driver.
        </div>
      )}
    </div>
  );
}
