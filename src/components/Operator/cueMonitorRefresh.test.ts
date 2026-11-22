import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mock = vi.hoisted(() => ({
  read: vi.fn(),
  listeners: new Map<string, (event: { payload?: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, callback: (event: { payload?: unknown }) => void) => {
    mock.listeners.set(event, callback);
    return () => mock.listeners.delete(event);
  },
}));
vi.mock("../../lib/commands", () => ({ getCueMonitorSnapshot: mock.read }));

import { watchCueMonitor } from "./cueMonitorRefresh";

const snapshot = { workspace_id: "show", name: "Rehearsal", active_cue_list_id: "main", lists: [] };
const send = (event: string, payload?: unknown) => mock.listeners.get(event)?.({ payload });

describe("cue monitor metadata refresh", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    mock.read.mockReset().mockResolvedValue(snapshot);
  });
  afterEach(() => {
    mock.listeners.clear();
    vi.useRealTimers();
  });

  it("reads once on open, coalesces metadata events, and ignores clock samples", async () => {
    const publish = vi.fn();
    const monitor = watchCueMonitor("show", publish, vi.fn());
    await vi.advanceTimersByTimeAsync(1000);
    expect(mock.read).toHaveBeenCalledExactlyOnceWith("show");
    expect(publish).toHaveBeenCalledExactlyOnceWith(snapshot);
    expect(mock.listeners.has("cue-time-update")).toBe(false);

    send("cue-state-changed");
    send("playhead-moved");
    send("cue-list-refresh");
    send("cue-monitor-structure-changed", { workspace_id: "other-show" });
    await vi.advanceTimersByTimeAsync(1000);
    expect(mock.read).toHaveBeenCalledTimes(2);
    expect(publish).toHaveBeenCalledTimes(1);

    mock.read.mockResolvedValue({ ...snapshot, name: "New cue list status" });
    send("cue-monitor-structure-changed", { workspace_id: "show" });
    await vi.advanceTimersByTimeAsync(50);
    expect(mock.read).toHaveBeenCalledTimes(3);
    expect(publish).toHaveBeenLastCalledWith({ ...snapshot, name: "New cue list status" });
    monitor.dispose();
    expect(mock.listeners.size).toBe(0);
    await vi.advanceTimersByTimeAsync(5000);
    expect(mock.read).toHaveBeenCalledTimes(3);
  });

  it("bounds busy retries, then waits for another notification or explicit refresh", async () => {
    mock.read.mockResolvedValue(null);
    const notice = vi.fn();
    const monitor = watchCueMonitor("show", vi.fn(), notice);
    await vi.advanceTimersByTimeAsync(5000);
    expect(mock.read).toHaveBeenCalledTimes(4);
    expect(notice).toHaveBeenLastCalledWith("Desk busy. Choose Refresh to update.");
    monitor.refresh();
    mock.read.mockResolvedValue(snapshot);
    await vi.advanceTimersByTimeAsync(5000);
    expect(mock.read).toHaveBeenCalledTimes(5);
    expect(notice).toHaveBeenLastCalledWith("");
    monitor.dispose();
  });

  it("discards an in-flight snapshot invalidated by an event and refreshes once", async () => {
    let finish: (value: typeof snapshot) => void = () => {};
    mock.read.mockReturnValueOnce(new Promise(resolve => { finish = resolve; }));
    const publish = vi.fn();
    const monitor = watchCueMonitor("show", publish, vi.fn());
    await vi.advanceTimersByTimeAsync(100);
    send("cue-lists-changed");
    send("workspace-modified");
    finish({ ...snapshot, name: "Old status" });
    await vi.advanceTimersByTimeAsync(500);
    expect(mock.read).toHaveBeenCalledTimes(2);
    expect(publish).toHaveBeenCalledExactlyOnceWith(snapshot);
    monitor.dispose();
  });
});
