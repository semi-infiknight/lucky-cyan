import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { OperatorDialog } from "./OperatorDialog";

interface Revision { id: string; saved_at: string; bytes: number; kind: "save" | "timed" }

export function SavedRevisionsDialog({ currentPath, onClose }: { currentPath?: string | null; onClose: () => void }) {
  const [sources, setSources] = useState<string[] | null>(null);
  const [source, setSource] = useState("");
  const [versions, setVersions] = useState<Revision[]>([]);
  const [version, setVersion] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState("");

  useEffect(() => {
    let active = true;
    invoke<string[]>("list_revision_sources").then(items => {
      if (active) {
        setSources(items);
        setSource(items.find(path => path === currentPath) || items[0] || "");
      }
    }).catch(e => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, [currentPath]);

  useEffect(() => {
    let active = true;
    if (source) invoke<Revision[]>("list_saved_revisions", { source }).then(items => {
      if (active) { setVersions(items); setVersion(items[0]?.id || ""); }
    }).catch(e => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, [source]);

  async function restore() {
    setBusy(true);
    setError("");
    setResult("");
    try {
      const destination = await save({
        defaultPath: source.replace(/\.inkue$/i, "") + " restored.inkue",
        filters: [{ name: "Inkue workspace", extensions: ["inkue"] }],
      });
      if (!destination) return;
      const path = /\.inkue$/i.test(destination) ? destination : destination + ".inkue";
      await invoke("restore_revision_copy", { source, revision: version, destination: path });
      setResult("Created " + path);
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }

  return <OperatorDialog title="Saved revisions" onClose={onClose}>
    <p>Restore an earlier revision as a separate .inkue file. Use File → Open Workspace when you are ready to switch shows.</p>
    {sources === null && !error && <p role="status">Loading saved workspaces…</p>}
    {sources?.length === 0 && <p>No saved revisions yet. Save a workspace to start its local history.</p>}
    {!!sources?.length && <>
      <label>Workspace <select value={source} disabled={busy} onChange={e => {
        setVersions([]); setVersion(""); setError(""); setResult(""); setSource(e.target.value);
      }}>{sources.map(path => <option key={path} value={path}>{path}</option>)}</select></label>
      <label>Revision <select value={version} disabled={busy || !versions.length}
        onChange={e => { setVersion(e.target.value); setError(""); setResult(""); }}>
        {versions.map((r, i) => <option key={r.id} value={r.id}>{new Date(r.saved_at).toLocaleString()} · {r.kind === "timed" ? "Timed snapshot" : "Save"} · {Math.ceil(r.bytes / 1024)} KiB{i === 0 ? " · latest" : ""}</option>)}
      </select></label>
      <footer><button type="button" className="primary" disabled={busy || !version} onClick={() => void restore()}>Restore to new file…</button></footer>
    </>}
    {result && <p role="status">{result}</p>}
    {error && <p role="alert">{error}</p>}
  </OperatorDialog>;
}
