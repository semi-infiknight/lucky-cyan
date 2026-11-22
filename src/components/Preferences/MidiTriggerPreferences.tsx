// MIDI trigger section shown in Preferences → Network.
//
// Machine config, not workspace config: the port name belongs to the hardware
// in front of the operator, so a show carried to another rig keeps its per-cue
// triggers and picks up that rig's input here.

import { useEffect, useState } from "react";
import type { MidiTriggerConfig } from "../../lib/types";
import { getMidiTriggerConfig, setMidiTriggerConfig, listMidiInputPorts } from "../../lib/commands";
import { useTriggerOverride } from "../../hooks/useTriggerOverride";
import { Select } from "../common/Select";
import { WorkspaceMidiBindings } from "./WorkspaceMidiBindings";

const inputStyle: React.CSSProperties = {
  background: "var(--wc-bg-app)",
  border: "1px solid var(--wc-border-strong)",
  borderRadius: 4,
  color: "var(--wc-text)",
  fontSize: 12,
  padding: "4px 8px",
  width: "100%",
};

const labelStyle: React.CSSProperties = {
  fontSize: 10, fontWeight: 600, color: "var(--wc-text-muted)",
  textTransform: "uppercase", letterSpacing: "0.07em",
  marginBottom: 10, paddingBottom: 5,
  borderBottom: "1px solid var(--wc-border)",
};

export function MidiTriggerPreferences() {
  const [config, setConfig] = useState<MidiTriggerConfig>({ enabled: false, port: null, msc_device_id: null });
  const [ports, setPorts] = useState<string[]>([]);
  const [ready, setReady] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [selectedMscId, setSelectedMscId] = useState(0);
  const override = useTriggerOverride("midi");
  const mscEnabled = config.msc_device_id != null;
  const disabled = !ready || saving;

  useEffect(() => {
    getMidiTriggerConfig().then(next => {
      setConfig(next);
      setSelectedMscId(next.msc_device_id ?? 0);
      setReady(true);
    })
      .catch(cause => setError(String(cause)));
    listMidiInputPorts().then(setPorts).catch(cause => setError(String(cause)));
  }, []);

  const apply = async (next: MidiTriggerConfig) => {
    setSaving(true);
    setError("");
    try {
      await setMidiTriggerConfig(next);
      setConfig(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div style={{ marginBottom: 24 }}>
      <div style={labelStyle}>MIDI Input</div>

      <div style={{ marginBottom: 12 }}>
        <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12 }}>
          <input
            type="checkbox"
            checked={config.enabled}
            disabled={disabled}
            onChange={(e) => apply({ ...config, enabled: e.target.checked })}
            style={{ accentColor: "var(--wc-accent)", width: 14, height: 14 }}
          />
          Fire cues from incoming MIDI
        </label>
        {(config.enabled || mscEnabled || override.suspended) && <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, marginTop: 8 }}>
          <input type="checkbox" checked={override.suspended === true}
            disabled={override.suspended === null || override.pending}
            onChange={event => void override.change(event.target.checked)} />
          Suspend incoming MIDI control for this session
        </label>}
        {override.error && <div role="alert" style={{ fontSize: 11 }}>{override.error}</div>}
      </div>

      {(config.enabled || mscEnabled || ports.length > 0) && (
        <div style={{ marginBottom: 12 }}>
          <div style={{ fontSize: 11, color: "var(--wc-text-secondary)", marginBottom: 4 }}>
            Input port
          </div>
          {ports.length > 0 ? (
            <Select
              style={{ ...inputStyle, cursor: "pointer" }}
              value={config.port ?? ""}
              ariaLabel="MIDI input port"
              disabled={disabled}
              onChange={(e) => apply({ ...config, port: e.target.value || null })}
            >
              <option value="" disabled={mscEnabled}>(first available)</option>
              {[...new Set(ports)].map((p) => (
                <option key={p} value={p}>{p}</option>
              ))}
              {config.port && !ports.includes(config.port) && (
                <option value={config.port}>{config.port} (not found)</option>
              )}
            </Select>
          ) : (
            <div style={{ fontSize: 11, color: "var(--wc-text-faint)" }}>
              No MIDI input ports detected. Connect a controller or install a
              virtual MIDI driver, then reopen Preferences.
            </div>
          )}
        </div>
      )}

      <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12 }}>
        <input type="checkbox" checked={mscEnabled} disabled={disabled || !config.port}
          onChange={event => void apply({ ...config, msc_device_id: event.target.checked ? selectedMscId : null })} />
        Receive MIDI Show Control
      </label>
      {!config.port && <p style={{ fontSize: 11, color: "var(--wc-text-secondary)" }}>
        Choose a named input port to receive Show Control.
      </p>}
      {config.port && <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, marginTop: 8 }}>
        Show Control device ID
        <select value={config.msc_device_id ?? selectedMscId} disabled={disabled} style={{ ...inputStyle, width: "auto" }}
          onChange={event => {
            const id = Number(event.target.value);
            setSelectedMscId(id);
            if (mscEnabled) void apply({ ...config, msc_device_id: id });
          }}>
          {Array.from({ length: 127 }, (_, id) => <option key={id} value={id}>{id}</option>)}
        </select>
      </label>}
      {error && <p role="alert" style={{ fontSize: 11 }}>{error}</p>}
      <WorkspaceMidiBindings voiceEnabled={config.enabled} suspended={override.suspended === true} />
    </div>
  );
}
