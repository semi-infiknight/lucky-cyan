import { beforeEach, expect, it, vi } from "vitest";
import type { CueValidation } from "../../lib/types";
import { checkWorkspace } from "../../lib/commands";
import { useWorkspaceStore } from "../workspaceStore";

vi.mock("../../lib/commands", () => ({ checkWorkspace: vi.fn() }));
const scanner = vi.mocked(checkWorkspace);
const state = useWorkspaceStore.getState;

function openShow(id: string) {
  useWorkspaceStore.setState({
    workspaceInfo: { name: id, instance_id: id, file_path: null, revision: 0, is_modified: false },
  });
}
function deferredCheck() {
  let resolve!: (rows: CueValidation[]) => void;
  const promise = new Promise<CueValidation[]>(complete => { resolve = complete; });
  scanner.mockReturnValueOnce(promise);
  return resolve;
}
function problem(id: string): CueValidation {
  return {
    owner: { instance_id: id, revision: 0, file_path: null }, cue_type: "audio",
    cue_id: "cue", cue_number: "1", cue_name: "music", missing_file: "music.wav",
    issues: [{ severity: "error", message: "File not found" }],
  };
}

beforeEach(() => {
  scanner.mockReset();
  openShow("first");
});

it("coalesces repeated checks while a stat waits, then applies the trailing result", async () => {
  const release = deferredCheck();
  scanner.mockResolvedValueOnce([]);
  const done = state().refreshValidation();
  for (let i = 0; i < 10; i++) expect(state().refreshValidation()).toBe(done);
  expect(scanner).toHaveBeenCalledTimes(1);
  expect(state().validationPending).toBe(true);
  release([problem("first")]);
  await done;
  expect(scanner).toHaveBeenCalledTimes(2);
  expect(state().validation).toEqual([]);
  expect(state().validationPending).toBe(false);
});

it("checks a new show while an old share is blocked and ignores the superseded reply", async () => {
  const release = deferredCheck();
  const old = state().refreshValidation();
  openShow("second");
  scanner.mockRejectedValueOnce(new Error("The show changed"));
  await state().refreshValidation();
  release([problem("first")]);
  await old;
  expect(state().validation).toEqual([]);
  expect(state().validationError).toContain("show changed");
  expect(state().validationPending).toBe(false);
});
