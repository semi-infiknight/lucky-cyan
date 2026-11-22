// Output Patch management — named device+channel routes for Audio/Video cues.
// Shown in Preferences → Audio. Mirrors InputPatchesPanel; patches live in the
// workspace and are resolved by cues at GO time.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { CueEffect, DeviceInfo, OutputPatch } from "../../lib/types";
import {
  listOutputDevices,
  getOutputPatchTable,
  setOutputPatch,
  setOutputPatchEffects,
  setCueOutputEffects,
  setOutputPatchRequireDevice,
  removeOutputPatch,
  setDefaultOutputPatch,
} from "../../lib/commands";
import { Select } from "../common/Select";
import { OrderedAudioEffects } from "../Inspector/AudioEffectsTab";

const inputStyle: React.CSSProperties = {
  background: "var(--wc-bg-app)",
  border: "1px solid var(--wc-border-strong)",
  borderRadius: 4,
  color: "var(--wc-text)",
  fontSize: 12,
  padding: "4px 8px",
  width: "100%",
};

const btnStyle: React.CSSProperties = {
  padding: "4px 10px",
  background: "var(--wc-bg-surface)",
  border: "1px solid var(--wc-border-strong)",
  borderRadius: 4,
  color: "var(--wc-text-secondary)",
  fontSize: 12,
  cursor: "pointer",
};

interface EditablePatch extends OutputPatch {
  dirty?: boolean;
}

/** 0-based channel array → "1, 2" (1-based for display). */
function channelsToText(channels: number[]): string {
  return channels.map((c) => c + 1).join(", ");
}

/** "1, 2" (1-based) → 0-based channel array, ignoring junk. */
function textToChannels(text: string): number[] {
  return text
    .split(",")
    .map((s) => parseInt(s.trim(), 10))
    .filter((n) => Number.isFinite(n) && n >= 1)
    .map((n) => n - 1);
}

