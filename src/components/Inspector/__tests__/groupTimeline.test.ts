import { describe, expect, it } from "vitest";
import { dragEntranceMs, matchingTimelineOwner, MAX_PRE_WAIT_MS, nudgeTimelineEntrance, parseEntranceSeconds, snapTimelineEntrance, timelineActionTime, timelineGuideTimes, timelinePointerDelta, timelineReferenceOrder, timelineSpanMs, timelineZoomScrollLeft } from "../groupTimeline";

describe("Group Timeline entrances", () => {
  it("uses only the owner captured with the displayed group and its rows", () => {
    const owner = { workspace_id: "opened-show-A", list_id: "saved-list" };
    expect(matchingTimelineOwner(owner, "opened-show-A", "saved-list")).toBe(owner);
    expect(matchingTimelineOwner(owner, "opened-copy-B", "saved-list")).toBeUndefined();
    expect(matchingTimelineOwner(owner, "opened-show-A", "another-list")).toBeUndefined();
    expect(matchingTimelineOwner(undefined, undefined, null)).toBeUndefined();
  });

  it("uses the group's action clock only in the current workspace", () => {
    const timing = { workspace_id: "show", elapsed_ms: 4700, action_elapsed_ms: 1700, remaining_ms: null };
    expect(timelineActionTime(timing, "show")).toBe(1700);
    expect(timelineActionTime(timing, "reopened-show")).toBeNull();
    expect(timelineActionTime(undefined, "show")).toBeNull();
  });

  it("includes the duration after an entrance and gives unknown ends a start marker only", () => {
    expect(timelineSpanMs([])).toBe(4000);
    expect(timelineSpanMs([{ pre_wait_ms: 2000, duration_ms: null }])).toBe(4000);
    expect(timelineSpanMs([{ pre_wait_ms: 3000, duration_ms: 5000 }])).toBe(10000);
  });

  it("moves from the original entrance by pixel delta and keeps zero motion a no-op", () => {
    expect(dragEntranceMs(1250, 50, 400, 4000)).toBe(1750);
    expect(dragEntranceMs(1250, -200, 400, 4000)).toBe(0);
    expect(dragEntranceMs(MAX_PRE_WAIT_MS - 100, 50, 400, 4000)).toBe(MAX_PRE_WAIT_MS);
    expect(dragEntranceMs(1250, 0, 400, 4000)).toBe(1250);
  });

  it("keeps the same point in view when zooming and clamps both ends", () => {
    expect(timelineZoomScrollLeft(0, 400, 400, 800)).toBe(200);
    expect(timelineZoomScrollLeft(650, 1600, 400, 800)).toBe(225);
    expect(timelineZoomScrollLeft(0, 1600, 400, 800)).toBe(0);
    expect(timelineZoomScrollLeft(1200, 1600, 400, 800)).toBe(400);
    expect(timelineZoomScrollLeft(650, 1600, 400, 400)).toBe(0);
  });

  it("uses the full zoomed track scale during a drag, including horizontal scroll", () => {
    const trackLeft = -590;
    const initialX = 210;
    const grabX = initialX - trackLeft;
    const width = 1600;
    const span = 4000;
    const entrance = 2000;

    expect(dragEntranceMs(entrance, timelinePointerDelta(grabX, initialX, trackLeft), width, span)).toBe(2000);
    expect(dragEntranceMs(entrance, timelinePointerDelta(grabX, 250, trackLeft), width, span)).toBe(2100);
    const panned = dragEntranceMs(entrance, timelinePointerDelta(grabX, 250, -630), width, span);
    expect(panned).toBe(2200);
    expect(snapTimelineEntrance(panned, [2210], width, span)).toBe(2210);
    expect(snapTimelineEntrance(panned, [2220], width, span)).toBeNull();
  });

  it("keeps one reference at the top while preserving the authored rows and their objects", () => {
    const first = { id: "sound-check" };
    const second = { id: "doors" };
    const third = { id: "show-call" };
    const authored = Object.freeze([first, second, third]);

    const pinned = timelineReferenceOrder(authored, third.id);
    expect(pinned).toEqual([third, first, second]);
    expect(pinned[0]).toBe(third);
    expect(timelineReferenceOrder(authored, second.id)).toEqual([second, first, third]);
    expect(timelineReferenceOrder(authored, first.id)).toBe(authored);
    expect(timelineReferenceOrder(authored, null)).toBe(authored);
    expect(timelineReferenceOrder(authored, "removed-cue")).toBe(authored);
  });

  it("accepts millisecond keyboard precision and rejects missing or out-of-range input", () => {
    expect(parseEntranceSeconds("1.375")).toBe(1375);
    expect(parseEntranceSeconds("0")).toBe(0);
    expect(parseEntranceSeconds("86400")).toBe(MAX_PRE_WAIT_MS);
    for (const text of ["", "Infinity", "-0.1", "86400.001", "1.2s"]) {
      expect(parseEntranceSeconds(text)).toBeNull();
    }
  });

  it("guides to armed sibling starts and known ends without snapping to the moving cue", () => {
    const row = { id: "a", pre_wait_ms: 700, duration_ms: 4300,
      supports_timeline_entrance: true, is_disabled: false };
    const guides = timelineGuideTimes([
      row, { ...row, id: "b", pre_wait_ms: 2000, duration_ms: null },
      { ...row, id: "c", pre_wait_ms: 1500, duration_ms: 2500 },
      { ...row, id: "d", pre_wait_ms: 8500, is_disabled: true },
      { ...row, id: "e", pre_wait_ms: 300, supports_timeline_entrance: false },
    ], "a");
    expect(guides).toEqual([0, 1500, 2000, 4000]);
    expect(snapTimelineEntrance(1475, guides, 400, 4000)).toBe(1500);
    expect(snapTimelineEntrance(3950, guides, 400, 4000)).toBe(4000);
    expect(snapTimelineEntrance(3800, guides, 400, 4000)).toBeNull();
    expect(snapTimelineEntrance(20, guides, 400, 4000)).toBe(0);
  });

  it("nudges by 100 ms or 10 ms without leaving the playable range", () => {
    expect(nudgeTimelineEntrance(1250, true, false)).toBe(1350);
    expect(nudgeTimelineEntrance(1250, false, true)).toBe(1240);
    expect(nudgeTimelineEntrance(5, false, true)).toBe(0);
    expect(nudgeTimelineEntrance(MAX_PRE_WAIT_MS - 32, true, false)).toBe(MAX_PRE_WAIT_MS);
  });
});
