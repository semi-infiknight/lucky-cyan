// Zone (c) — command wrapper argument-shape contract.
//
// Mocks the Tauri invoke bridge and asserts each wrapper forwards the exact
// command name and argument object the Rust side expects — including the
// non-obvious normalisation logic (default positions, null coalescing, rounding)
// that could silently drift and send a malformed payload.

import { describe, it, expect, beforeEach, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

import * as cmd from "../commands";

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
});

describe("command wrappers forward the correct name + args", () => {
  it("auxiliary cart commands bind at explicit open and address a pad by ID", async () => {
    await cmd.openCartWindow("cart-uuid");
    await cmd.readCartWindow();
    await cmd.cartWindowPad("pad-uuid", "fire");
    await cmd.cartWindowPad("pad-uuid", "stop");
    expect(invokeMock.mock.calls).toEqual([
      ["open_cart_window", { listId: "cart-uuid" }],
      ["read_cart_window"],
      ["cart_window_pad", { cueId: "pad-uuid", action: "fire" }],
      ["cart_window_pad", { cueId: "pad-uuid", action: "stop" }],
    ]);
  });

  it("goCue → go_cue { cueId }", async () => {
    await cmd.goCue("cue-1");
    expect(invokeMock).toHaveBeenCalledWith("go_cue", { cueId: "cue-1" });
  });

  it("saves a recorded cue key against its displayed owner and prior binding", async () => {
    await cmd.setCueHotkey("show", "list", "cue", "Shift+J", "4");
    expect(invokeMock).toHaveBeenCalledWith("set_cue_hotkey", {
      workspaceId: "show", listId: "list", cueId: "cue", expectedKey: "Shift+J", key: "4",
    });
  });

  it("saves or opens a template against the exact desk identity", async () => {
    const path = "/shows/starter.inkue-template";
    const owner = { instance_id: "show", revision: 3, file_path: "/shows/current.inkue" };
    await cmd.saveWorkspaceTemplate(path, owner);
    await cmd.newWorkspaceFromTemplate(path, owner);
    expect(invokeMock.mock.calls.slice(-2)).toEqual([
      ["save_workspace_template", { path, owner }],
      ["new_workspace_from_template", { path, owner }],
    ]);
  });

  it("starts by default or explicitly blank, and keeps the template choice separate", async () => {
    const owner = { instance_id: "show", revision: 2, file_path: null };
    const path = "/shows/starter.inkue-template";
    await cmd.newWorkspace(owner);
    await cmd.newWorkspace(owner, true);
    await cmd.getDefaultWorkspaceTemplate();
    await cmd.setDefaultWorkspaceTemplate(path);
    await cmd.setDefaultWorkspaceTemplate(null);
    expect(invokeMock.mock.calls.slice(-5)).toEqual([
      ["new_workspace", { owner, blank: false }],
      ["new_workspace", { owner, blank: true }],
      ["get_default_workspace_template"],
      ["set_default_workspace_template", { path }],
      ["set_default_workspace_template", { path: null }],
    ]);
  });

  it("reads, exports and patches fixture profiles with the correct desk and definition", async () => {
    const profile = { name: "Pan pair", parameters: [
      { name: "Pan", kind: "pan", width: "Bit16", channel_offset: 1, default: .5 },
    ] } as const;
    const definition = { ...profile, parameters: [...profile.parameters] };
    const path = "/shows/pan.inkue-fixture";
    const edit = { workspace_id: "show", label: "Head", universe: 3, base_address: 48, profile: definition };
    await cmd.loadFixturePatch("show");
    await cmd.readFixtureProfile(path);
    await cmd.exportFixtureProfile(path, "show", "head-source", definition);
    await cmd.patchImportedFixture(edit);
    expect(invokeMock.mock.calls.slice(-4)).toEqual([
      ["load_fixture_patch", { workspaceId: "show" }],
      ["read_fixture_profile", { path }],
      ["export_fixture_profile", { path, workspaceId: "show", fixtureId: "head-source", expected: definition }],
      ["patch_imported_fixture", { edit }],
    ]);
  });

  it("applies a Playlist trigger against the displayed workspace, list and prior value", async () => {
    const expected = { workspace_id: "show", list_id: "list", trigger: "ignore" } as const;
    await cmd.setPlaylistSecondTrigger("playlist", expected, "previous");
    expect(invokeMock).toHaveBeenCalledWith("set_playlist_second_trigger", {
      groupId: "playlist", expected, trigger: "previous",
    });
  });

  it("applies Playlist shuffle with the displayed owner and prior value", async () => {
    const edit = {
      workspace_id: "show", list_id: "list", group_id: "playlist", expected: false, enabled: true,
    };
    await cmd.setPlaylistAutoShuffle(edit);
    expect(invokeMock).toHaveBeenCalledWith("set_playlist_auto_shuffle", { edit });
  });

  it("seekCue → seek_cue { cueId, positionMs }", async () => {
    await cmd.seekCue("cue-1", 1234);
    expect(invokeMock).toHaveBeenCalledWith("seek_cue", { cueId: "cue-1", positionMs: 1234 });
  });

  it("addCue defaults position to -1 (append)", async () => {
    await cmd.addCue("audio");
    expect(invokeMock).toHaveBeenCalledWith("add_cue", { cueType: "audio", position: -1 });
  });

  it("moveCues forwards a null beforeId verbatim", async () => {
    await cmd.moveCues(["a", "b"], null);
    expect(invokeMock).toHaveBeenCalledWith("move_cues", { ids: ["a", "b"], beforeId: null });
  });

  it("pasteCue with no arg coalesces to afterCueId: null", async () => {
    await cmd.pasteCue();
    expect(invokeMock).toHaveBeenCalledWith("paste_cue", { afterCueId: null });
  });

  it("updateCue nests the partial properties object", async () => {
    await cmd.updateCue("cue-1", { volume_db: -6, pan: 0.5 } as never);
    expect(invokeMock).toHaveBeenCalledWith("update_cue", {
      cueId: "cue-1",
      properties: { volume_db: -6, pan: 0.5 },
    });
  });

  it("previewCue rounds fractional millisecond markers to integers", async () => {
    await cmd.previewCue("cue-1", "audition-1", "pw:phones", -18, 12.7, 40.2);
    expect(invokeMock).toHaveBeenCalledWith("preview_cue", {
      cueId: "cue-1", requestId: "audition-1", deviceId: "pw:phones", monitorDb: -18,
      startMs: 13,
      endMs: 40,
    });
  });

  it("addresses a saturation preview by the displayed playback and cancels only its token", async () => {
    const request = {
      workspace_id: "workspace", list_id: "list", fade_id: "fade", target_id: "image", voice_id: "run", saturation: 0,
    };
    invokeMock.mockResolvedValueOnce(["preview-token", 2000]);
    await expect(cmd.previewPictureSaturation(request)).resolves.toEqual(["preview-token", 2000]);
    await cmd.cancelPictureSaturationPreview("run", "preview-token");
    expect(invokeMock.mock.calls).toEqual([
      ["preview_picture_saturation", { request }],
      ["cancel_picture_saturation_preview", { voiceId: "run", token: "preview-token" }],
    ]);
  });

  it("previewCue passes null markers when omitted", async () => {
    await cmd.previewCue("cue-1", "audition-1", "pw:phones", -18);
    expect(invokeMock).toHaveBeenCalledWith("preview_cue", {
      cueId: "cue-1", requestId: "audition-1", deviceId: "pw:phones", monitorDb: -18,
      startMs: null,
      endMs: null,
    });
  });

  it("previewCue requests cue effects only for a wet audition", async () => {
    await cmd.previewCue("cue-1", "audition-1", "pw:phones", -18, undefined, undefined, true);
    expect(invokeMock).toHaveBeenCalledWith("preview_cue", {
      cueId: "cue-1", requestId: "audition-1", deviceId: "pw:phones", monitorDb: -18,
      startMs: null,
      endMs: null,
      audioFx: true,
    });
  });

  it("listAudioDevices coalesces an omitted backend to null", async () => {
    invokeMock.mockResolvedValue([]);
    await cmd.listAudioDevices();
    expect(invokeMock).toHaveBeenCalledWith("list_audio_devices", { backend: null });
  });

  it("setAudioFile → set_audio_file { cueId, filePath }", async () => {
    await cmd.setAudioFile("cue-1", "audio/track.wav");
    expect(invokeMock).toHaveBeenCalledWith("set_audio_file", {
      cueId: "cue-1",
      filePath: "audio/track.wav",
    });
  });

  it("setOutputPatch forwards the full patch definition", async () => {
    invokeMock.mockResolvedValue("patch-id");
    await cmd.setOutputPatch(null, "Main", "dev-1", [0, 1]);
    expect(invokeMock).toHaveBeenCalledWith("set_output_patch", {
      patchId: null,
      name: "Main",
      deviceId: "dev-1",
      channels: [0, 1],
    });
  });

  it("sends names in logical output order independently of hardware order", async () => {
    await cmd.setOutputPatch("patch-id", "Monitors", "dev-1", [7, 6], ["Stage", "Booth"]);
    expect(invokeMock).toHaveBeenCalledWith("set_output_patch", {
      patchId: "patch-id", name: "Monitors", deviceId: "dev-1", channels: [7, 6], channelNames: ["Stage", "Booth"],
    });
  });
});
