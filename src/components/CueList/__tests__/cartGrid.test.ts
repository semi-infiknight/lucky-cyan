import { describe, expect, it } from "vitest";
import type { CartLayout, CueSummary } from "../../../lib/types";
import { cartGrid, cellAt } from "../cartGrid";

const cues = ["a", "b", "c", "d"].map(id => ({ id }) as CueSummary);
const layout: CartLayout = {
  rows: 2, columns: 3,
  pads: { a: { row: 2, column: 3 }, b: { row: 1, column: 2 }, c: { row: 3, column: 1 }, d: null },
};

describe("fixed cart cells", () => {
  it("leaves holes and keeps physical positions when cue order changes", () => {
    for (const order of [cues, [...cues].reverse()]) {
      expect(cartGrid(order, layout).cells.map(c => c?.id ?? null)).toEqual([null, "b", null, null, null, "a"]);
    }
  });
  it("keeps out-of-bounds and unplaced pads available to the editor", () => {
    expect(cartGrid(cues, layout).outside.map(c => c.id)).toEqual(["c", "d"]);
    const resized = cartGrid(cues, { ...layout, rows: 3 });
    expect(resized.cells[6]?.id).toBe("c");
    expect(resized.outside.map(c => c.id)).toEqual(["d"]);
  });
  it("uses row first and one-based coordinates for edits", () => {
    expect(cellAt(0, 3)).toEqual({ row: 1, column: 1 });
    expect(cellAt(5, 3)).toEqual({ row: 2, column: 3 });
  });
});
