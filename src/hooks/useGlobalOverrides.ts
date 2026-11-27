import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { getGlobalOverrides, setGlobalOverride, type GlobalOverrides } from "../lib/commands";

export const OVERRIDE_LABELS: ReadonlyArray<readonly [string, string]> = [
  ["dmx_output", "DMX output"],
  ["midi_input", "MIDI input"],
  ["midi_output", "MIDI output"],
  ["msc_input", "MSC input"],
  ["msc_output", "MSC output"],
  ["network_external_input", "External network input"],
  ["timecode_input", "Timecode input"],
];

export function useGlobalOverrides() {
  const [overrides, setOverrides] = useState<GlobalOverrides | null>(null);
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    let mounted = true;
    const subscription = listen<GlobalOverrides>("global-overrides-changed", event => {
      if (mounted) {
        setOverrides(event.payload);
        setError("");
      }
    });
    void subscription
      .then(() => getGlobalOverrides())
      .then(value => { if (mounted) setOverrides(value); })
      .catch(cause => { if (mounted) setError(String(cause)); });
    return () => {
      mounted = false;
      void subscription.then(unlisten => unlisten()).catch(console.error);
    };
  }, []);

  const toggle = async (key: string) => {
    setPending(key);
    setError("");
    try {
      await setGlobalOverride(key, !(overrides?.[key] ?? true));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setPending("");
    }
  };

  return { overrides, open, setOpen, toggle, pending, error };
}
