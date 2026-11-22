import { useId, useState } from "react";
import type { KeyboardEvent } from "react";
import { setCueHotkey } from "../../lib/commands";
import { cueHotkey, CUE_HOTKEY_DIGITS, CUE_HOTKEY_FUNCTION_KEYS, CUE_HOTKEY_LETTERS } from "../../lib/shortcuts";
import type { CueSummary } from "../../lib/types";
import { useWorkspaceStore } from "../../stores/workspaceStore";

export function CueHotkeySection({ cue, onSave }: { cue: CueSummary; onSave?: () => void }) {
  const cues = useWorkspaceStore(state => state.cues);
  const workspaceId = useWorkspaceStore(state => state.workspaceInfo?.instance_id);
  const listId = useWorkspaceStore(state => state.activeCueListId);
  const [busy, setBusy] = useState(false);
  const [capture, setCapture] = useState<{ expectedKey: string | null } | null>(null);
  const [error, setError] = useState("");
  const hint = useId();
  const owner = cue.cue_list_owner;
  const currentOwner = owner && owner.workspace_id === workspaceId && owner.list_id === listId ? owner : null;
  const eligible = Boolean(currentOwner) && cues.some(top => top.id === cue.id ||
    ((top.group_mode === "sequential" || top.group_mode === "playlist" || top.group_mode === "start_first_and_enter") &&
      top.children?.some(child => child.id === cue.id)));

  async function save(key: string | null, expectedKey: string | null) {
    if (!currentOwner || key === expectedKey) return;
    setBusy(true);
    setError("");
    try {
      await setCueHotkey(currentOwner.workspace_id, currentOwner.list_id, cue.id, expectedKey, key);
    } catch (cause) {
      setError(String(cause));
    } finally {
      try {
        await onSave?.();
      } finally {
        setBusy(false);
      }
    }
  }

  function record(event: KeyboardEvent<HTMLButtonElement>) {
    if (!capture) return;
    if (event.key === "Escape" || event.key === "Tab") {
      // Leave the desk's stop shortcut and ordinary focus navigation available.
      setCapture(null);
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (!event.nativeEvent.isTrusted || event.repeat || event.nativeEvent.isComposing) return;
    if (["Shift", "Control", "Alt", "Meta"].includes(event.key)) return;
    const key = cueHotkey(event.nativeEvent);
    if (!key) {
      setError("Use a key from the list. Shift is available for letters and function keys.");
      return;
    }
    setCapture(null);
    void save(key, capture.expectedKey);
  }

  return <div style={{ marginBottom: 16 }} data-cue-hotkeys-blocked={capture ? true : undefined}>
    <div style={{ display: "flex", flexWrap: "wrap", alignItems: "center", gap: 6 }}>
      <label>Keyboard trigger{" "}
        <select value={cue.hotkey ?? ""} disabled={busy || !currentOwner || (!eligible && !cue.hotkey)}
          onChange={e => void save(e.currentTarget.value || null, cue.hotkey ?? null)}>
          <option value="">Unassigned</option>
          {["", "Shift+"].map(prefix => <optgroup key={prefix} label={prefix ? "With Shift" : "Letter"}>
            {CUE_HOTKEY_LETTERS.map(letter => <option key={letter} disabled={!eligible} value={prefix + letter}>
              {prefix + letter}
            </option>)}
          </optgroup>)}
          <optgroup label="Number">
            {CUE_HOTKEY_DIGITS.map(digit => <option key={digit} disabled={!eligible} value={digit}>{digit}</option>)}
          </optgroup>
          {["", "Shift+"].map(prefix => <optgroup key={prefix} label={prefix ? "Function key with Shift" : "Function key"}>
            {CUE_HOTKEY_FUNCTION_KEYS.map(key => <option key={key} disabled={!eligible} value={prefix + key}>
              {prefix + key}
            </option>)}
          </optgroup>)}
        </select>
      </label>
      <button type="button" aria-pressed={Boolean(capture)} aria-describedby={hint} aria-disabled={busy || !eligible}
        onClick={() => {
          if (busy || !eligible) return;
          setError("");
          setCapture(capture ? null : { expectedKey: cue.hotkey ?? null });
        }}
        onKeyDown={record} onBlur={() => setCapture(null)}>
        {capture ? "Press a key…" : "Capture key"}
      </button>
    </div>
    <p id={hint}>{eligible
      ? "Use the assigned key with focus in this cue list, its trigger pads or Show Mode."
      : "Assign the hotkey to the group, a top-level cue or a direct child of a top-level ordered group."}</p>
    {error && <p role="alert">{error}</p>}
  </div>;
}
