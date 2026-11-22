import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { CueMonitorSnapshot } from "../../lib/types";
import { CueMonitorContents, monitorTimes } from "./CueMonitor";

describe("Cue monitor", () => {
  it("shows standby, nested running and paused rows across cue lists without playback actions", () => {
    const cue = { id: "memo", number: "1", name: "Dry call", kind: "Memo", is_disabled: true };
    const status: CueMonitorSnapshot = { workspace_id: "rehearsal", name: "Rehearsal", active_cue_list_id: "main", lists: [
      { id: "main", name: "Calls", standby: cue, active: [
        { cue: { ...cue, id: "first", name: "Long Wait" }, parent_id: null, state: "paused" },
      ] },
      { id: "cart", name: "Auxiliary", standby: null, active: [
        { cue: { ...cue, id: "nested", name: "Presettling Wait" }, parent_id: "group", state: "running" },
      ] },
    ] };
    const alerts = [{ key: "output", level: "error" as const, message: "Output unavailable",
      action: "restore_audio_device", action_label: "Restore output" }];
    const html = renderToStaticMarkup(createElement(CueMonitorContents, { status, alerts }));
    for (const expected of [
      "Dry call", "Disarmed", "Selected on desk", "Calls active cues", "Paused",
      "Auxiliary active cues", 'data-nested="true"', "Running", "No cue at playhead", "Output unavailable",
    ]) {
      expect(html).toContain(expected);
    }
    expect(html).not.toContain("Restore output");
    expect(html).not.toContain("<button");

    const select = () => {};
    const programming = renderToStaticMarkup(createElement(CueMonitorContents, {
      status, alerts, inspection: { listId: "main", select },
    }));
    expect(programming).toContain('aria-label="Inspect cue 1 Dry call"');
    expect(programming).toContain('aria-label="Inspect cue 1 Long Wait"');
    expect(programming).not.toContain('aria-label="Inspect cue 1 Presettling Wait"');

    const stale = renderToStaticMarkup(createElement(CueMonitorContents, {
      status, alerts, inspection: { listId: "cart", select },
    }));
    expect(stale).not.toContain("<button");
  });

  it("rounds sampled clocks to whole seconds and ignores timing from another workspace", () => {
    const sample = { workspace_id: "show", elapsed_ms: 3500, action_elapsed_ms: 2750, remaining_ms: 8200 };
    expect(monitorTimes(sample, "show")).toEqual(["0:02", "0:08"]);
    expect(monitorTimes({ ...sample, action_elapsed_ms: 2999, remaining_ms: 8000 }, "show")).toEqual(["0:02", "0:08"]);
    expect(monitorTimes({ ...sample, remaining_ms: null }, "show")).toEqual(["0:02", "—"]);
    expect(monitorTimes(sample, "other")).toEqual(["—", "—"]);
    expect(monitorTimes(undefined, "show")).toEqual(["—", "—"]);
  });
});
