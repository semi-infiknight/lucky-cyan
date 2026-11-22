import { createElement, type ComponentProps } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { agentReviewConnect, agentReviewDisconnect } from "../../lib/commands";
import { CueReviewAccess } from "./AgentPanel";

const reviewFile = "/desk/local/private-review.json";
const props: ComponentProps<typeof CueReviewAccess> = {
  connected: true,
  locked: false,
  workspaceId: "displayed-workspace",
  reviewFile: null,
  busy: false,
  onEnable: () => {},
  onRevoke: () => {},
};
const markup = (update: Partial<typeof props> = {}) =>
  renderToStaticMarkup(createElement(CueReviewAccess, { ...props, ...update }));

describe("cue review access", () => {
  it("requires a session and workspace and stays disabled in Show Mode", () => {
    expect(markup({ connected: false })).toContain("Start an Agent access session");
    expect(markup({ connected: false })).not.toContain("Enable cue review</button>");
    expect(markup({ workspaceId: undefined })).toContain("Open a workspace");
    const locked = markup({ locked: true, reviewFile });
    expect(locked).toContain("Cue review is unavailable in Show Mode.");
    expect(locked).not.toContain(reviewFile);
    expect(locked).not.toContain("Copy path");
    expect(locked).not.toContain("Enable cue review</button>");
  });

  it("renders explicit opt-in and a private, labeled path only after enablement", () => {
    invoke.mockClear();
    const inactive = markup({ busy: true });
    expect(inactive).toContain('<button type="button" disabled="">Enable cue review</button>');
    expect(inactive).not.toContain("Private cue review file");
    const active = markup({ reviewFile });
    expect(active).toContain("edit a note if it still matches their starting text");
    expect(active).toContain("undo history");
    expect(active).toContain("Private cue review file");
    expect(active).toContain('readonly=""');
    expect(active).toContain(reviewFile);
    expect(active).toContain("Copy path");
    expect(active).toContain("Revoke cue review");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("sends the displayed workspace owner on both explicit review commands", async () => {
    invoke.mockClear();
    await agentReviewConnect("displayed-workspace");
    await agentReviewDisconnect("displayed-workspace");
    expect(invoke.mock.calls).toEqual([
      ["agent_review_connect", { workspaceId: "displayed-workspace" }],
      ["agent_review_disconnect", { workspaceId: "displayed-workspace" }],
    ]);
  });
});
