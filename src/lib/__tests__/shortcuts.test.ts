import { describe, expect, it } from "vitest";
import { cueHotkey, isShowShortcut } from "../shortcuts";

describe("Show Mode keyboard controls", () => {
  for (const key of [" ", "Escape", "F5", "F9", "g", "S", "p", "[", "]"]) {
    it(`preserves ${key}`, () => expect(isShowShortcut({ key } as KeyboardEvent)).toBe(true));
  }
  for (const key of ["n", "o", "v", "z", "d", "s", "g", "Delete"]) {
    it(`blocks the editing shortcut ${key}`, () =>
      expect(isShowShortcut({ key, ctrlKey: true, metaKey: true } as KeyboardEvent)).toBe(false));
  }
});

describe("cue trigger keys", () => {
  const from = (values: Partial<KeyboardEvent>) => cueHotkey(values as KeyboardEvent);

  it("treats top-row and keypad digits as the same logical trigger", () => {
    expect(from({ key: "1", code: "Digit1" })).toBe("1");
    expect(from({ key: "1", code: "Numpad1" })).toBe("1");
    expect(from({ key: "0" })).toBe("0");
    expect(from({ key: "End", code: "Numpad1" })).toBeNull();
    expect(from({ key: "1", shiftKey: true })).toBeNull();
  });

  it("reserves transport and modifier combinations", () => {
    expect(from({ key: "J" })).toBe("J");
    expect(from({ key: "j", shiftKey: true })).toBe("Shift+J");
    for (const key of ["G", "p", "s", "Escape", " ", "F5", "ArrowDown", "!"]) {
      expect(from({ key })).toBeNull();
    }
    expect(from({ key: "1", getModifierState: modifier => modifier === "AltGraph" })).toBeNull();
    expect(from({ key: "9", ctrlKey: true })).toBeNull();
  });
});
