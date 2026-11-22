import { describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { setCueNotes } from "../../../lib/commands";
import { noteSaveRequest, observeSavedNote } from "../CueNotesField";

describe("cue note drafts", () => {
  it("updates a clean field from remote text without sending an untouched blur", () => {
    const before = { base: "Before\r\n ", text: "Before\r\n ", saved: "Before\r\n " };
    expect(noteSaveRequest(before, false)).toBeNull();
    const updated = observeSavedNote(before, "Reviewer\n ");
    expect(updated).toEqual({ base: "Reviewer\n ", text: "Reviewer\n ", saved: "Reviewer\n " });
    expect(noteSaveRequest(updated, false)).toBeNull();
  });

  it("retains a dirty draft and requires explicit intent to replace the newer text", () => {
    const before = { base: "Before", text: "My draft", saved: "Before" };
    expect(noteSaveRequest(before, false)).toEqual({ expectedNote: "Before", note: "My draft" });
    const conflicted = observeSavedNote(before, "Reviewer's edit\n ");
    expect(conflicted.text).toBe("My draft");
    expect(conflicted.base).toBe("Before");
    expect(noteSaveRequest(conflicted, false)).toBeNull();
    expect(noteSaveRequest(conflicted, true)).toEqual({
      expectedNote: "Reviewer's edit\n ", note: "My draft",
    });
    expect(observeSavedNote(conflicted, "My draft")).toEqual({ base: "My draft", text: "My draft", saved: "My draft" });
  });

  it("sends the captured workspace, list, cue and exact starting text", async () => {
    invoke.mockClear();
    await setCueNotes("opened-show", "notes-list", "cue", "Starting\r\n ", "Draft");
    expect(invoke).toHaveBeenCalledExactlyOnceWith("set_cue_notes", {
      workspaceId: "opened-show", listId: "notes-list", cueId: "cue",
      expectedNote: "Starting\r\n ", note: "Draft",
    });
  });
});
