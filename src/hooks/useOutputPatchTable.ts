import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getOutputPatchTable, type OutputPatchTable } from "../lib/commands";

export function useOutputPatchTable() {
  const [table, setTable] = useState<OutputPatchTable>();
  const [error, setError] = useState("");
  useEffect(() => {
    let active = true;
    let request = 0;
    const refresh = () => {
      const current = ++request;
      void getOutputPatchTable().then(result => {
        if (active && request === current) {
          setTable(result);
          setError("");
        }
      }).catch(error => { if (active && request === current) setError(String(error)); });
    };
    const listener = listen("output-patches-changed", refresh);
    void listener.then(() => { if (active) refresh(); }).catch(error => { if (active) setError(String(error)); });
    refresh();
    return () => {
      active = false;
      void listener.then(unlisten => unlisten()).catch(console.error);
    };
  }, []);
  return { table, error };
}
