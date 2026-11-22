import type { DevampCueData } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { Section, Segmented } from "./Field";
import { CueTargetPicker } from "./CueTargetPicker";

export function DevampTab({
  cue,
  onSave,
}: {
  cue: DevampCueData;
  onSave: (p: Partial<DevampCueData>) => void;
}) {
  const allCues = useWorkspaceStore((s) => s.cues);
  const targetIds: string[] = cue.target_cue_ids ?? [];
  const scope = cue.scope ?? "current_slice";
  const wholeCue = scope === "whole_cue";

  return (
    <>
      <Section title="Release scope" hint={wholeCue
        ? "Stop at the saved clip end. Use zero Pre-Wait and Do Not Continue on this Devamp."
        : undefined}>
        <Segmented
          options={[
            { value: "current_slice", label: "Current slice" },
            { value: "whole_cue", label: "Whole cue" },
          ]}
          value={scope}
          onChange={(value) => onSave({ scope: value })}
        />
      </Section>
      <Section
        title="Targets"
        hint={wholeCue ? "Choose one looping File Audio cue without slices or one silent, untrimmed MP4 or MOV set to loop indefinitely. Start it before running the Devamp."
          : "Cues whose current slice loop this GO releases."}
      >
        <CueTargetPicker
          allCues={allCues}
          selfId={cue.id}
          selectedIds={targetIds}
          filterTypes={wholeCue ? ["audio", "video"] : ["audio", "video", "group"]}
          onChange={(ids) => {
            const targets = wholeCue ? ids.slice(-1) : ids;
            const nums = targets
              .map((id) => allCues.find((c) => c.id === id)?.number)
              .filter((n): n is string => n != null);
            onSave({ target_cue_ids: targets, target_cue_numbers: nums });
          }}
        />
        <div style={{ height: 8 }} />
      </Section>

      {!wholeCue && (
        <Section title="After the current pass">
          <Segmented
            options={[
              { value: "continue", label: "Continue", hint: "Play on into the next slice" },
              { value: "stop", label: "Stop", hint: "Stop at the end of the current slice" },
            ]}
            value={cue.stop_at_end ? "stop" : "continue"}
            onChange={(v) => onSave({ stop_at_end: v === "stop" })}
          />
          <div style={{ fontSize: 11, color: "var(--wc-text-faint)", marginTop: -4, marginBottom: 6 }}>
            {cue.stop_at_end
              ? "The target finishes its current pass, then stops at the slice boundary."
              : "The target finishes its current pass, then continues into the next slice."}
          </div>
        </Section>
      )}
    </>
  );
}
