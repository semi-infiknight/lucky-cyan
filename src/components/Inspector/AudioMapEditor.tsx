import { useEffect, useState } from "react";
import type { AudioMap, AudioMapPatch, AudioMark } from "../../lib/types";
import { AudioMapOutputMeters } from "./AudioMapOutputMeters";

export function Coordinate({ label, value, onChange, min = 0, max = 100, step = 1 }: {
  label: string;
  value: number;
  min?: number;
  max?: number;
  step?: number;
  onChange: (n: number) => void;
}) {
  const [text, setText] = useState(String(value));
  useEffect(() => setText(String(value)), [value]);
  return <label>{label}<input type="number" min={min} max={max} step={step} value={text}
    onBlur={() => setText(String(value))}
    onChange={e => {
      setText(e.target.value);
      if (Number.isFinite(e.target.valueAsNumber)) onChange(e.target.valueAsNumber);
    }} /></label>;
}

export interface MapDraft {
  previous: AudioMap | null;
  name: string;
  marks: AudioMark[];
}

const SILENCE = -60;

function makeMark(name: string, x: number, outputs: number, activeOutput?: number): AudioMark {
  return {
    id: crypto.randomUUID(), name, position: { x, y: 50 },
    levels: Array.from({ length: outputs }, (_, index) => index === activeOutput ? 0 : SILENCE),
  };
}

export function newMapDraft(outputs: number): MapDraft {
  return {
    previous: null, name: "",
    marks: [
      makeMark("Left", 0, outputs, 0),
      makeMark("Right", 100, outputs, Math.min(1, outputs - 1)),
    ],
  };
}

export function AudioMapEditor({ draft, outputLabels, workspaceId, patch, busy, onChange, onSave, onCancel }: {
  draft: MapDraft;
  outputLabels: string[];
  workspaceId?: string;
  patch: AudioMapPatch | null;
  busy: boolean;
  onChange: (next: MapDraft) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  const [markId, setMarkId] = useState(draft.marks[0]?.id);
  const selected = draft.marks.find(mark => mark.id === markId) ?? draft.marks[0];
  const changeMark = (update: Partial<AudioMark>) => {
    onChange({ ...draft, marks: draft.marks.map(mark => mark.id === selected.id ? { ...mark, ...update } : mark) });
  };
  const add = () => {
    let index = draft.marks.length + 1;
    while (draft.marks.some(mark => mark.name.toLowerCase() === `mark ${index}`)) index++;
    const mark = makeMark(`Mark ${index}`, 50, outputLabels.length);
    onChange({ ...draft, marks: [...draft.marks, mark] });
    setMarkId(mark.id);
  };
  return <fieldset disabled={busy} className="audio-map-editor">
    <legend>{draft.previous ? "Edit audio map" : "New audio map"}</legend>
    <label className="audio-map-name">Map name <input type="text" maxLength={100} value={draft.name}
      onChange={e => onChange({ ...draft, name: e.target.value })} /></label>
    <div className="point-row">
      <label>Calibration mark <select value={selected.id} onChange={e => setMarkId(e.target.value)}>
        {draft.marks.map((mark, index) => <option key={mark.id} value={mark.id}>{mark.name || `Mark ${index + 1}`}</option>)}
      </select></label>
      <button type="button" disabled={draft.marks.length >= 128} onClick={add}>Add mark</button>
      <button type="button" disabled={draft.marks.length <= 2}
        onClick={() => onChange({ ...draft, marks: draft.marks.filter(mark => mark.id !== selected.id) })}>Remove mark</button>
    </div>
    <div className="point-plane" role="group" aria-label="Calibration positions"
      onClick={e => {
        if (busy) return;
        const box = e.currentTarget.getBoundingClientRect();
        const axis = (position: number, length: number) => Math.min(100, Math.max(0, Math.round(100 * position / length)));
        changeMark({ position: { x: axis(e.clientX - box.left, box.width), y: axis(e.clientY - box.top, box.height) } });
      }}>
      {draft.marks.map((mark, index) => <button className="audio-map-mark" type="button" key={mark.id}
        aria-label={mark.name || `Mark ${index + 1}`} aria-pressed={mark.id === selected.id}
        onClick={e => { e.stopPropagation(); setMarkId(mark.id); }}
        style={{ left: `${mark.position.x}%`, top: `${mark.position.y}%` }}>{index + 1}</button>)}
    </div>
    <div className="point-row">
      <label className="audio-map-name">Mark name <input type="text" maxLength={100} value={selected.name}
        onChange={e => changeMark({ name: e.target.value })} /></label>
      <Coordinate key={`${selected.id}-x`} label="Mark X" step={0.1} value={selected.position.x}
        onChange={x => changeMark({ position: { ...selected.position, x } })} />
      <Coordinate key={`${selected.id}-y`} label="Mark Y" step={0.1} value={selected.position.y}
        onChange={y => changeMark({ position: { ...selected.position, y } })} />
    </div>
    <fieldset className="audio-map-calibration">
      <legend>Mix at this mark (dB)</legend>
      {outputLabels.map((label, index) => <Coordinate key={`${selected.id}-${index}`} label={label} min={SILENCE} max={24} step={0.1}
        value={selected.levels[index]}
        onChange={db => changeMark({ levels: selected.levels.map((level, i) => i === index ? db : level) })} />)}
    </fieldset>
    <AudioMapOutputMeters key={`${workspaceId}:${JSON.stringify(patch)}`} workspaceId={workspaceId} patch={patch} outputLabels={outputLabels} />
    <div className="point-row">
      <button type="button" disabled={!draft.name.trim()} onClick={onSave}>Save map</button>
      <button type="button" onClick={onCancel}>Cancel</button>
    </div>
  </fieldset>;
}
