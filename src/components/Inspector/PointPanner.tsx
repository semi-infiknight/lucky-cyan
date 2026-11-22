import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { deleteAudioMap, listAudioMaps, previewPointMix, saveAudioMap } from "../../lib/commands";
import type { AudioMap, AudioMapLibrary, AudioMapPatch, PointMix, PointPosition } from "../../lib/types";
import { AudioMapEditor, Coordinate, newMapDraft, type MapDraft } from "./AudioMapEditor";
import "./pointPanner.css";

const stereo: PointMix = {
  input: 1, source: { x: 50, y: 50 },
  speakers: [{ output: 1, x: 0, y: 50 }, { output: 2, x: 100, y: 50 }],
};

type Props = {
  saved?: PointMix | null;
  patch: AudioMapPatch | null;
  patchName: string;
  outputLabels: string[];
  onApply: (mix: PointMix, workspaceId?: string) => Promise<void>;
};

function matchesPatch(map: AudioMap, patch: AudioMapPatch | null) {
  return patch && map.patch.id === patch.id && map.patch.device_id === patch.device_id
    && map.patch.channels.length === patch.channels.length
    && map.patch.channels.every((channel, index) => patch.channels[index] === channel);
}

function Editor({ saved, patch, patchName, outputLabels, onApply }: Props) {
  const [draft, setDraft] = useState<PointMix>(saved ?? stereo);
  const [library, setLibrary] = useState<AudioMapLibrary>();
  const [libraryError, setLibraryError] = useState("");
  const [result, setResult] = useState<{ draft: PointMix; width: number; levels?: number[]; error?: string }>();
  const [mapEditor, setMapEditor] = useState<MapDraft | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const outputs = outputLabels.length;

  useEffect(() => {
    let active = true;
    let latest = 0;
    const refresh = () => {
      const request = ++latest;
      void listAudioMaps().then(data => {
        if (active && request === latest) {
          setLibrary(data);
          setLibraryError("");
        }
      }).catch(err => {
        if (active && request === latest) setLibraryError(String(err));
      });
    };
    const listener = listen("audio-maps-changed", refresh);
    void listener.then(() => { if (active) refresh(); }).catch(err => {
      if (active) setLibraryError(String(err));
    });
    refresh();
    return () => {
      active = false;
      void listener.then(unlisten => unlisten()).catch(console.error);
    };
  }, []);

  useEffect(() => {
    let active = true;
    void previewPointMix(draft, outputs).then(
      levels => { if (active) setResult({ draft, width: outputs, levels }); },
      error => { if (active) setResult({ draft, width: outputs, error: String(error) }); },
    );
    return () => { active = false; };
  }, [draft, outputs]);

  const preview = result?.draft === draft && result.width === outputs ? result : undefined;
  const map = draft.map;
  const maps = library?.maps ?? [];
  const latest = maps.find(item => item.id === map?.id);
  const current = latest?.revision === map?.revision && latest;
  const unavailable = Boolean(map && !current);
  const wrongPatch = Boolean(map && !matchesPatch(map, patch));
  const speakers = draft.speakers ?? stereo.speakers ?? [];
  const move = (point: PointPosition) => setDraft(d => ({ ...d, source: point }));
  const select = (map: AudioMap | null) => {
    setDraft(d => ({ ...d, map, speakers: d.speakers ?? stereo.speakers }));
    setConfirmDelete(false);
    setStatus("");
    setError("");
  };
  const editSpeaker = (index: number, update: Partial<typeof speakers[number]>) => {
    setDraft(d => ({ ...d, speakers: speakers.map((speaker, i) => i === index ? { ...speaker, ...update } : speaker) }));
  };
  const addSpeaker = () => {
    const next = outputLabels.findIndex((_, i) => !speakers.some(s => s.output === i + 1)) + 1;
    if (next) setDraft(d => ({ ...d, speakers: [...speakers, { output: next, x: 50, y: 0 }] }));
  };
  const perform = async (action: () => Promise<void>) => {
    setBusy(true);
    setStatus("");
    setError("");
    try {
      await action();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };
  const saveMap = () => {
    if (!mapEditor || !library || !patch) return;
    void perform(async () => {
      const updated = await saveAudioMap(library.workspace_id, mapEditor.previous, patch, mapEditor.name, mapEditor.marks);
      setLibrary(prev => prev?.workspace_id !== library.workspace_id ? prev : {
        ...prev, maps: [...prev.maps.filter(item => item.id !== updated.id), updated],
      });
      select(updated);
      setMapEditor(null);
      setStatus("Map saved. Apply point mix to update this cue.");
    });
  };
  const deleteMap = () => {
    if (!library || !current) return;
    void perform(async () => {
      await deleteAudioMap(library.workspace_id, current);
      setLibrary(prev => prev?.workspace_id !== library.workspace_id ? prev : {
        ...prev, maps: prev.maps.filter(item => item.id !== current.id),
      });
      setConfirmDelete(false);
      setStatus("Map removed from the library. Cues keep their applied mixes.");
    });
  };

  return <div className="point-editor">
    <div>{patchName} · {outputs} logical outputs</div>
    <div className="point-row">
      <label>Audio map <select
        disabled={busy || Boolean(mapEditor) || !library || Boolean(libraryError)}
        value={unavailable ? "snapshot" : map?.id ?? ""}
        onChange={e => select(maps.find(item => item.id === e.target.value) ?? null)}>
        <option value="">Custom speakers on this cue</option>
        {unavailable && <option value="snapshot" disabled>{map?.name} · cue or draft copy</option>}
        {maps.map(item => <option key={item.id} value={item.id}>{item.name}{matchesPatch(item, patch) ? "" : " · different output assignment"}</option>)}
      </select></label>
      {!mapEditor && <>
        <button type="button" disabled={busy || !library || !!libraryError || outputs === 0}
          onClick={() => { setError(""); setStatus(""); setConfirmDelete(false); setMapEditor(newMapDraft(outputs)); }}>New map…</button>
        {current && <>
          <button type="button" disabled={busy || !!libraryError || wrongPatch}
            onClick={() => { setError(""); setStatus(""); setConfirmDelete(false); setMapEditor({ previous: current, name: current.name, marks: current.marks }); }}>Edit map…</button>
          <button type="button" disabled={busy || !!libraryError} onClick={() => setConfirmDelete(true)}>Delete map…</button>
        </>}
      </>}
    </div>
    {libraryError && <p role="alert">Audio map library: {libraryError}</p>}
    {!library && !libraryError && <p role="status">Loading audio maps…</p>}
    {library && unavailable && <p>{latest
      ? "This copy uses an earlier calibration. Select the current map from the list to apply another mix."
      : "This map is absent from the library. The cue keeps its last applied routes."}</p>}
    {wrongPatch && <p>Select the output assignment used to calibrate this map before applying a mix.</p>}
    {confirmDelete && current && <div className="point-row">
      <span>Delete {current.name} from this workspace? Applied cue mixes will be kept.</span>
      <button type="button" disabled={busy} onClick={deleteMap}>Delete map</button>
      <button type="button" disabled={busy} onClick={() => setConfirmDelete(false)}>Cancel</button>
    </div>}
    {mapEditor && <AudioMapEditor draft={mapEditor} outputLabels={mapEditor.marks[0].levels.map((_, i) => outputLabels[i] ?? `Output ${i + 1}`)}
      workspaceId={library?.workspace_id} patch={patch}
      busy={busy} onSave={saveMap} onChange={setMapEditor} onCancel={() => { setMapEditor(null); setError(""); }} />}
    <fieldset className="point-mix-fields" disabled={busy || !!mapEditor || confirmDelete}>
      <legend>Source placement</legend>
      <div className="point-plane" role="group" aria-label="Speaker layout and draft source position"
        onClick={e => {
          if (busy || mapEditor || confirmDelete) return;
          const r = e.currentTarget.getBoundingClientRect();
          const axis = (position: number, length: number) => Math.min(100, Math.max(0, Math.round(100 * position / length)));
          move({ x: axis(e.clientX - r.left, r.width), y: axis(e.clientY - r.top, r.height) });
        }}>
        {map ? map.marks.map((mark, i) => <button className="audio-map-mark" type="button" key={mark.id}
          title={mark.name} aria-label={`Place source at ${mark.name}`}
          onClick={e => { e.stopPropagation(); move(mark.position); }}
          style={{ left: `${mark.position.x}%`, top: `${mark.position.y}%` }}>{i + 1}</button>)
          : speakers.map((s, i) => <span className="point-speaker" key={i} title={`Output ${s.output}`}
            style={{ left: `${s.x}%`, top: `${s.y}%` }}>{s.output}</span>)}
        <span className="point-source" title="Draft source position" style={{ left: `${draft.source.x}%`, top: `${draft.source.y}%` }} />
      </div>
      <div className="point-row">
        <Coordinate label="File input" min={1} max={128} value={draft.input} onChange={input => setDraft({ ...draft, input })} />
        <Coordinate label="Source X" step={0.1} value={draft.source.x} onChange={x => move({ ...draft.source, x })} />
        <Coordinate label="Source Y" step={0.1} value={draft.source.y} onChange={y => move({ ...draft.source, y })} />
        {map && <label>Place at mark <select value="" onChange={e => {
          const mark = map.marks.find(mark => mark.id === e.target.value);
          if (mark) move(mark.position);
        }}>
          <option value="" disabled>Choose a mark…</option>
          {map.marks.map(mark => <option key={mark.id} value={mark.id}>{mark.name}</option>)}
        </select></label>}
      </div>
      {map ? !wrongPatch && <dl className="audio-map-readout">
        {outputLabels.map((label, i) => <div key={i}><dt>{label}</dt><dd>{formatLevel(preview?.levels?.[i])}</dd></div>)}
      </dl> : <>
        {speakers.map((speaker, index) => <fieldset key={index} className="point-row">
          <legend>Speaker {index + 1}</legend>
          <Coordinate label="Output" min={1} max={outputs} value={speaker.output} onChange={output => editSpeaker(index, { output })} />
          <Coordinate label="X" value={speaker.x} onChange={x => editSpeaker(index, { x })} />
          <Coordinate label="Y" value={speaker.y} onChange={y => editSpeaker(index, { y })} />
          <output>{formatLevel(preview?.levels?.[speaker.output - 1])}</output>
          <button type="button" aria-label={`Remove speaker ${index + 1}`} disabled={speakers.length <= 2}
            onClick={() => setDraft(d => ({ ...d, speakers: speakers.filter((_, i) => i !== index) }))}>Remove</button>
        </fieldset>)}
        <button type="button" disabled={speakers.length >= outputs} onClick={addSpeaker}>Add speaker</button>
      </>}
      {preview?.error && !mapEditor && !wrongPatch && <p role="alert">{preview.error}</p>}
      <p>Apply replaces the routes for file input {draft.input}. A running cue picks up the change immediately.</p>
      <button type="button" disabled={!preview?.levels || unavailable || wrongPatch || Boolean(map && libraryError)}
        onClick={() => void perform(async () => {
          await onApply(map ? { ...draft, speakers: undefined } : draft, map ? library?.workspace_id : undefined);
          setStatus("Point mix applied.");
        })}>Apply point mix</button>
    </fieldset>
    {error && <p role="alert">{error}</p>}
    {status && <p role="status">{status}</p>}
  </div>;
}

function formatLevel(db?: number) {
  return db === undefined ? "—" : db <= -60 ? "−∞ dB" : `${db.toFixed(1)} dB`;
}

export function PointPanner(props: Props) {
  const [open, setOpen] = useState(false);
  return <section className="point-panner">
    <button type="button" aria-expanded={open} onClick={() => setOpen(value => !value)}>2D point panner</button>
    {open && <Editor key={JSON.stringify(props.saved ?? null)} {...props} />}
  </section>;
}
