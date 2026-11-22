import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";

interface UsageStatus {
  enabled: boolean;
  error: string | null;
  dropped_events: number;
}

export function MediaUsagePreferences() {
  const [status, setStatus] = useState<UsageStatus | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let active = true;
    const refresh = () => invoke<UsageStatus>("media_usage_status")
      .then(value => { if (active) setStatus(value); })
      .catch(error => { if (active) setMessage(String(error)); });
    void refresh();
    const timer = window.setInterval(refresh, 3000);
    return () => { active = false; window.clearInterval(timer); };
  }, []);

  async function act(action: () => Promise<string>) {
    setBusy(true);
    try {
      setMessage(await action());
      setStatus(await invoke<UsageStatus>("media_usage_status"));
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  return <section aria-label="Media usage reports">
    <h3>Media usage reports</h3>
    <label>
      <input type="checkbox" checked={status?.enabled ?? false} disabled={!status || busy}
        onChange={event => {
          const enabled = event.target.checked;
          void act(async () => { await invoke("set_media_usage_enabled", { enabled }); return ""; });
        }} /> Record media usage on this desk
    </label>
    <p>Records file playback starts and releases across shows. CSV reports include local paths and UTC timestamps.</p>
    <button type="button" disabled={!status || busy} onClick={() => void act(async () => {
      const path = await save({ defaultPath: "Inkue media usage.csv", filters: [{name: "CSV", extensions: ["csv"]}] });
      if (!path) return "";
      await invoke("export_media_usage", { path });
      return "Exported media usage report.";
    })}>Export media usage CSV…</button>
    {status?.error && <p role="alert">{status.error}</p>}
    {!!status?.dropped_events && <p role="alert">Report incomplete. {status.dropped_events} events were dropped since launch.</p>}
    {message && <p role="status">{message}</p>}
  </section>;
}
