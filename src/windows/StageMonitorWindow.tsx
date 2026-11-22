import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { StagePicture } from "../components/Operator/StageConfidence";
import "../components/Operator/cueMonitor.css";
import { readStageMonitor } from "../lib/commands";
import type { StageMonitorInfo } from "../lib/types";
import { followDeskTheme } from "./deskTheme";

export function StageMonitorWindow() {
  const [output, setOutput] = useState<StageMonitorInfo | null>(null);
  const [error, setError] = useState("");
  const name = output?.name;

  useEffect(followDeskTheme, []);
  useEffect(() => {
    if (name) void getCurrentWindow().setTitle(`Stage monitor · ${name}`).catch(() => {});
  }, [name]);

  useEffect(() => {
    let disposed = false;
    let version = 0;
    const listeners: UnlistenFn[] = [];

    async function refresh() {
      const request = ++version;
      try {
        const next = await readStageMonitor();
        if (disposed || request !== version) return;
        setOutput(next);
        if (!next) {
          setError("The original workspace or output closed.");
          await getCurrentWindow().close();
        } else {
          setError("");
        }
      } catch (cause) {
        if (!disposed && request === version) setError(String(cause));
      }
    }

    const registrations = ["workspace-modified", "cue-lists-changed"].map(async event => {
      const remove = await listen(event, refresh);
      if (disposed) remove();
      else listeners.push(remove);
    });
    void Promise.allSettled(registrations).then(() => { if (!disposed) void refresh(); });
    window.addEventListener("focus", refresh);
    return () => {
      disposed = true;
      window.removeEventListener("focus", refresh);
      listeners.forEach(remove => remove());
    };
  }, []);

  return <main className="stage-monitor-window" aria-label="Stage monitor window">
    <header><h1>{name ?? "Stage monitor"}</h1></header>
    {error && <p role="alert">{error}</p>}
    {output ? <section className="stage-confidence" aria-label={`Stage picture for ${output.name}`}>
      <StagePicture key={`${output.workspace_id}:${output.output_id}`}
        workspaceId={output.workspace_id} outputId={output.output_id} autoStart />
    </section> : !error && <p role="status">Opening stage monitor…</p>}
  </main>;
}
