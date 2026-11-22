import { useEffect, useState } from "react";
import { getCue } from "../../lib/commands";
import type { CueEffect, FadeCueData } from "../../lib/types";
import { Select } from "../common/Select";
import { Grid2, NumberInput, Section, inputStyle } from "./Field";

type Destination =
  | ({ kind: "cutoff" } & NonNullable<FadeCueData["target_cutoff"]>)
  | ({ kind: "eq_gain" } & NonNullable<FadeCueData["target_eq_gain"]>);

function effectOptionValue(effect: CueEffect): string {
  return `${effect.kind === "peak" ? "eq_gain" : "cutoff"}:${effect.id}`;
}

export function AudioEffectFadeSection({ targetId, destination, onChange }: {
  targetId?: string;
  destination: Destination | null;
  onChange: (destination: Destination | null) => void;
}) {
  const [loaded, setLoaded] = useState<{ id: string; filters: (CueEffect & { slot: number })[] }>();
  const [error, setError] = useState<string>();

  useEffect(() => {
    setLoaded(undefined);
    setError(undefined);
    if (!targetId) return;
    let mounted = true;
    void getCue(targetId).then(target => {
      if (mounted) {
        const filters = ("audio_effects" in target ? target.audio_effects ?? [] : [])
          .map((effect, index) => ({ ...effect, slot: index + 1 }))
          .filter(effect => effect.enabled);
        setLoaded({ id: targetId, filters });
      }
    }).catch(() => {
      if (mounted) setError("Could not read the target cue’s filters.");
    });
    return () => { mounted = false; };
  }, [targetId]);

  const filters = loaded?.id === targetId ? loaded?.filters ?? [] : [];
  const selectedValue = destination ? `${destination.kind}:${destination.filter_id}` : "";
  const selected = filters.find(filter => effectOptionValue(filter) === selectedValue);
  const missing = !!destination && !!loaded && !selected;

  return (
    <Section title="Audio effect" hint="Fade the cutoff of a high-pass or low-pass filter, or the gain of a peaking EQ, on one Audio cue.">
      <Grid2>
        <label>
          Effect
          <Select aria-label="Audio effect" style={inputStyle} value={selectedValue}
            onChange={event => {
              const filter = filters.find(effect => effectOptionValue(effect) === event.target.value);
              onChange(!filter ? null : filter.kind === "peak"
                ? { kind: "eq_gain", filter_id: filter.id, gain_db: filter.gain_db }
                : { kind: "cutoff", filter_id: filter.id, frequency_hz: filter.frequency_hz });
            }}>
            <option value="">None</option>
            {filters.map(filter => (
              <option key={filter.id} value={effectOptionValue(filter)}>
                {filter.slot}. {filter.kind === "peak" ? "Peaking EQ" : filter.kind === "high_pass" ? "High-pass" : "Low-pass"} · {filter.frequency_hz} Hz
              </option>
            ))}
            {destination && !selected && <option value={selectedValue}>Assigned effect</option>}
          </Select>
        </label>
        {destination?.kind === "cutoff" && (
          <label>Destination (Hz)
            <NumberInput value={destination.frequency_hz} min={20} max={20000} step={1}
              onCommit={frequency_hz => onChange({ ...destination, frequency_hz })} />
          </label>
        )}
        {destination?.kind === "eq_gain" && (
          <label>Destination gain (dB)
            <NumberInput value={destination.gain_db} min={-12} max={12} step={0.1}
              onCommit={gain_db => onChange({ ...destination, gain_db })} />
          </label>
        )}
      </Grid2>
      {error && <p role="status">{error}</p>}
      {missing && <p role="status">Select a supported enabled effect to restore this Fade’s target.</p>}
      {loaded && !destination && filters.length === 0 && <p>Add an enabled filter or peaking EQ in the Audio cue’s Audio FX tab.</p>}
    </Section>
  );
}
