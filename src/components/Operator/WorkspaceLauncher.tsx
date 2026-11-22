import { useEffect, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getDefaultWorkspaceTemplate, setDefaultWorkspaceTemplate } from "../../lib/commands";
import type { DefaultWorkspaceTemplate } from "../../lib/types";
import { OperatorDialog } from "./OperatorDialog";

export const TEMPLATE_EXTENSION = "inkue-template";
export const templateFilter = { name: "Inkue Workspace Template", extensions: [TEMPLATE_EXTENSION] };

function DefaultTemplate({ onUse }: { onUse: (path: string) => void }) {
  const [selection, setSelection] = useState<DefaultWorkspaceTemplate | null>(null);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    void getDefaultWorkspaceTemplate().then(value => {
      if (live) setSelection(value);
    }).catch(cause => {
      if (live) setError(String(cause));
    }).finally(() => {
      if (live) setBusy(false);
    });
    return () => { live = false; };
  }, []);

  async function changeDefault(clear: boolean) {
    setBusy(true);
    setError("");
    try {
      const chosen = clear ? null : await openDialog({ multiple: false, filters: [templateFilter] });
      if (!clear && typeof chosen !== "string") return;
      setSelection(await setDefaultWorkspaceTemplate(typeof chosen === "string" ? chosen : null));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }

  return <section className="workspace-launcher-template" aria-label="Default workspace template" aria-busy={busy}>
    <h3>Default template</h3>
    {selection ? <p>
      <strong>{selection.name}</strong>
      <small>{selection.path}</small>
      {!selection.available && <small role="status">Template file unavailable</small>}
    </p> : !busy && !error && <p>New Workspace starts blank.</p>}
    {error && <p role="alert">{error}</p>}
    <div className="workspace-launcher-template-actions">
      {selection && <button type="button" disabled={busy || !selection.available}
        onClick={() => onUse(selection.path)}>New from default</button>}
      <button type="button" disabled={busy} onClick={() => void changeDefault(false)}>
        {selection ? "Change default…" : "Set default…"}
      </button>
      {(selection || error) && <button type="button" disabled={busy}
        onClick={() => void changeDefault(true)}>Clear default</button>}
    </div>
  </section>;
}

export function WorkspaceLauncher({
  recentFiles, onOpen, onBrowse, onNewBlank, onNewTemplate, onNewFromDefault, onClose,
}: {
  recentFiles: string[];
  onOpen: (path: string) => void;
  onBrowse: () => void;
  onNewBlank: () => void;
  onNewTemplate: () => void;
  onNewFromDefault: (path: string) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const search = useRef<HTMLInputElement>(null);
  const term = query.trim().toLocaleLowerCase();
  const matches = recentFiles.filter(path => path.toLocaleLowerCase().includes(term));

  return <OperatorDialog title="Workspace Launcher" onClose={onClose}>
    <form onSubmit={event => {
      event.preventDefault();
      if (matches.length === 1) onOpen(matches[0]);
      else search.current?.focus();
    }}>
      <label htmlFor="launcher-search">Find a recent workspace</label>
      <input id="launcher-search" type="search" ref={search} autoFocus autoComplete="off"
        value={query} onChange={event => setQuery(event.target.value)} />
    </form>
    {matches.length ? <ul className="workspace-launcher-recent" aria-label="Recent workspaces">
      {matches.map(path => <li key={path}>
        <button type="button" onClick={() => onOpen(path)}>
          <strong>{path.split(/[\\/]/).pop() || path}</strong>
          <small>{path}</small>
        </button>
      </li>)}
    </ul> : <p role="status">{recentFiles.length ? "No matching workspaces." : "No recent workspaces."}</p>}
    <DefaultTemplate onUse={onNewFromDefault} />
    <footer>
      <button type="button" onClick={onBrowse}>Browse…</button>
      <button type="button" onClick={onNewBlank}>New Blank Workspace</button>
      <button type="button" onClick={onNewTemplate}>New from Template…</button>
    </footer>
  </OperatorDialog>;
}
