// Command Cue main tab: the cues this one acts on.
//
// The action is fixed by the cue type (a Goto Cue always gotos), so there is
// nothing to choose but the targets — and, for Goto, exactly one of them.

import { COMMAND_CUE_TYPES, type CommandCueType, type StopCueData } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { NumberInput, Section, ToggleRow } from "./Field";
import { CueTargetPicker } from "./CueTargetPicker";

export function CommandTab({
  cue,
  onSave,
}: {
  cue: StopCueData;
  onSave: (p: Partial<StopCueData>) => void;
}) {
  const allCues = useWorkspaceStore((s) => s.cues);
  const targetIds: string[] = cue.target_cue_ids ?? [];
  const meta = COMMAND_CUE_TYPES.find((c) => c.type === (cue.cue_type as CommandCueType));
  const retarget = cue.cue_type === "target";
  const isLoad = cue.cue_type === "load";
  const timedLoad = isLoad && cue.load_time_ms != null;
  const singleTarget = cue.cue_type === "goto" || retarget || timedLoad;

  return (
    <Section title={singleTarget ? "Target" : "Targets"}>
      <div style={{ fontSize: 12, color: "var(--wc-text-secondary)", marginBottom: 10 }}>
        {meta?.hint ?? "Acts on the cues below."}
        {(retarget || cue.cue_type === "goto") && " — only the first target is used."}
      </div>

      {isLoad && <ToggleRow label="Load at a specified time" checked={timedLoad}
        onToggle={enabled => onSave({ load_time_ms: enabled ? 0 : null })}>
        <label>Seconds from the clip start</label>
        <NumberInput value={(cue.load_time_ms ?? 0) / 1000} step={0.001}
          min={-Number.MAX_SAFE_INTEGER / 1000} max={Number.MAX_SAFE_INTEGER / 1000}
          ariaLabel="Load seconds from clip start" onCommit={seconds => onSave({ load_time_ms: Math.round(seconds * 1000) })} />
        <p>Negative values count back from the clip end. Choose one top-level File Audio cue.</p>
      </ToggleRow>}

      <CueTargetPicker
        allCues={timedLoad ? allCues.filter(c => c.cue_type === "audio") : allCues}
        filterTypes={retarget ? ["stop", "devamp", ...COMMAND_CUE_TYPES.map(c => c.type)] : undefined}
        selfId={cue.id}
        selectedIds={targetIds}
        onChange={(ids) => {
          const kept = singleTarget ? ids.slice(-1) : ids;
          const nums = kept
            .map((id) => allCues.find((c) => c.id === id)?.number)
            .filter((n): n is string => n != null);
          onSave({ target_cue_ids: kept, target_cue_numbers: nums });
        }}
      />

      {cue.temporary_target_id && <p>Current temporary target {allCues.find(c => c.id === cue.temporary_target_id)?.number ?? "unavailable"}</p>}
      {retarget && <Section title="Temporary destination">
        <CueTargetPicker allCues={allCues.filter(c => c.number && !targetIds.includes(c.id))}
          selfId={cue.id} selectedIds={cue.replacement_id ? [cue.replacement_id] : []}
          onChange={ids => {
            const next = allCues.find(c => c.id === ids[ids.length - 1]);
            onSave({ replacement_id: next?.id ?? null, replacement_number: next?.number ?? "" });
          }} />
      </Section>}

      {targetIds.length === 0 && (
        <div style={{ fontSize: 11, color: "var(--wc-text-faint)", marginTop: 8 }}>
          No target — this cue will do nothing.
        </div>
      )}
    </Section>
  );
}
