import { describe, expect, it } from "vitest";
import type { CueSummary } from "../../lib/types";
import { nextGoCue, runSheetRows } from "./runSheet";

function cue(id: string, overrides: Partial<CueSummary> = {}): CueSummary {
  return {
    id, name: id, number: id, cue_type: "memo", notes: "", state: "standby",
    continue_mode: "do_not_continue", color: "none", pre_wait_ms: 0, post_wait_ms: 0,
    duration_ms: null, file_path: null, file_duration_ms: null, is_loading: false,
    is_disabled: false, is_warning: false, is_broken: false, seekable: false, ...overrides,
  };
}

describe("Show Mode standby", () => {
  it("skips disarmed rows without treating earlier rows as completed", () => {
    const cues = [cue("1", { is_disabled: true }), cue("2")];
    expect(nextGoCue(cues, "1")?.cue.id).toBe("2");
    expect(runSheetRows(cues)[0].cue.state).toBe("standby");
    expect(nextGoCue(cues, null)).toBeNull();
    expect(nextGoCue(cues, "elsewhere")).toBeNull();
  });

  it("follows the native next child through nested ordered groups", () => {
    const inner = cue("group 2", { cue_type: "group", group_mode: "playlist",
      children: [cue("A"), cue("B")], active_child_id: "B" });
    const outer = cue("group 1", { cue_type: "group", group_mode: "sequential",
      children: [inner], active_child_id: inner.id });
    const standby = nextGoCue([outer], outer.id);
    expect(standby?.cue.id).toBe("B");
    expect(standby?.groups.map(c => c.id)).toEqual([outer.id, inner.id]);
    expect(runSheetRows([outer]).map(row => row.depth)).toEqual([0, 1, 2, 2]);
  });

  it("shows the exact standby child while inside Start First And Enter", () => {
    const group = cue("G", { cue_type: "group", group_mode: "start_first_and_enter",
      children: [cue("1"), cue("2")], active_child_id: "2" });
    const outside = cue("3");
    expect(nextGoCue([group, outside], group.id)?.cue.id).toBe("2");
    expect(nextGoCue([group, outside], outside.id)?.cue.id).toBe("3");
  });

  it("keeps groups visible when their children have no manual standby", () => {
    for (const mode of ["start_first", "start_random", "simultaneous"] as const) {
      const group = cue("G", { cue_type: "group", group_mode: mode, children: [cue("1"), cue("2")] });
      expect(nextGoCue([group], "G")?.cue.id).toBe("G");
    }
  });
});
