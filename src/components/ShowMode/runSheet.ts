import type { CueSummary } from "../../lib/types";

export interface RunSheetRow {
  cue: CueSummary;
  depth: number;
}

export function runSheetRows(cues: CueSummary[], depth = 0): RunSheetRow[] {
  return cues.flatMap(cue => [
    { cue, depth },
    ...(cue.children ? runSheetRows(cue.children, depth + 1) : []),
  ]);
}

/** GO skips disarmed list rows. Ordered groups expose the next child the native
 * transport will advance to. Random and simultaneous groups stand as a unit. */
export function nextGoCue(cues: CueSummary[], playhead: string | null) {
  const index = cues.findIndex(cue => cue.id === playhead);
  if (index < 0) return null;
  const first = cues.slice(index).find(item => !item.is_disabled);
  if (!first) return null;
  let cue: CueSummary = first;
  const groups: CueSummary[] = [];
  while (cue.cue_type === "group" &&
    (cue.group_mode === "sequential" || cue.group_mode === "playlist" || cue.group_mode === "start_first_and_enter")) {
    const nextChild = cue.active_child_id;
    const child: CueSummary | undefined = cue.children?.find(item => item.id === nextChild);
    if (!child || child.is_disabled) break;
    groups.push(cue);
    cue = child;
  }
  return { cue, groups };
}
