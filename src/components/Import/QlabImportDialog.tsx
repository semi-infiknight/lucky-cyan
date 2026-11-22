import { useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import type { ImportReport } from "../../lib/types";
import { OperatorDialog } from "../Operator/OperatorDialog";

export function QlabImportDialog({ report, onClose, onCheck }: {
  report: ImportReport;
  onClose: () => void;
  onCheck: () => void;
}) {
  const [status, setStatus] = useState("");
  const attention = report.cues.filter((cue) => cue.note);
  const outputs = report.video_outputs ?? [];

  async function saveReport() {
    try {
      const path = await save({
        defaultPath: "QLab import report.json",
        filters: [{ name: "Import report", extensions: ["json"] }],
      });
      if (!path) return;
      await writeTextFile(path, JSON.stringify(report, null, 2) + "\n");
      setStatus("Report saved.");
    } catch (error) {
      setStatus(String(error));
    }
  }

  return (
    <OperatorDialog title={`Imported “${report.workspace_name}”`} onClose={onClose}>
      <p>{report.cue_count} cues across {report.cue_list_count} lists · {report.media_found} media references resolved</p>
      <section>
        <h3>Conversion review</h3>
        {attention.length > 0 ? (
          <ul>{attention.map((cue, i) => <li key={i}>
            <strong>{[cue.cue_number, cue.cue_name || cue.qlab_class].filter(Boolean).join(" · ")}</strong>
            {" "}({cue.inkue_type}) — {cue.note}
          </li>)}</ul>
        ) : <p>The importer flagged no cue conversions for review.</p>}
        {report.media_missing.length > 0 && <details open>
          <summary>{report.media_missing.length} missing media references</summary>
          <ul>{report.media_missing.map((path, i) => <li key={i}>{path}</li>)}</ul>
        </details>}
        {!!report.lighting_patch_notes?.length && <section>
          <h3>Lighting patch</h3>
          <ul>{report.lighting_patch_notes.map((note, index) => <li key={index}>{note}</li>)}</ul>
        </section>}
        {!!report.cart_layout_notes?.length && <section>
          <h3>Cart placement</h3>
          <ul>{report.cart_layout_notes.map((note, index) => <li key={index}>{note}</li>)}</ul>
        </section>}
        {!!report.audio_map_notes?.length && <section>
          <h3>Object audio</h3>
          <ul>{report.audio_map_notes.map((note, index) => <li key={index}>{note}</li>)}</ul>
        </section>}
        {outputs.length > 0 && <p>Assign screens in Preferences → Outputs for {outputs.join(", ")}.</p>}
      </section>
      <section>
        <h3>Before the first GO</h3>
        <p>Check routing, levels, fade shapes and follows against your QLab show. Use Check Workspace
          to locate missing media. Save As creates an Inkue workspace. Collect and Save gathers its media.</p>
      </section>
      <footer>
        <button className="primary" onClick={onCheck}>Check Workspace…</button>
        <button onClick={() => void saveReport()}>Save conversion report…</button>
        <span role="status">{status}</span>
      </footer>
    </OperatorDialog>
  );
}
