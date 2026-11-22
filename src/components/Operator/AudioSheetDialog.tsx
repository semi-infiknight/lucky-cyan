import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { previewAudioSheet, type AudioSheet } from "../../lib/commands";
import { OperatorDialog } from "./OperatorDialog";

export function AudioSheetDialog({ onClose, onAccept }: {
  onClose: () => void;
  onAccept: (sheet: AudioSheet, folder: string) => void;
}) {
  const [text, setText] = useState("");
  const [title, setTitle] = useState("Rehearsal");
  const [sheet, setSheet] = useState<AudioSheet | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function preview() {
    setBusy(true);
    setError("");
    try { setSheet(await previewAudioSheet(title, text)); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  async function selectMedia() {
    if (!sheet) return;
    setError("");
    try {
      const folder = await open({ directory: true, title: "Choose the media folder for this sheet" });
      if (typeof folder === "string") onAccept(sheet, folder);
    } catch (e) { setError(String(e)); }
  }

  return (
    <OperatorDialog title="Review audio cue sheet" onClose={onClose}>
      {!sheet ? <>
        <p>Paste a marked script or a JSON proposal from your assistant. Each SOUND line becomes one manual audio cue.</p>
        {!text.trimStart().startsWith("{") && <label>Show title
          <input value={title} onChange={(e) => setTitle(e.target.value)} />
        </label>}
        <label>Script or proposal
          <textarea value={text} onChange={(e) => setText(e.target.value)} rows={7}
            placeholder={"The door swings shut.\n[SOUND | 1 | Rain in alley | fx/rain.wav | -18]"} />
        </label>
        <button className="primary" disabled={busy || !text.trim()} onClick={() => void preview()}>Preview cue list</button>
      </> : <>
        <section><h3>{sheet.title}</h3>
          <p>{sheet.cues.length} manual audio {sheet.cues.length === 1 ? "cue" : "cues"}. Opening this sheet creates a new unsaved show.</p>
          <table><thead><tr><th scope="col">Cue</th><th scope="col">Name / media</th><th scope="col">Level</th></tr></thead>
            <tbody>{sheet.cues.map((cue) => <tr key={cue.number}><td>{cue.number}</td>
              <td>{cue.name}<small>{cue.media}</small></td><td>{cue.level_db} dB</td></tr>)}</tbody>
          </table></section>
        <footer><button onClick={() => setSheet(null)}>Edit sheet</button>
          <button className="primary" onClick={() => void selectMedia()}>Choose media folder and open…</button></footer>
      </>}
      {error && <p role="alert">{error}</p>}
    </OperatorDialog>
  );
}
