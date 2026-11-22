import { useMemo, useState } from "react";
import { setLiveCrosspoint } from "../../lib/commands";
import type { CueId } from "../../lib/types";
import { DragNumber } from "../common/DragNumber";
import { CHANNEL_LIMIT, INPUT_BANK_SIZE, OUTPUT_BANK_SIZE, MAX_DB, SILENT_DB, parseCrosspoint, routedBanks, stereoMatrix, withCrosspoint, withSourceChannelName } from "./levelMatrix";
import "./inspector.css";

interface Props {
  cueId: CueId;
  matrix: number[][] | null;
  patchName: string;
  deviceChannels: number[];
  channelNames?: string[];
  sourceNames?: string[];
  onSaveSourceNames?: (names: string[]) => void;
  onSave: (matrix: number[][] | null) => void;
}

function ChannelBank({ label, width, start, onChange }: {
  label: string; width: number; start: number; onChange: (start: number) => void;
}) {
  return <label className="route-bank">
    <span>{label}</span>
    <select aria-label={label} value={start} onChange={e => onChange(Number(e.target.value))}>
      {Array.from({ length: Math.ceil(CHANNEL_LIMIT / width) }, (_, bank) => (
        <option key={bank} value={width * bank}>{width * bank + 1}–{Math.min((bank + 1) * width, CHANNEL_LIMIT)}</option>
      ))}
    </select>
  </label>;
}

function Crosspoint({ db, label, onPreview, onCommit }: {
  db: number; label: string; onPreview: (db: number) => void; onCommit: (db: number) => void;
}) {
  // Keep partial input (including the initial minus sign) while previewing complete values.
  const [text, setText] = useState<string | null>(null);
  const display = text ?? String(Number(db.toFixed(3)));
  const level = parseCrosspoint(display);
  return <label className="route-cell" data-silent={level === SILENT_DB} data-routed={level !== undefined && level > SILENT_DB}>
    <span className="inspector-sr-only">{label}, gain in decibels</span>
    <DragNumber step={1} min={SILENT_DB} max={MAX_DB} value={display} title={label}
      onChange={e => {
        setText(e.target.value);
        const next = parseCrosspoint(e.target.value);
        if (next !== undefined) onPreview(next);
      }}
      onBlur={() => {
        if (text === null) return;
        if (level === undefined) onPreview(db);
        else if (level !== db) onCommit(level);
        setText(null);
      }} />
  </label>;
}

export function LevelMatrixGrid({ cueId, matrix, patchName, deviceChannels, channelNames = [], sourceNames = [], onSaveSourceNames, onSave }: Props) {
  const [inputStart, setInputStart] = useState(0);
  const [outputStart, setOutputStart] = useState(0);
  const [liveError, setLiveError] = useState("");
  const banks = useMemo(() => routedBanks(matrix), [matrix]);
  const inputs = Array.from({ length: INPUT_BANK_SIZE }, (_, i) => inputStart + i);
  const outputs = Array.from({ length: OUTPUT_BANK_SIZE }, (_, i) => outputStart + i);
  const preview = (input: number, output: number, db: number) => {
    void setLiveCrosspoint(cueId, input, output, db).catch(error => setLiveError(String(error)));
  };
  return <section className="route-matrix" aria-label="Level matrix">
    <header className="route-heading">
      <div><h3>Level matrix</h3><span>{patchName}</span></div>
      <button type="button" onClick={() => onSave(matrix === null ? stereoMatrix() : null)}>
        {matrix === null ? "Use a matrix" : "Use pan instead"}
      </button>
    </header>
    {onSaveSourceNames && <details className="source-channel-editor">
      <summary>Source channel names</summary>
      <ChannelBank label="Source channels" width={INPUT_BANK_SIZE} start={inputStart} onChange={setInputStart} />
      {inputs.map(input => <label className="source-channel-name" key={`${cueId}-${input}-${sourceNames[input] ?? ""}`}>
        Input {input + 1}
        <input type="text" aria-label={`Name for source channel ${input + 1}`}
          defaultValue={sourceNames[input] ?? ""}
          onKeyDown={e => { if (e.key === "Enter") e.currentTarget.blur(); }}
          onBlur={e => {
            const name = e.currentTarget.value.trim();
            e.currentTarget.value = name;
            if (name !== (sourceNames[input] ?? "")) onSaveSourceNames(withSourceChannelName(sourceNames, input, name));
          }} />
      </label>)}
    </details>}
    {matrix !== null && <>
      <div className="route-banks">
        <ChannelBank label="Inputs" width={INPUT_BANK_SIZE} start={inputStart} onChange={setInputStart} />
        <ChannelBank label="Outputs" width={OUTPUT_BANK_SIZE} start={outputStart} onChange={setOutputStart} />
        {banks.some(bank => bank.inputStart !== inputStart || bank.outputStart !== outputStart) &&
          <label className="route-bank route-bank-jump">
            <span>Find routes</span>
            <select value="" onChange={event => {
              const bank = banks[Number(event.target.value)];
              if (bank) { setInputStart(bank.inputStart); setOutputStart(bank.outputStart); }
            }}>
              <option value="" disabled>Choose a routed bank…</option>
              {banks.map((bank, index) => <option key={`${bank.inputStart}-${bank.outputStart}`} value={index}>
                In {bank.inputStart + 1}–{bank.inputStart + INPUT_BANK_SIZE} → Out {bank.outputStart + 1}–{bank.outputStart + OUTPUT_BANK_SIZE} ({bank.count})
              </option>)}
            </select>
          </label>}
      </div>
      <div className="route-scroll">
        <table aria-label={`Crosspoint gains for ${patchName}`}>
          <thead><tr><th scope="col">Input</th>{outputs.map(output => {
            const channel = deviceChannels.length ? deviceChannels[output] : output;
            const destination = channel === undefined ? "unassigned" : `device channel ${channel + 1}`;
            const label = channelNames[output]?.trim();
            return <th key={output} scope="col" title={`Output ${output + 1}${label ? ` (${label})` : ""} → ${destination}`}>
              Out {output + 1}{label && <small>{label}</small>}<small>{channel === undefined ? "Unassigned" : `HW ${channel + 1}`}</small>
            </th>;
          })}</tr></thead>
          <tbody>{inputs.map(input => (
            <tr key={input}><th scope="row">{input + 1}{(sourceNames[input] || input < 2) && <small>{sourceNames[input] || (input === 0 ? "L" : "R")}</small>}</th>
              {outputs.map(output => <td key={output}>
                <Crosspoint db={matrix[input]?.[output] ?? SILENT_DB}
                  label={`Input ${input + 1}${sourceNames[input] ? ` (${sourceNames[input]})` : ""} to output ${output + 1}`}
                  onPreview={db => preview(input, output, db)}
                  onCommit={db => onSave(withCrosspoint(matrix, input, output, db))} />
              </td>)}
            </tr>
          ))}</tbody>
        </table>
      </div>
      <p className="route-legend">Gain in dB · −∞ mutes a route</p>
    </>}
    {liveError && <p className="inspector-error" role="alert">Live routing update failed. {liveError}</p>}
  </section>;
}
