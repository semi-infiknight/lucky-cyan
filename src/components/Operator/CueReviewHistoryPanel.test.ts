import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { getCueReviewHistory } from "../../lib/commands";
import { CueReviewHistoryTable } from "./CueReviewHistoryPanel";

describe("cue review history", () => {
  it("shows recorded time, cue list and current cue identity without edit controls", () => {
    const time = "2026-11-22T10:23:49Z";
    const html = renderToStaticMarkup(createElement(CueReviewHistoryTable, { rows: [
      { event_index: 2, time, list_name: "Calls", cue_number: "Q4.1", cue_name: "Opening call" },
      { event_index: 1, time, list_name: null, cue_number: null, cue_name: null },
    ] }));
    for (const expected of ["Recorded cue review edits", time, "Calls", "Q4.1", "Opening call", "Removed cue list", "Removed cue"]) {
      expect(html).toContain(expected);
    }
    expect(html).not.toContain("<button");
  });

  it("reports an empty history and scopes each read to the displayed workspace", async () => {
    expect(renderToStaticMarkup(createElement(CueReviewHistoryTable, { rows: [] }))).toContain("No recent cue review edits.");
    invoke.mockClear();
    await getCueReviewHistory("displayed-show");
    expect(invoke).toHaveBeenCalledExactlyOnceWith("get_cue_review_history", { workspaceId: "displayed-show" });
  });
});
