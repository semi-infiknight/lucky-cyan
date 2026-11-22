import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mock = vi.hoisted(() => ({
  workspace: {
    workspaceInfo: { instance_id: "show" } as { instance_id: string } | null,
    activeCueListId: "list" as string | null,
    cues: [
      { id: "disarmed", number: "101", is_disabled: true },
      { id: "next", number: "102", is_disabled: false },
    ],
  },
  listeners: new Map<string, (event: { payload: unknown }) => Promise<void>>(),
  setTiming: vi.fn(),
  cleanup: undefined as (() => void) | undefined,
}));

vi.mock("react", () => ({
  useEffect: (setup: () => () => void) => { mock.cleanup = setup(); },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, listener: (event: { payload: unknown }) => Promise<void>) => {
    mock.listeners.set(name, listener);
    return () => mock.listeners.delete(name);
  },
}));
vi.mock("../stores/workspaceStore", () => ({ useWorkspaceStore: { getState: () => mock.workspace } }));
vi.mock("../stores/transportStore", () => ({ useTransportStore: { getState: () => ({}) } }));
vi.mock("../stores/timingStore", () => ({ useTimingStore: { getState: () => ({ setTiming: mock.setTiming }) } }));
vi.mock("../lib/commands", () => ({
  go: vi.fn(async () => {}),
  goCue: vi.fn(async () => {}),
  setPlayhead: vi.fn(async () => {}),
  stopAll: vi.fn(async () => {}),
  stopCue: vi.fn(async () => {}),
  hardStopAll: vi.fn(async () => {}),
  pauseCue: vi.fn(async () => {}),
  resumeCue: vi.fn(async () => {}),
}));

import { go, goCue, setPlayhead, stopCue } from "../lib/commands";
import { useTauriEvents } from "./useTauriEvents";

it("allows the launch document to open after the workspace event listeners register", async () => {
  const workspaceEventsReady = vi.fn(() => {
    expect(mock.listeners.has("cue-lists-changed")).toBe(true);
    expect(mock.listeners.has("workspace-modified")).toBe(true);
    expect(mock.listeners.has("preferences-applied")).toBe(true);
    expect(mock.listeners.has("health-changed")).toBe(true);
  });
  useTauriEvents({ onWorkspaceEventsReady: workspaceEventsReady });
  try {
    expect(workspaceEventsReady).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(workspaceEventsReady).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(mock.listeners.has("osc-command")).toBe(true));
  } finally {
    mock.cleanup?.();
    mock.listeners.clear();
  }
});

describe("OSC GO dispatch", () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    mock.workspace.workspaceInfo = { instance_id: "show" };
    mock.workspace.activeCueListId = "list";
    useTauriEvents();
    await vi.waitFor(() => expect(mock.listeners.has("osc-command")).toBe(true));
  });
  afterEach(() => { mock.cleanup?.(); mock.listeners.clear(); });

  async function receive(command: string, cueNumber?: string) {
    await mock.listeners.get("osc-command")?.({ payload: { command, cue_number: cueNumber } });
  }

  it("still handles plain OSC GO", async () => {
    await receive("go");
    expect(go).toHaveBeenCalledOnce();
  });

  it("does not re-resolve native numbered commands in the WebView", async () => {
    mock.workspace.workspaceInfo = { instance_id: "other-show" };
    mock.workspace.activeCueListId = "other-list";
    await receive("cue_go", "101");
    await receive("cue_start", "102");
    await receive("cue_select", "102");
    await receive("cue_stop", "102");
    expect(goCue).not.toHaveBeenCalled();
    expect(setPlayhead).not.toHaveBeenCalled();
    expect(stopCue).not.toHaveBeenCalled();
    expect(go).not.toHaveBeenCalled();
  });

  it("carries workspace identity and unknown remaining time in the timing stream", async () => {
    await mock.listeners.get("cue-time-update")?.({ payload: {
      cue_id: "wait", workspace_id: "show", elapsed_ms: 1200, action_elapsed_ms: 1000, remaining_ms: null,
    } });
    expect(mock.setTiming).toHaveBeenCalledWith("wait", {
      workspace_id: "show", elapsed_ms: 1200, action_elapsed_ms: 1000, remaining_ms: null,
    });
  });
});