export function OutputPatchesPanel({ backend }: { backend?: string }) {
  const linux = typeof navigator !== "undefined" && /linux/i.test(navigator.platform);
  const [patches, setPatches] = useState<EditablePatch[]>([]);
  const [defaultId, setDefaultId] = useState<string | null>(null);
  const [workspaceId, setWorkspaceId] = useState<string | null>(null);
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [channelText, setChannelText] = useState<Record<string, string>>({});
  const [savingEffects, setSavingEffects] = useState(false);
  const [effectOutputs, setEffectOutputs] = useState<Record<string, number>>({});
  const [patchError, setPatchError] = useState<{ id: string; message: string }>();

  const reload = () =>
    getOutputPatchTable()
      .then((t) => {
        setPatches(t.patches);
        setDefaultId(t.default_patch_id);
        setWorkspaceId(t.workspace_id);
      })
      .catch(console.error);

  useEffect(() => {
    const refresh = () => {
      reload();
      listOutputDevices(backend).then(setDevices).catch(console.error);
    };
    refresh();
    // The preferences window persists hidden between opens, and the device
    // universe changes with the audio backend (ASIO vs shared) — refetch on
    // every focus AND when the backend/device is applied (device-changed).
    window.addEventListener("focus", refresh);
    const unlisten = listen("device-changed", refresh);
    return () => {
      window.removeEventListener("focus", refresh);
      void unlisten.then((u) => u());
    };
    // Re-runs when the Backend dropdown changes, so the patch device list
    // follows the selection instead of waiting for Apply.
  }, [backend]);

  const handleAdd = async () => {
    const device = devices[0]?.id ?? "";
    try {
      await setOutputPatch(null, "New Output", device, [0, 1]);
      await reload();
    } catch (e) { console.error(e); }
  };

  const handleChange = (id: string, field: keyof OutputPatch, value: string | number[] | string[]) => {
    setPatches((prev) =>
      prev.map((p) => (p.id === id ? { ...p, [field]: value, dirty: true } : p)),
    );
  };

  const commit = async (patch: EditablePatch) => {
    if (!patch.dirty) return;
    try {
      setPatchError(undefined);
      await setOutputPatch(patch.id, patch.name, patch.device_id, patch.channels, patch.channel_names);
      setPatches((prev) => prev.map((p) => (p.id === patch.id ? { ...p, dirty: false } : p)));
    } catch (e) {
      setPatchError({ id: patch.id, message: String(e) });
      await reload();
    }
  };

  /** Set the device and persist immediately (the custom Select has no onBlur). */
  const setDevice = async (patch: EditablePatch, deviceId: string) => {
    try {
      setPatchError(undefined);
      await setOutputPatch(patch.id, patch.name, deviceId, patch.channels, patch.channel_names);
      setPatches((prev) =>
        prev.map((p) => (p.id === patch.id ? { ...p, device_id: deviceId, dirty: false } : p)),
      );
    } catch (e) { setPatchError({ id: patch.id, message: String(e) }); }
  };

  const handleRemove = async (id: string) => {
    try {
      setPatchError(undefined);
      await removeOutputPatch(id);
      await reload();
    } catch (e) { setPatchError({ id, message: String(e) }); }
  };

  const handleSetDefault = async (id: string) => {
    try {
      await setDefaultOutputPatch(id);
      setDefaultId(id);
    } catch (e) { console.error(e); }
  };

  const setRequireDevice = async (id: string, required: boolean) => {
    if (!workspaceId) return;
    try {
      setPatchError(undefined);
      await setOutputPatchRequireDevice(workspaceId, id, required);
    } catch (e) { setPatchError({ id, message: String(e) }); }
    await reload();
  };

  const saveEffects = async (id: string, effects: CueEffect[], output?: number) => {
    if (!workspaceId || savingEffects) return;
    setSavingEffects(true);
    setPatchError(undefined);
    try {
      if (output === undefined) await setOutputPatchEffects(workspaceId, id, effects);
      else await setCueOutputEffects(workspaceId, id, output, effects);
      await reload();
    } catch (e) {
      setPatchError({ id, message: String(e) });
    } finally {
      setSavingEffects(false);
    }
  };

  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", marginBottom: 6 }}>
        <span style={{ fontSize: 11, fontWeight: 600, color: "var(--wc-text-muted)", textTransform: "uppercase", letterSpacing: "0.06em" }}>
          Output Patches
        </span>
        <button style={{ ...btnStyle, marginLeft: "auto" }} onClick={() => void handleAdd()}>
          + Add
        </button>
      </div>

      {patches.length === 0 && (
        <div style={{ fontSize: 11, color: "var(--wc-text-faint)", marginBottom: 4 }}>
          No patches — all audio plays on the main output device, channels 1-2.
        </div>
      )}

      {patches.map((patch) => {
        const deviceMissing = !devices.some((d) => d.id === patch.device_id);
        const device = devices.find(d => d.id === patch.device_id);
        const channelCount = Math.min(128, patch.channels.length || device?.channels || patch.channel_names?.length || 2);
        const effectOutput = Math.max(0, Math.min(effectOutputs[patch.id] ?? 0, patch.channels.length - 1));
        const cueEffects = patch.cue_output_effects?.find(entry => entry.output === effectOutput)?.effects ?? [];
        return (
        <div
          key={patch.id}
          style={{
            display: "grid",
            gridTemplateColumns: "18px 1fr 1.4fr 70px 24px",
            gap: 6, alignItems: "center", marginBottom: 4,
          }}
        >
          <button
            title={defaultId === patch.id ? "Default patch" : "Make default"}
            onClick={() => void handleSetDefault(patch.id)}
            style={{
              background: "transparent", border: "none", cursor: "pointer",
              color: defaultId === patch.id ? "var(--wc-accent)" : "var(--wc-text-faint)",
              fontSize: 13, padding: 0,
            }}
          >
            {defaultId === patch.id ? "★" : "☆"}
          </button>
          <input
            style={inputStyle}
            value={patch.name}
            onChange={(e) => handleChange(patch.id, "name", e.target.value)}
            onBlur={() => void commit(patch)}
          />
          <Select
            style={{
              ...inputStyle,
              cursor: "pointer",
              ...(deviceMissing ? { border: "1px solid #f59e0b", color: "#f59e0b" } : {}),
            }}
            value={patch.device_id}
            onChange={(e) => void setDevice(patch, e.target.value)}
          >
            {deviceMissing && (
              <option value={patch.device_id}>⚠ unavailable — {patch.device_id}</option>
            )}
            {devices.map((d) => (
              <option key={d.id} value={d.id}>{d.name}</option>
            ))}
          </Select>
          <input
            style={inputStyle}
            title="Output channels, 1-based (e.g. 1, 2)"
            value={channelText[patch.id] ?? channelsToText(patch.channels)}
            onChange={(e) => {
              setChannelText((prev) => ({ ...prev, [patch.id]: e.target.value }));
              handleChange(patch.id, "channels", textToChannels(e.target.value));
            }}
            onBlur={() => {
              setChannelText((prev) => {
                const { [patch.id]: _removed, ...rest } = prev;
                return rest;
              });
              void commit(patch);
            }}
          />
          <button
            style={{ ...btnStyle, padding: "4px 6px" }}
            title="Remove patch"
            onClick={() => void handleRemove(patch.id)}
          >
            ✕
          </button>
          <label style={{ gridColumn: "2 / -1", display: "flex", gap: 6, alignItems: "center", fontSize: 12 }}>
            <input
              type="checkbox"
              checked={patch.require_named_device ?? false}
              onChange={(event) => void setRequireDevice(patch.id, event.target.checked)}
              aria-label={`Require named device for Audio GO on ${patch.name}`}
            />
            Require named device for Audio GO
          </label>
          {patch.require_named_device && linux && !patch.device_id.startsWith("hw:") && (
            <span role="status" style={{ gridColumn: "2 / -1", color: "#f59e0b", fontSize: 11 }}>
              Start Inkue with <code>INKUE_AUDIO_HOST=alsa</code> to choose a direct ALSA output.
            </span>
          )}
          {patchError?.id === patch.id && (
            <div role="alert" style={{ gridColumn: "2 / -1", color: "#f59e0b", fontSize: 12 }}>
              {patchError.message}
            </div>
          )}
          <details style={{ gridColumn: "2 / -1", fontSize: 12 }}>
            <summary>Output FX</summary>
            <p>Processes Audio, Video and Mic cues on this patch’s named main output. Enable the required-device check and stop playback on this patch before editing.</p>
            <fieldset disabled={savingEffects} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
              <OrderedAudioEffects effects={patch.audio_effects ?? []} onChange={effects => void saveEffects(patch.id, effects)} />
            </fieldset>
          </details>
          {!!patch.channels.length && patch.channels.length <= 32 && (
            <details style={{ gridColumn: "2 / -1", fontSize: 12 }}>
              <summary>Cue Output FX</summary>
              <p>Filters one Audio, Video or Mic cue output before device routing. Stop playback on this patch before editing.</p>
              <fieldset disabled={savingEffects || patch.dirty} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
                <Select aria-label={`Cue output for effects on ${patch.name}`} value={String(effectOutput)}
                  onChange={event => setEffectOutputs({ ...effectOutputs, [patch.id]: Number(event.target.value) })}>
                  {patch.channels.map((channel, index) => (
                    <option key={index} value={index}>
                      {patch.channel_names?.[index] || `Cue output ${index + 1}`} · HW {channel + 1}
                      {patch.cue_output_effects?.some(entry => entry.output === index) ? " · FX" : ""}
                    </option>
                  ))}
                </Select>
                <OrderedAudioEffects effects={cueEffects}
                  onChange={effects => void saveEffects(patch.id, effects, effectOutput)} />
              </fieldset>
            </details>
          )}
          <details style={{ gridColumn: "2 / -1", fontSize: 12 }}>
            <summary>Channel names</summary>
            {Array.from({ length: channelCount }, (_, index) => (
              <label key={index} style={{ display: "grid", gridTemplateColumns: "120px 1fr", alignItems: "center", marginTop: 4 }}>
                Out {index + 1} · HW {(patch.channels[index] ?? index) + 1}
                <input
                  aria-label={`${patch.name} output ${index + 1} name`}
                  style={inputStyle}
                  value={patch.channel_names?.[index] ?? ""}
                  onChange={e => {
                    const names = Array.from({ length: Math.max(index + 1, patch.channel_names?.length ?? 0) },
                      (_, i) => patch.channel_names?.[i] ?? "");
                    names[index] = e.target.value;
                    handleChange(patch.id, "channel_names", names);
                  }}
                  onBlur={() => void commit(patch)}
                  onKeyDown={e => { if (e.key === "Enter") e.currentTarget.blur(); }}
                />
              </label>
            ))}
          </details>
        </div>
        );
      })}

      <span style={{ fontSize: 10, color: "var(--wc-text-faint)" }}>
        ★ = default for cues with no explicit patch. Channels are 1-based on the
        selected device. Cues pick their patch in the inspector's Levels tab.
      </span>
    </div>
  );
}
