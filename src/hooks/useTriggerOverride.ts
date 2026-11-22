import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { getMidiTriggerOverride, setMidiTriggerOverride, getTcTriggerOverride, setTcTriggerOverride } from "../lib/commands";

const sources = {
  midi: { event: "midi-trigger-override", read: getMidiTriggerOverride, set: setMidiTriggerOverride },
  timecode: { event: "timecode-trigger-override", read: getTcTriggerOverride, set: setTcTriggerOverride },
};

export function useTriggerOverride(source: keyof typeof sources) {
  const { event: eventName, read, set } = sources[source];
  const [suspended, setSuspended] = useState<boolean | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    let mounted = true;
    let receivedChange = false;
    const subscription = listen<boolean>(eventName, event => {
      if (mounted) {
        receivedChange = true;
        setSuspended(event.payload);
        setError("");
      }
    });
    void subscription.then(() => read())
      .then(value => { if (mounted && !receivedChange) setSuspended(value); })
      .catch(cause => { if (mounted) setError(String(cause)); });
    return () => {
      mounted = false;
      void subscription.then(unlisten => unlisten()).catch(console.error);
    };
  }, [eventName, read]);

  const change = async (next: boolean) => {
    setPending(true);
    setError("");
    try { await set(next); }
    catch (cause) { setError(String(cause)); }
    finally { setPending(false); }
  };
  return { suspended, pending, change, error };
}
