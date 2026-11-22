import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { getMidiTriggerOverride, setMidiTriggerOverride } from "../lib/commands";

export function useMidiTriggerOverride() {
  const [suspended, setSuspended] = useState<boolean | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    let mounted = true;
    let receivedChange = false;
    const subscription = listen<boolean>("midi-trigger-override", event => {
      if (mounted) {
        receivedChange = true;
        setSuspended(event.payload);
        setError("");
      }
    });
    void subscription.then(() => getMidiTriggerOverride())
      .then(value => { if (mounted && !receivedChange) setSuspended(value); })
      .catch(cause => { if (mounted) setError(String(cause)); });
    return () => {
      mounted = false;
      void subscription.then(unlisten => unlisten()).catch(console.error);
    };
  }, []);

  const change = async (next: boolean) => {
    setPending(true);
    setError("");
    try { await setMidiTriggerOverride(next); }
    catch (cause) { setError(String(cause)); }
    finally { setPending(false); }
  };
  return { suspended, pending, change, error };
}
