import { describe, expect, it } from "vitest";
import { cueAncestors, findCueBranches, navigateRows, rangeOfRows } from "../listNavigation";

const group = { cue: { id: "g", cue_type: "group", children: [{}] }, parentGroupId: null };
const child = { cue: { id: "a", cue_type: "audio" }, parentGroupId: "g" };
const next = { cue: { id: "b", cue_type: "wait" }, parentGroupId: null };
const expanded = new Set(["g"]);

describe("visible cue list navigation", () => {
  it("reveals only the ancestors of a matching nested cue", () => {
    const cues = [{ id: "g1", children: [{ id: "g2", children: [{ id: "a" }] }] }, { id: "b" }];
    expect(cueAncestors(cues, "a")).toEqual(["g1", "g2"]);
    expect(cueAncestors(cues, "b")).toEqual([]);
    expect(cueAncestors(cues, "missing")).toEqual([]);
  });
  it("opens every matching branch for Select Found", () => {
    const cues = [
      { id: "g1", children: [{ id: "g2", children: [{ id: "a" }] }, { id: "b" }] },
      { id: "g3", children: [{ id: "c" }] },
      { id: "g4", children: [{ id: "d" }] },
    ];
    expect(findCueBranches(cues, ["a", "b", "c", "missing"])).toEqual(new Set(["g1", "g2", "g3"]));
    expect(findCueBranches(cues, ["g4"])).toEqual(new Set());
  });
  it("visits group children in show order and skips collapsed rows", () => {
    expect(navigateRows([group, child, next], "g", "ArrowDown", expanded)).toEqual({ select: "a" });
    expect(navigateRows([group, next], "g", "ArrowDown", new Set())).toEqual({ select: "b" });
  });
  it("opens, enters, exits and closes a group with the arrow keys", () => {
    expect(navigateRows([group, next], "g", "ArrowRight", new Set())).toEqual({ toggle: "g" });
    expect(navigateRows([group, child, next], "g", "ArrowRight", expanded)).toEqual({ select: "a" });
    expect(navigateRows([group, child, next], "a", "ArrowLeft", expanded)).toEqual({ select: "g" });
    expect(navigateRows([group, child, next], "g", "ArrowLeft", expanded)).toEqual({ toggle: "g" });
  });
  it("extends and contracts ranges through visible groups", () => {
    const rows = [group, child, next];
    expect(rangeOfRows(rows, "b", "g")).toEqual(["g", "a", "b"]);
    expect(rangeOfRows(rows, "b", "a")).toEqual(["a", "b"]);
    expect(rangeOfRows([group, next], "a", "b")).toEqual(["b"]);
  });
});
