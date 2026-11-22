import { describe, expect, it } from "vitest";
import { parseCrosspoint, routedBanks, SILENT_DB, stereoMatrix, withCrosspoint, withSourceChannelName } from "../levelMatrix";

describe("multichannel routing edits", () => {
  it("changes one route and retains offscreen, ragged and legacy channels", () => {
    const original = [Array(135).fill(-12), [0], [], Array(64).fill(-18)];
    const next = withCrosspoint(original, 0, 3, -7);
    expect(next[0][3]).toBe(-7);
    expect(original[0][3]).toBe(-12);
    expect(next[0].slice(4)).toEqual(original[0].slice(4));
    expect(next.slice(1)).toEqual(original.slice(1));
  });

  it("extends only the selected route and keeps new crosspoints silent", () => {
    const matrix = stereoMatrix();
    const next = withCrosspoint(matrix, 127, 127, -6);
    expect(next[0]).toBe(matrix[0]);
    expect(next[1]).toBe(matrix[1]);
    expect(next[126]).toEqual([]);
    expect(next[127]).toEqual([...Array(127).fill(SILENT_DB), -6]);
    expect(withCrosspoint(next, 32, 100, SILENT_DB)).toBe(next);
    expect(withCrosspoint(next, 128, 0, 3)).toBe(next);
  });

  it("finds distant active routes in channel order and skips silent pages", () => {
    let matrix = withCrosspoint(stereoMatrix(), 127, 127, -3);
    matrix = withCrosspoint(matrix, 17, 8, -6);
    expect(routedBanks(matrix)).toEqual([
      {inputStart: 0, outputStart: 0, count: 2},
      {inputStart: 16, outputStart: 8, count: 1},
      {inputStart: 120, outputStart: 124, count: 1},
    ]);
  });

  it("allows precise gain, clamps the range and leaves incomplete typing uncommitted", () => {
    expect(parseCrosspoint("-12.75")).toBe(-12.75);
    expect(parseCrosspoint("−∞")).toBe(SILENT_DB);
    expect(parseCrosspoint("-inf")).toBe(SILENT_DB);
    expect(parseCrosspoint("-90")).toBe(SILENT_DB);
    expect(parseCrosspoint("50")).toBe(12);
    for (const text of ["", "-", "1z", "Infinity", "NaN"]) expect(parseCrosspoint(text)).toBeUndefined();
  });
});

it("keeps names aligned through edits and clearing the last channel", () => {
  const names = withSourceChannelName(["Boom"], 127, "  Choir  ");
  expect(names).toEqual(["Boom", ...Array(126).fill(""), "Choir"]);
  const renamed = withSourceChannelName(names, 2, "Radio");
  expect(withSourceChannelName(renamed, 127, "")).toEqual(["Boom", "", "Radio"]);
  expect(names[2]).toBe("");
});
