import { useEffect, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { applyWorkspaceSettings, exportSettingsFile, getSettingsSnapshot, readSettingsFile,
  type SettingsFile, type SettingsSnapshot, type TransferSettings } from "../../lib/commands";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { OperatorDialog } from "./OperatorDialog";

type Key = keyof TransferSettings;
const labels: Record<Key, string> = {
  double_go_protection_ms: "Double-GO protection",
  show_hard_stop_blackout: "Show Hard stop + blackout button",
  default_fade_out_ms: "Default audio stop fade",
  confirm_before_delete: "Confirm before deleting cues",
  auto_scroll_to_playhead: "Scroll to playhead",
  cue_row_height: "Cue row height",
  cue_color_style: "Cue color display",
  theme: "Desk theme",
  auto_renumber_on_reorder: "Renumber cues on future reorders",
};
const keys = Object.keys(labels) as Key[];
const filters = [{ name: "Inkue workspace settings", extensions: ["inkuesettings"] }];

function display(value: TransferSettings[Key], key: Key) {
  if (typeof value === "boolean") return value ? "On" : "Off";
  if (typeof value === "number") return value === 0 && key === "double_go_protection_ms" ? "Off (0 ms)" : `${value} ms`;
  if (value === "light") return "Light (Warm Cream)";
  if (value === "system") return "System (follow OS)";
  return value === "full_row" ? "Full row" : value[0].toUpperCase() + value.slice(1);
}

export function SettingsTransferDialog({ mode, onClose }: { mode: "export" | "import"; onClose: () => void }) {
  const importing = mode === "import";
  const [snapshot, setSnapshot] = useState<SettingsSnapshot | null>(null);
  const [file, setFile] = useState<SettingsFile | null>(null);
  const [checked, setChecked] = useState<Key[]>(importing ? [] : keys);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState("");

  useEffect(() => {
    if (importing) return;
    let active = true;
    getSettingsSnapshot().then(s => { if (active) setSnapshot(s); })
      .catch(e => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, [importing]);

  async function chooseFile() {
    setBusy(true);
    setError("");
    try {
      const path = await open({ title: "Open workspace settings", filters });
      if (typeof path !== "string") return;
      setFile(null);
      setChecked([]);
      const source = await readSettingsFile(path);
      const current = await getSettingsSnapshot();
      setSnapshot(current);
      setFile(source);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  async function retryReview() {
    setError("");
    setBusy(true);
    try { setSnapshot(await getSettingsSnapshot()); setChecked(importing ? [] : keys); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  const values = importing ? file?.settings : snapshot?.values;
  const offered = keys.filter(key => values?.[key] !== undefined);
  const changes = offered.filter(key => !importing || values?.[key] !== snapshot?.values[key]);

  async function confirm() {
    if (!snapshot || !values || !checked.length) return;
    setBusy(true);
    setError("");
    const selected = Object.fromEntries(checked.map(key => [key, values[key]]));
    try {
      if (importing) {
        await applyWorkspaceSettings(snapshot, selected);
        const store = useWorkspaceStore.getState();
        await Promise.all([store.loadGeneralPrefs(), store.loadDisplayPrefs(), store.refreshWorkspaceInfo()]);
        setResult(`Applied ${checked.length} settings to ${snapshot.name}. Save the workspace to keep them.`);
      } else {
        const path = await save({ title: "Export workspace settings", defaultPath: "Desk settings.inkuesettings", filters });
        if (!path) return;
        await exportSettingsFile(path, snapshot, selected);
        setResult(`Saved ${checked.length} settings to ${path}.`);
      }
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  return <OperatorDialog title={importing ? "Import workspace settings" : "Export workspace settings"}
    onClose={() => { if (!busy) onClose(); }}>
    {result ? <><p role="status">{result}</p><footer><button onClick={onClose}>Done</button></footer></> : <>
      <p>{importing ? "Choose an Inkue settings file, then select the changes for this workspace."
        : "Choose the desk settings to include in the file."} Cue programming, media and hardware routing stay with their workspace.</p>
      {importing && <button disabled={busy} onClick={() => void chooseFile()}>{file ? "Choose another file…" : "Choose settings file…"}</button>}
      {snapshot && values && <section>
        {importing && <p>Settings from <strong>{file?.source_workspace}</strong></p>}
        <h3>{importing ? "Destination" : "Workspace"} · {snapshot.name}</h3>
        <p>{snapshot.path ?? "Unsaved workspace"}</p>
        <table className="workspace-settings-table"><thead><tr><th scope="col">Setting</th><th scope="col">Current</th>
          {importing && <th scope="col">From file</th>}</tr></thead>
          <tbody>{offered.map(key => <tr key={key}>
            <td><label><input type="checkbox" checked={checked.includes(key)} disabled={busy || !changes.includes(key)}
              onChange={e => {
                const next = e.currentTarget.checked;
                setChecked(prev => next ? [...prev, key] : prev.filter(k => k !== key));
              }} /> {labels[key]}</label></td>
            <td>{display(snapshot.values[key], key)}</td>
            {importing && <td>{display(values[key]!, key)}</td>}
          </tr>)}</tbody>
        </table>
        {!changes.length && importing && <p>All settings in this file already match.</p>}
      </section>}
      {error && <p role="alert">{error} <button disabled={busy} onClick={() => void retryReview()}>Refresh review</button></p>}
      <footer>
        {changes.length > 0 && <button disabled={busy} onClick={() => setChecked(checked.length === changes.length ? [] : changes)}>
          {checked.length === changes.length ? "Clear selection" : importing ? "Select all changes" : "Select all settings"}</button>}
        <button className="primary" disabled={busy || !values || !checked.length} onClick={() => void confirm()}>
          {importing ? "Apply selected settings" : "Save settings file…"}</button>
      </footer>
    </>}
  </OperatorDialog>;
}
