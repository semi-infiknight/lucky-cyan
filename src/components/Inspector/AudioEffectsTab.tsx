import type { AudioCueData, CueEffect } from "../../lib/types";
import { Grid2, NumberInput, Section, inputStyle } from "./Field";
import "./audioEffects.css";

const MAX_EFFECTS = 8;

const defaults = {
  high_pass: { name: "High-pass", effect: { kind: "high_pass", enabled: true, frequency_hz: 80 } },
  low_pass: { name: "Low-pass", effect: { kind: "low_pass", enabled: true, frequency_hz: 8000 } },
  peak: { name: "Peaking EQ", effect: { kind: "peak", enabled: true, frequency_hz: 1000, gain_db: -3, q: 1 } },
} as const;

export function AudioEffectsTab({ cue, onSave }: {
  cue: AudioCueData;
  onSave: (change: Partial<AudioCueData>) => void;
}) {
  return (
    <Section title="Cue audio effects" hint="Effects run from top to bottom. Cutoff and peaking EQ gain edits on enabled effects apply during playback. Changes to the chain take effect on the next start.">
      <OrderedAudioEffects effects={cue.audio_effects ?? []} onChange={audio_effects => onSave({ audio_effects })} />
    </Section>
  );
}

export function OrderedAudioEffects({ effects, onChange: save }: {
  effects: CueEffect[];
  onChange: (effects: CueEffect[]) => void;
}) {
  const replace = (index: number, effect: CueEffect) =>
    save(effects.map((current, i) => i === index ? effect : current));

  function move(index: number, delta: number) {
    const next = [...effects];
    const destination = index + delta;
    [next[index], next[destination]] = [next[destination], next[index]];
    save(next);
  }

  return (
    <>
      <div className="cue-effect-controls">
        <select aria-label="Add audio effect" value="" style={inputStyle} disabled={effects.length >= MAX_EFFECTS}
          onChange={event => {
            const kind = event.currentTarget.value as CueEffect["kind"];
            save([...effects, { ...defaults[kind].effect, id: crypto.randomUUID() }]);
          }}>
          <option value="" disabled>{effects.length >= MAX_EFFECTS ? "Eight effects added" : "Add audio effect…"}</option>
          {Object.entries(defaults).map(([kind, { name }]) => <option key={kind} value={kind}>{name}</option>)}
        </select>
      </div>
      {effects.map((effect, index) => (
        <fieldset key={effect.id} className="cue-effect">
          <legend>{defaults[effect.kind].name}</legend>
          <div className="cue-effect-controls">
            <label><input type="checkbox" checked={effect.enabled}
              onChange={event => replace(index, { ...effect, enabled: event.currentTarget.checked })} /> Enabled</label>
            <button type="button" aria-label={`Move effect ${index + 1} up`} disabled={index === 0}
              onClick={() => move(index, -1)}>↑</button>
            <button type="button" aria-label={`Move effect ${index + 1} down`} disabled={index === effects.length - 1}
              onClick={() => move(index, 1)}>↓</button>
            <button type="button" aria-label={`Remove effect ${index + 1}`}
              onClick={() => save(effects.filter((_, i) => i !== index))}>Remove</button>
          </div>
          <Grid2>
            <label>Frequency (Hz)
              <NumberInput value={effect.frequency_hz} step={1} min={20} max={20000}
                onCommit={frequency_hz => replace(index, { ...effect, frequency_hz })} />
            </label>
            {effect.kind === "peak" && <>
              <label>Gain (dB)
                <NumberInput value={effect.gain_db} step={0.1} min={-12} max={12}
                  onCommit={gain_db => replace(index, { ...effect, gain_db })} />
              </label>
              <label>Q
                <NumberInput value={effect.q} step={0.1} min={0.3} max={6}
                  onCommit={q => replace(index, { ...effect, q })} />
              </label>
            </>}
          </Grid2>
        </fieldset>
      ))}
    </>
  );
}
