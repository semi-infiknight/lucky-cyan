const mac = typeof navigator !== "undefined" && /mac/i.test(navigator.platform);
export const modifierLabel = mac ? "Cmd" : "Ctrl";

export function commandModifier(event: KeyboardEvent) {
  return mac ? event.metaKey : event.ctrlKey;
}

export function isShowShortcut(event: KeyboardEvent): boolean {
  if (event.altKey) return false;
  if (commandModifier(event)) return event.key === "ArrowUp" || event.key === "ArrowDown";
  return [" ", "escape", "f5", "f9", "s", "p", "[", "]", "g"].includes(event.key.toLowerCase());
}

export const CUE_HOTKEY_LETTERS = Array.from("ABCDEFHIJKLMNOQRTUVWXYZ");
export const CUE_HOTKEY_DIGITS = Array.from("0123456789");
export const CUE_HOTKEY_FUNCTION_KEYS = [
  "F2", "F3", "F4", "F6", "F7", "F8",
  "F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
];

/** Canonical logical key. Caps Lock changes the label, never the Shift binding. */
export function cueHotkey(event: KeyboardEvent): string | null {
  if (event.altKey || event.ctrlKey || event.metaKey || event.isComposing ||
      event.getModifierState?.("AltGraph")) return null;
  if (!event.shiftKey && CUE_HOTKEY_DIGITS.includes(event.key)) return event.key;
  const prefix = event.shiftKey ? "Shift+" : "";
  if (CUE_HOTKEY_FUNCTION_KEYS.includes(event.key)) return prefix + event.key;
  if (!/^[a-z]$/i.test(event.key)) return null;
  const letter = event.key.toUpperCase();
  if (!CUE_HOTKEY_LETTERS.includes(letter)) return null;
  return prefix + letter;
}
