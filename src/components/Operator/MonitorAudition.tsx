import { useState } from "react";
import type { CueMonitorSnapshot } from "../../lib/types";
import { Select } from "../common/Select";
import { AudioAudition } from "../Editor/AudioAudition";

export function MonitorAudition({ snapshot }: { snapshot: CueMonitorSnapshot }) {
  const [selection, setSelection] = useState("");
  const choices = snapshot.lists.flatMap(list => {
    const shown = [...(list.standby ? [list.standby] : []), ...list.active.map(run => run.cue)];
    const audio = new Map(shown.filter(cue => cue.kind.toLowerCase() === "audio").map(cue => [cue.id, cue]));
    return [...audio.values()].map(cue => ({
      key: `${list.id}:${cue.id}`, cue, listId: list.id, listName: list.name,
    }));
  });
  const target = choices.find(choice => choice.key === selection) ?? choices[0];
  if (!target) return <p className="cue-monitor-notice">No standby or active Audio cues.</p>;

  return <section className="monitor-audition" aria-label="Audition standby or active Audio">
    <Select value={target.key} onChange={event => setSelection(event.target.value)}
      ariaLabel="Audio cue to audition">
      {choices.map(choice => <option value={choice.key} key={choice.key}>
        {choice.listName} · {[choice.cue.number, choice.cue.name || "Audio"].filter(Boolean).join(" ")}
      </option>)}
    </Select>
    <AudioAudition key={`${snapshot.workspace_id}:${target.key}`} cueId={target.cue.id}
      owner={{ workspace_id: snapshot.workspace_id, list_id: target.listId }} />
  </section>;
}
