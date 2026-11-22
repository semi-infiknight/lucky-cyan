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

/** Canonical logical key. Caps Lock changes the label, never the Shift binding. */
export function cueHotkey(event: KeyboardEvent): string | null {
  if (event.altKey || event.ctrlKey || event.metaKey || event.isComposing ||
      event.getModifierState?.("AltGraph")) return null;
  if (!event.shiftKey && CUE_HOTKEY_DIGITS.includes(event.key)) return event.key;
  if (!/^[a-z]$/i.test(event.key)) return null;
  const letter = event.key.toUpperCase();
  if (!CUE_HOTKEY_LETTERS.includes(letter)) return null;
  return (event.shiftKey ? "Shift+" : "") + letter;
}
