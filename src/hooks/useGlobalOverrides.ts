import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import {
  getGlobalOverrides,
  getOverrideWindowOpen,
  setGlobalOverride,
  setOverrideWindowOpen,
  type GlobalOverrides,
} from "../lib/commands";

export const OVERRIDE_LABELS: ReadonlyArray<readonly [string, string]> = [
  ["dmx_output", "DMX output"],
  ["midi_input", "MIDI input"],
  ["midi_output", "MIDI output"],
  ["msc_input", "MSC input"],
  ["msc_output", "MSC output"],
  ["network_external_input", "External network input"],
  ["network_external_output", "External network output"],
  ["network_local_input", "Local network input"],
  ["network_local_output", "Local network output"],
  ["sysex_input", "SysEx input"],
  ["sysex_output", "SysEx output"],
  ["timecode_input", "Timecode input"],
  ["timecode_output", "Timecode output"],
];

export function useGlobalOverrides() {
  const [overrides, setOverrides] = useState<GlobalOverrides | null>(null);
  const [open, setOpenState] = useState(false);
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
    // QLab `/overrideWindow {bool}` drives the same panel via OSC.
    const windowSubscription = listen<boolean>("override-window-changed", event => {
      if (mounted) setOpenState(event.payload);
    });
    void subscription
      .then(() => getGlobalOverrides())
      .then(value => { if (mounted) setOverrides(value); })
      .catch(cause => { if (mounted) setError(String(cause)); });
    void windowSubscription
      .then(() => getOverrideWindowOpen())
      .then(value => { if (mounted) setOpenState(value); })
      .catch(() => {});
    return () => {
      mounted = false;
      void subscription.then(unlisten => unlisten()).catch(console.error);
      void windowSubscription.then(unlisten => unlisten()).catch(console.error);
    };
  }, []);

  // Keep the UI button and OSC `/overrideWindow` on the same backend state.
  const setOpen = (value: boolean) => {
    setOpenState(value);
    void setOverrideWindowOpen(value).catch(() => {});
  };

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
