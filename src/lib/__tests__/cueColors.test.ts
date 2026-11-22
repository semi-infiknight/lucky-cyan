import { describe, expect, it } from "vitest";
import { CUE_COLOR_SWATCHES, cueAccent } from "../cueColors";

describe("cue accents", () => {
  it("uses an effective native color when the cue switched", () => {
    expect(cueAccent({ color: "red", display_color: "forest" })).toBe(CUE_COLOR_SWATCHES.forest);
  });

  it("preserves an explicit no-highlight override", () => {
    expect(cueAccent({ color: "red", display_color: "none" })).toBe("transparent");
  });

  it("falls back to the programmed color when a summary has no display color", () => {
    expect(cueAccent({ color: "red" })).toBe(CUE_COLOR_SWATCHES.red);
  });
});
