import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

let cleanup: (() => void) | undefined;
vi.mock("react", () => ({
  useRef: (initial: unknown) => ({ current: initial }),
  useEffect: (setup: () => () => void) => { cleanup = setup(); },
}));
vi.mock("../stores/workspaceStore", () => ({
  useWorkspaceStore: Object.assign(
    () => ({ selectedCueId: "1", generalPrefs: { double_go_protection_ms: 0 } }),
    { getState: () => ({
      cues: [
        { id: "7", hotkey: "J", cue_list_owner: { workspace_id: "show", list_id: "list" } },
        { id: "8", hotkey: "Shift+J", cue_list_owner: { workspace_id: "show", list_id: "list" } },
        { id: "9", hotkey: "4", cue_list_owner: { workspace_id: "show", list_id: "list" } },
        { id: "10", hotkey: "5", cue_list_owner: { workspace_id: "replaced", list_id: "list" } },
        { id: "11", hotkey: "6", cue_list_owner: { workspace_id: "show", list_id: "previous-list" } },
      ],
      activeCueListId: "list", workspaceInfo: { instance_id: "show" },
    }) },
  ),
}));
vi.mock("../lib/cueOperations", () => ({ pasteAfterSelection: vi.fn(async () => {}) }));
vi.mock("../lib/commands", () => ({
  go: vi.fn(async () => {}),
  fireCueHotkey: vi.fn(async () => {}),
  stopAll: vi.fn(async () => {}),
  hardStopAll: vi.fn(async () => {}),
}));

import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { fireCueHotkey, go, hardStopAll, stopAll } from "../lib/commands";
import { pasteAfterSelection } from "../lib/cueOperations";

describe("playback keyboard dispatch", () => {
  let listener: (event: KeyboardEvent) => Promise<void>;
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("window", {
      addEventListener: (_: string, fn: typeof listener) => { listener = fn; },
      removeEventListener: vi.fn(),
    });
    useKeyboardShortcuts(vi.fn(), undefined, undefined, undefined, undefined, undefined,
      undefined, undefined, undefined, true);
  });
  afterEach(() => { cleanup?.(); vi.unstubAllGlobals(); });

  function key(key: string, tag = "MAIN", repeat = false) {
    const preventDefault = vi.fn();
    const handled = listener({
      key, repeat, preventDefault,
      target: {
        tagName: tag,
        closest: (selector: string) => selector.split(", ").includes(tag.toLowerCase()) ? {} : null,
      },
    } as unknown as KeyboardEvent);
    return { preventDefault, handled };
  }

  it("dispatches exactly the assigned cue when a trusted desk key is pressed", async () => {
    const focus = vi.fn(() => true);
    const overlay = vi.fn((): object | null => null);
    vi.stubGlobal("document", { hasFocus: focus, querySelector: overlay });
    const press = (overrides = {}, editable = false, desk = true) => listener({
      key: "j", isTrusted: true, preventDefault: vi.fn(),
      target: { tagName: "DIV", isContentEditable: editable,
        closest: (s: string) => desk && s === "[data-cue-hotkeys]" ? {} : null },
      ...overrides,
    } as unknown as KeyboardEvent);
    await press();
    await press({ key: "J", shiftKey: true });
    await press({ key: "4", code: "Numpad4" });
    expect(vi.mocked(fireCueHotkey).mock.calls).toEqual([
      ["show", "list", "7", "J"], ["show", "list", "8", "Shift+J"], ["show", "list", "9", "4"],
    ]);
    for (const flags of [{ repeat: true }, { isTrusted: false }, { isComposing: true },
      { ctrlKey: true }, { altKey: true }, { metaKey: true }, { key: "Shift" }, { key: "k" },
      { key: "5" }, { key: "6" }, { key: "4", shiftKey: true }, { key: "4", repeat: true }]) {
      await press(flags);
    }
    await press({}, true);
    await press({}, false, false);
    focus.mockReturnValue(false);
    await press();
    focus.mockReturnValue(true);
    overlay.mockReturnValue({});
    await press();
    expect(fireCueHotkey).toHaveBeenCalledTimes(3);
  });

  it("opens cue help once from a programming field and keeps Show Mode clear", async () => {
    const help = vi.fn();
    const bind = (showMode: boolean, modal: boolean) => useKeyboardShortcuts(
      vi.fn(), undefined, undefined, undefined, undefined, undefined,
      undefined, undefined, undefined, showMode, modal, undefined, help,
    );
    bind(false, false);
    await key("F1", "INPUT").handled;
    await key("F1", "INPUT", true).handled;
    expect(help).toHaveBeenCalledOnce();
    bind(true, false);
    await key("F1").handled;
    bind(false, true);
    await key("F1").handled;
    expect(help).toHaveBeenCalledOnce();
    expect(go).not.toHaveBeenCalled();
  });

  it("opens property paste without duplicating the copied cue", async () => {
    const preview = vi.fn();
    const refresh = vi.fn();
    useKeyboardShortcuts(refresh, undefined, undefined, undefined, undefined, undefined,
      undefined, undefined, undefined, false, false, undefined, undefined, preview);
    const press = (shiftKey: boolean, repeat = false) => listener({
      key: shiftKey ? "V" : "v", ctrlKey: true, shiftKey, repeat, preventDefault: vi.fn(),
      target: { tagName: "DIV", closest: () => null },
    } as unknown as KeyboardEvent);
    await press(true);
    await press(true, true);
    expect(preview).toHaveBeenCalledOnce();
    expect(pasteAfterSelection).not.toHaveBeenCalled();
    await press(false);
    expect(pasteAfterSelection).toHaveBeenCalledWith(refresh);
  });

  it("fires one GO from the desk while Space is held", async () => {
    await key(" ").handled;
    await key(" ", "MAIN", true).handled;
    expect(go).toHaveBeenCalledTimes(1);
  });

  it.each(["BUTTON", "SELECT"])("preserves native Space activation on a focused %s", async tag => {
    const event = key(" ", tag);
    await event.handled;
    expect(event.preventDefault).not.toHaveBeenCalled();
    expect(go).not.toHaveBeenCalled();
  });

  it("permits native button activation once and cancels repeated Enter", async () => {
    const first = key("Enter", "BUTTON");
    await first.handled;
    expect(first.preventDefault).not.toHaveBeenCalled();
    const repeat = key("Enter", "BUTTON", true);
    await repeat.handled;
    expect(repeat.preventDefault).toHaveBeenCalledOnce();
  });

  it("keeps a held Escape at normal stop until the next distinct press", async () => {
    const first = key("Escape").handled;
    await key("Escape", "MAIN", true).handled;
    expect(stopAll).toHaveBeenCalledOnce();
    expect(hardStopAll).not.toHaveBeenCalled();
    await key("Escape").handled;
    await first;
    expect(hardStopAll).toHaveBeenCalledOnce();
  });
});
