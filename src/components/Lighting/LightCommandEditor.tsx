import { useMemo, useState } from "react";
import type { FixtureGroup, LightCueData, ParamTarget, PatchedFixture } from "../../lib/types";
import { defaultParameters, previewLightCommands, type LevelUnit } from "./lightCommands";
import "./lightCommands.css";

export function LightCommandEditor({ cue, fixtures, groups, onApply }: {
  cue: LightCueData; fixtures: PatchedFixture[]; groups: FixtureGroup[];
  onApply: (targets: ParamTarget[]) => void;
}) {
  const imported = cue.qlab_light_source;
  const original = typeof imported?.commandText === "string" ? imported.commandText : "";
  const [source, setSource] = useState(original);
  const [parameter, setParameter] = useState("intensity");
  const [unit, setUnit] = useState<LevelUnit>("percent");
  const [review, setReview] = useState(false);
  const preview = useMemo(() => review
    ? previewLightCommands(source, fixtures, groups, parameter, unit) : null,
    [source, fixtures, groups, parameter, unit, review]);
  const byId = useMemo(() => new Map(fixtures.map(f => [f.id, f])), [fixtures]);
  const [applied, setApplied] = useState(false);
  const [showArchive, setShowArchive] = useState(false);
  const changed = () => { setReview(false); setApplied(false); };

  return <details className="light-commands" open={imported ? true : undefined}
    onKeyDown={event => event.stopPropagation()}>
    <summary>{imported ? "Review imported light commands" : "Program with light commands"}</summary>
    {imported && <details onToggle={e => setShowArchive(e.currentTarget.open)}>
      <summary>Original QLab cue data</summary>
      {showArchive && <pre>{JSON.stringify(imported, null, 2)}</pre>}
    </details>}
    {imported?.alwaysCollate && <p>QLab collates earlier cues here. Include those levels in the look before arming.</p>}
    <label>Commands<textarea rows={4} value={source} spellCheck={false}
      placeholder={"Front wash.intensity = 75\n1 - 3.red = 50"}
      onChange={e => { changed(); setSource(e.target.value); }} /></label>
    <div className="light-command-settings">
      <label>Default parameter<select value={parameter} onChange={e => { changed(); setParameter(e.target.value); }}>
        {defaultParameters.map(p => <option key={p} value={p}>{p}</option>)}
      </select></label>
      <label>Numeric levels<select value={unit} onChange={e => { changed(); setUnit(e.target.value as LevelUnit); }}>
        <option value="percent">Percent · 0–100</option><option value="dmx8">8-bit DMX · 0–255</option>
        <option value="dmx16">16-bit DMX · 0–65535</option>
      </select></label>
    </div>
    <p>Use fixture or group labels from the lighting patch. Groups and ranges expand to the fixtures patched now.</p>
    <button type="button" disabled={!source.trim()} onClick={() => { setApplied(false); setReview(true); }}>Preview look</button>
    {preview && (preview.errors.length
      ? <ul role="alert">{preview.errors.map((e, i) => <li key={i}>{e}</li>)}</ul>
      : <div className="light-command-review">
        <p>{preview.targets.length} parameter targets · replaces {cue.targets?.length ?? 0} currently in this cue</p>
        <div className="light-command-table"><table>
          <thead><tr><th>Fixture</th><th>Parameter</th><th>Level</th></tr></thead>
          <tbody>{preview.targets.slice(0, 160).map(t => {
            const fixture = byId.get(t.fixture_id);
            return <tr key={`${t.fixture_id}/${t.param_index}`}><td>{fixture?.label}</td>
              <td>{fixture?.fixture_type.parameters[t.param_index]?.name}</td><td>{Number((t.value * 100).toFixed(2))}%</td></tr>;
          })}</tbody>
        </table></div>
        {preview.targets.length > 160 && <p>Showing the first 160 of {preview.targets.length} targets.</p>}
        <button type="button" onClick={() => { onApply(preview.targets); setApplied(true); setReview(false); }}>
          Replace cue targets
        </button>
      </div>)}
    {applied && <p role="status">Review the cue targets below. {cue.is_disabled && "This cue is disarmed."}</p>}
  </details>;
}
