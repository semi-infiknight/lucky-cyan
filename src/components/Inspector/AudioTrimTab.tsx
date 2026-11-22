import { useState } from "react";
import { useOutputPatchTable } from "../../hooks/useOutputPatchTable";
import type { AudioCueData, AudioOutputTrim } from "../../lib/types";
import { Field, NumberInput, Section } from "./Field";

export function AudioTrimTab({ cue, onSaveMain, onSaveOutputs }: {
  cue: AudioCueData;
  onSaveMain: (db: number) => void;
  onSaveOutputs: (trim: AudioOutputTrim | null, workspaceId: string) => Promise<void>;
}) {
  const { table, error: patchError } = useOutputPatchTable();
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [editVersion, setEditVersion] = useState(0);
  const patch = table?.patches.find(patch => patch.id === (cue.output_patch_id ?? table.default_patch_id));
  const saved = cue.output_trim;
  const compatible = patch?.require_named_device && patch.channels.length > 0
    && patch.channels.every(channel => channel >= 0 && channel < 128)
    && new Set(patch.channels).size === patch.channels.length;
  const matches = !saved || !!patch && saved.patch.id === patch.id && saved.patch.device_id === patch.device_id
    && saved.patch.channels.length === patch.channels.length
    && saved.patch.channels.every((channel, index) => channel === patch.channels[index]);

  const commit = async (trim: AudioOutputTrim | null) => {
    if (!table || saving) return;
    setSaving(true);
    setError("");
    try {
      await onSaveOutputs(trim, table.workspace_id);
    } catch (err) {
      setError(String(err));
      setEditVersion(version => version + 1);
    } finally {
      setSaving(false);
    }
  };

  return <>
    <Section title="Output trim" hint="Live compensation after this cue’s programmed levels and fades.">
      <Field label="Main trim (dB)">
        <NumberInput ariaLabel="Main trim in decibels" value={cue.trim_db ?? 0} min={-60} max={12}
          step={0.1} width={76} onCommit={onSaveMain} />
      </Field>
    </Section>
    <Section title="Per-output trim" hint={compatible && matches ? patch?.name : undefined}>
      {patchError && <p role="alert">Output Patches: {patchError}</p>}
      {!table && !patchError && <p role="status">Loading Output Patches…</p>}
      {table && !matches && <p role="alert">The output assignment changed. Review and clear the old output trims before GO.</p>}
      {table && matches && !compatible && <p>Use an Output Patch that requires its named device and assigns each output to a different device channel.</p>}
      {patch && compatible && matches && <fieldset disabled={saving} style={{ border: 0, padding: 0, margin: 0 }}>
        {patch.channels.map((channel, index) => {
          const label = `${patch.channel_names?.[index] || `Output ${index + 1}`} · ch ${channel + 1}`;
          const db = saved?.levels_db[index] ?? 0;
          return <Field key={`${patch.id}:${patch.device_id}:${channel}:${editVersion}`} label={label}>
            <NumberInput ariaLabel={`${label} trim in decibels`} value={db} min={-60} max={12} step={0.1} width={76}
              onCommit={level => {
                if (level === db) return;
                const levels_db = patch.channels.map((_, i) => i === index ? level : saved?.levels_db[i] ?? 0);
                void commit({ patch: { id: patch.id, device_id: patch.device_id, channels: patch.channels }, levels_db });
              }} />
          </Field>;
        })}
      </fieldset>}
      {saved && table && <button type="button" disabled={saving} onClick={() => void commit(null)}>Clear output trims</button>}
      {error && <p role="alert">{error}</p>}
    </Section>
  </>;
}
