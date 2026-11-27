// MSC show-control broadcast section shown in Preferences → Network.
//
// Workspace config saved with the show, mirroring QLab's Workspace Settings →
// MIDI → MSC Broadcast tab: an enable checkbox plus destinations (MIDI output
// port + command format + device ID). While enabled, GO sends `MSC GO {cue
// number}` and Stop All / HardStop All / Panic All send `MSC ALL_OFF`, Reset
// All sends `MSC RESET`, to every destination. Delivery respects the global
// MSC output override.

import { useEffect, useState } from "react";
import type { MidiPreferences, MscBroadcastDestination } from "../../lib/types";
import { getPreferences, listMidiOutputPorts, updateMidiPreferences } from "../../lib/commands";
import { Select } from "../common/Select";

const inputStyle: React.CSSProperties = {
  background: "var(--wc-bg-app)",
  border: "1px solid var(--wc-border-strong)",
  borderRadius: 4,
  color: "var(--wc-text)",
  fontSize: 12,
  padding: "4px 8px",
};

const labelStyle: React.CSSProperties = {
  fontSize: 10, fontWeight: 600, color: "var(--wc-text-muted)",
  textTransform: "uppercase", letterSpacing: "0.07em",
  marginBottom: 10, paddingBottom: 5,
  borderBottom: "1px solid var(--wc-border)",
};

const COMMAND_FORMATS: { value: number; label: string }[] = [
  { value: 127, label: "All Types" },
  { value: 1, label: "Lighting (General)" },
  { value: 16, label: "Sound (General)" },
  { value: 48, label: "Video (General)" },
];

const emptyDestination = (ports: string[]): MscBroadcastDestination => ({
  port_name: ports[0] ?? "",
  command_format: 1,
  device_id: 127,
});

export function MscBroadcastSection() {
  const [prefs, setPrefs] = useState<MidiPreferences | null>(null);
  const [ports, setPorts] = useState<string[]>([]);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    getPreferences()
      .then((app) => setPrefs(app.midi ?? { msc_broadcast_enabled: false, msc_broadcast_destinations: [] }))
      .catch((cause) => setError(String(cause)));
    listMidiOutputPorts().then(setPorts).catch((cause) => setError(String(cause)));
  }, []);

  if (!prefs) return null;

  const apply = async (next: MidiPreferences) => {
    setSaving(true);
    setError("");
    try {
      await updateMidiPreferences(next);
      setPrefs(next);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setSaving(false);
    }
  };

  const updateDestination = (index: number, patch: Partial<MscBroadcastDestination>) => {
    const destinations = prefs.msc_broadcast_destinations.map((dest, i) =>
      i === index ? { ...dest, ...patch } : dest,
    );
    void apply({ ...prefs, msc_broadcast_destinations: destinations });
  };

  return (
    <section style={{ marginBottom: 22 }}>
      <div style={labelStyle}>MSC Broadcast</div>
      <label style={{ display: "flex", alignItems: "center", gap: 8, cursor: "pointer", fontSize: 13, color: "var(--wc-text)" }}>
        <input
          type="checkbox"
          checked={prefs.msc_broadcast_enabled}
          disabled={saving}
          onChange={(e) => void apply({ ...prefs, msc_broadcast_enabled: e.target.checked })}
          style={{ width: 14, height: 14, cursor: "pointer" }}
        />
        Send MSC show-control broadcast messages
      </label>
      <div style={{ fontSize: 11, color: "var(--wc-text-faint)", marginTop: 4, lineHeight: 1.5 }}>
        GO sends <code>MSC GO &#123;cue number&#125;</code>; Stop All, HardStop All and Panic All send{" "}
        <code>MSC ALL_OFF</code>; Reset All sends <code>MSC RESET</code>. Honors the MSC output override.
      </div>
      {prefs.msc_broadcast_enabled && (
        <div style={{ marginTop: 8 }}>
          {prefs.msc_broadcast_destinations.map((dest, index) => (
            <div key={index} style={{ display: "flex", gap: 6, alignItems: "center", marginBottom: 6, flexWrap: "wrap" }}>
              <Select
                ariaLabel="MSC broadcast MIDI output"
                value={dest.port_name}
                disabled={saving}
                onChange={(e) => updateDestination(index, { port_name: e.target.value })}
                style={{ ...inputStyle, width: 180 }}
              >
                {ports.length === 0 && <option value={dest.port_name}>{dest.port_name || "No MIDI output ports"}</option>}
                {ports.map((port) => <option key={port} value={port}>{port}</option>)}
                {ports.length > 0 && !ports.includes(dest.port_name) && (
                  <option value={dest.port_name}>{dest.port_name || "(unavailable)"}</option>
                )}
              </Select>
              <Select
                ariaLabel="MSC command format"
                value={String(dest.command_format)}
                disabled={saving}
                onChange={(e) => updateDestination(index, { command_format: Number(e.target.value) })}
                style={{ ...inputStyle, width: 160 }}
              >
                {COMMAND_FORMATS.map((format) => (
                  <option key={format.value} value={format.value}>{format.label} ({format.value})</option>
                ))}
                {!COMMAND_FORMATS.some((format) => format.value === dest.command_format) && (
                  <option value={dest.command_format}>Custom ({dest.command_format})</option>
                )}
              </Select>
              <input
                aria-label="MSC device ID"
                type="number"
                min={0}
                max={127}
                value={dest.device_id}
                disabled={saving}
                onChange={(e) => updateDestination(index, { device_id: Math.max(0, Math.min(127, Number(e.target.value) || 0)) })}
                style={{ ...inputStyle, width: 70 }}
                title="Device ID 0–127; 127 is the all-call address"
              />
              <button
                type="button"
                disabled={saving}
                onClick={() => void apply({
                  ...prefs,
                  msc_broadcast_destinations: prefs.msc_broadcast_destinations.filter((_, i) => i !== index),
                })}
                style={{ ...inputStyle, cursor: "pointer" }}
                title="Remove destination"
              >
                ✕
              </button>
            </div>
          ))}
          <button
            type="button"
            disabled={saving || ports.length === 0}
            onClick={() => void apply({
              ...prefs,
              msc_broadcast_destinations: [...prefs.msc_broadcast_destinations, emptyDestination(ports)],
            })}
            style={{ ...inputStyle, cursor: "pointer", fontSize: 12 }}
          >
            + New destination
          </button>
        </div>
      )}
      {error && <div style={{ color: "var(--wc-error, #c0392b)", fontSize: 11, marginTop: 6 }}>{error}</div>}
    </section>
  );
}
