// Typed wrappers around Tauri invoke() calls.
// All backend communication goes through this file.

import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  CartCell,
  CartWindowSnapshot,
  CartEdit,
  AppPreferences,
  AudioCueData,
  AudioMap,
  AudioMapLibrary,
  AudioMapPatch,
  AudioMark,
  AudioPreferences,
  CameraDeviceInfo,
  PlaylistSecondTrigger,
  PlaylistTriggerSetting,
  CollectReport,
  CueColor,
  CueValidation,
  MachineAudioConfig,
  CueId,
  CueEffect,
  CueListMode,
  CueListSummary,
  CueMonitorSnapshot,
  CueReviewHistory,
  CueSummary,
  CueType,
  DeviceInfo,
  DisplayPreferences,
  DmxUniverseSnapshot,
  FixtureConflict,
  FixtureColorChannels,
  FixtureColorModel,
  FixtureGroup,
  FixtureType,
  GeneralPreferences,
  CueListTcConfig,
  GroupMode,
  HealthAlert,
  ImportReport,
  InputPatch,
  LogLine,
  MidiTrigger,
  MscMessage,
  MidiTriggerConfig,
  OscPatch,
  TcMachineConfig,
  TcPosition,
  TcTrigger,
  OscListenerStatus,
  OscReceiveConfig,
  OutputPatch,
  PointMix,
  OutputTransform,
  ParamTarget,
  PatchedChannel,
  PatchedFixture,
  RecoveryInfo,
  RelinkResult,
  ScreenInfo,
  VideoOutputInfo,
  StageSnapshot,
  TestPattern,
  UniverseOutput,
  VideoCueData,
  WaveformData,
  DefaultWorkspaceTemplate,
  WorkspaceInfo,
  WallClockTrigger,
} from "./types";

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

export const go = () => invoke<void>("go");
export const fireCueHotkey = (workspaceId: string, listId: string, cueId: CueId, key: string) =>
  invoke<void>("fire_cue_hotkey", { workspaceId, listId, cueId, key });
export const setCueHotkey = (
  workspaceId: string, listId: string, cueId: CueId, expectedKey: string | null, key: string | null,
) => invoke<void>("set_cue_hotkey", { workspaceId, listId, cueId, expectedKey, key });
export const setWallClockTrigger = (workspaceId: string, listId: string, cueId: CueId, trigger: WallClockTrigger | null) =>
  invoke<void>("set_wall_clock_trigger", { workspaceId, listId, cueId, trigger });
export const armWallClockTrigger = (workspaceId: string, listId: string, cueId: CueId, armed: boolean) =>
  invoke<void>("arm_wall_clock_trigger", { workspaceId, listId, cueId, armed });
export const goCue = (cueId: CueId) => invoke<void>("go_cue", { cueId });
export const setMasterVolume = (db: number) => invoke<void>("set_master_volume", { db });
export const stopAll = () => invoke<void>("stop_all");
export const hardStopBlackout = (workspaceId: string) => invoke<void>("hard_stop_blackout", { workspaceId });
export const hardStopAll = () => invoke<void>("hard_stop_all");
export const stopCue = (cueId: CueId) => invoke<void>("stop_cue", { cueId });
export const pauseCue = (cueId: CueId) => invoke<void>("pause_cue", { cueId });
export const seekCue = (cueId: CueId, positionMs: number) =>
  invoke<void>("seek_cue", { cueId, positionMs });
export const loadAudioToTime = (workspaceId: string, listId: string, cueId: CueId, offsetMs: number | null) =>
  invoke<void>("load_audio_to_time", { workspaceId, listId, cueId, offsetMs });
export const resumeCue = (cueId: CueId) =>
  invoke<void>("resume_cue", { cueId });

// ---------------------------------------------------------------------------
// Cue management
// ---------------------------------------------------------------------------

export const getAllCues = () => invoke<CueSummary[]>("get_all_cues");
export const setCueFlag = (workspaceId: string, listId: string, cueId: CueId, flagged: boolean) =>
  invoke<void>("set_cue_flag", { workspaceId, listId, cueId, flagged });
export const setSecondCueColor = (workspaceId: string, listId: string, cueId: CueId, secondColor: CueColor | null) =>
  invoke<void>("set_second_cue_color", { workspaceId, listId, cueId, secondColor });
export const resetCueColors = (workspaceId: string) =>
  invoke<void>("reset_cue_colors", { workspaceId });
export const getCue = (cueId: CueId) =>
  invoke<AudioCueData | VideoCueData>("get_cue", { cueId });
export const findCues = (workspaceId: string, listId: string, query: string) =>
  invoke<CueId[]>("find_cues", { workspaceId, listId, query });
export const revertPictureFade = (workspaceId: string, listId: string, cueId: CueId) =>
  invoke<{ geometry: boolean; saturation: boolean }>("revert_picture_fade", { workspaceId, listId, cueId });
export const previewPictureSaturation = (request: {
  workspace_id: string; list_id: string; fade_id: CueId; target_id: CueId; voice_id: string; saturation: number;
}) => invoke<[token: string, durationMs: number]>("preview_picture_saturation", { request });
export const cancelPictureSaturationPreview = (voiceId: string, token: string) =>
  invoke<void>("cancel_picture_saturation_preview", { voiceId, token });
export const addCue = (cueType: CueType, position = -1, cartDrop?: { list_id: string; cell: CartCell }) =>
  invoke<CueId>("add_cue", cartDrop ? { cueType, position, cartDrop } : { cueType, position });
export const removeCue = (workspaceId: string, listId: string, cueId: CueId) =>
  invoke<void>("remove_cue", { workspaceId, listId, cueId });
export const removeCues = (workspaceId: string, listId: string, ids: CueId[]) =>
  invoke<void>("remove_cues", { workspaceId, listId, ids });
export const moveCue = (cueId: CueId, newPosition: number) =>
  invoke<void>("move_cue", { cueId, newPosition });
export const moveCues = (ids: CueId[], beforeId: CueId | null) =>
  invoke<void>("move_cues", { ids, beforeId });
export const renumberCues = () =>
  invoke<void>("renumber_cues");
export interface RenumberSelectionPreview {
  workspace_id: string;
  cue_list_id: string;
  start: number;
  increment: number;
  changes: { cue_id: CueId; previous: string | null; number: string }[];
}
export const previewRenumberSelectedCues = (
  workspaceId: string, cueListId: string, ids: CueId[], start: number, increment: number,
) => invoke<RenumberSelectionPreview>("preview_renumber_selected_cues", { workspaceId, cueListId, ids, start, increment });
export const renumberSelectedCues = (preview: RenumberSelectionPreview) =>
  invoke<void>("renumber_selected_cues", { preview });
export const clearCueNumbers = () =>
  invoke<void>("clear_cue_numbers");

// Live level edits — engine only, no workspace write and no undo entry. Used
// while a slider or a matrix cell is being dragged; the value is persisted
// once on release with the ordinary updateCue.
export const setLiveLevel = (cueId: CueId, volumeDb?: number, pan?: number) =>
  invoke<void>("set_live_level", { cueId, volumeDb, pan });
export const setLiveCrosspoint = (cueId: CueId, input: number, output: number, gainDb: number) =>
  invoke<void>("set_live_crosspoint", { cueId, input, output, gainDb });

export const groupCues = (ids: CueId[]) =>
  invoke<CueId>("group_cues", { ids });
export const ungroup = (groupId: CueId) =>
  invoke<void>("ungroup", { groupId });
export const setGroupMode = (groupId: CueId, mode: GroupMode) =>
  invoke<void>("set_group_mode", { groupId, mode });
export interface GroupChildEntranceEdit {
  workspace_id: string;
  list_id: CueId;
  group_id: CueId;
  child_id: CueId;
  expected_pre_wait_ms: number;
  pre_wait_ms: number;
}
export const setGroupChildEntrance = (edit: GroupChildEntranceEdit) =>
  invoke<boolean>("set_group_child_entrance", { edit });
export const setPlaylistLoop = (groupId: CueId, loopOn: boolean) =>
  invoke<void>("set_playlist_loop", { groupId, loopOn });
export const setPlaylistAdvancePlayhead = (groupId: CueId, advance: boolean) =>
  invoke<void>("set_playlist_advance_playhead", { groupId, advance });
export const setPlaylistSecondTrigger = (
  groupId: CueId, expected: PlaylistTriggerSetting, trigger: PlaylistSecondTrigger,
) => invoke<void>("set_playlist_second_trigger", { groupId, expected, trigger });
export interface PlaylistShuffleEdit {
  workspace_id: string;
  list_id: CueId;
  group_id: CueId;
  expected: boolean;
  enabled: boolean;
}
export const setPlaylistAutoShuffle = (edit: PlaylistShuffleEdit) =>
  invoke<void>("set_playlist_auto_shuffle", { edit });
export const addCueToGroup = (cueId: CueId, groupId: CueId, position = -1) =>
  invoke<void>("add_cue_to_group", { cueId, groupId, position });
export const removeCuesFromGroup = (workspaceId: string, listId: string, groupId: CueId, cueIds: CueId[]) =>
  invoke<void>("remove_cue_from_group", { workspaceId, listId, groupId, cueIds });
export const moveToTopLevel = (cueId: CueId, beforeId: CueId | null) =>
  invoke<void>("move_to_top_level", { cueId, beforeId });
export const duplicateCue = (cueId: CueId) =>
  invoke<CueId>("duplicate_cue", { cueId });
export const duplicateCues = (ids: CueId[]) =>
  invoke<CueId[]>("duplicate_cues", { ids });

// ---------------------------------------------------------------------------
// Undo / Redo / Copy / Paste
// ---------------------------------------------------------------------------

export const undo = () => invoke<void>("undo");
export const redo = () => invoke<void>("redo");
export const canUndo = () => invoke<boolean>("can_undo");
export const canRedo = () => invoke<boolean>("can_redo");
export const copyCue = (cueId: CueId) => invoke<void>("copy_cue", { cueId });
export const pasteCue = (afterCueId?: CueId | null) =>
  invoke<CueId>("paste_cue", { afterCueId: afterCueId ?? null });

export type CuePropertyGroup = "color" | "waits" | "audio_level" | "audio_matrix" | "audio_fades" | "audio_clip_window" | "pan" | "rate"
  | "picture_geometry" | "picture_layer" | "picture_fades" | "fade_duration" | "fade_curve_shapes";

export interface CuePropertiesPreview {
  workspace_id: string;
  cue_list_id: string;
  clipboard_revision: string;
  source_name: string;
  source_number: string | null;
  groups: CuePropertyGroup[];
}

export const previewPasteCueProperties = (workspaceId: string, cueListId: string, targets: CueId[]) =>
  invoke<CuePropertiesPreview>("preview_paste_cue_properties", { workspaceId, cueListId, targets });

export const pasteCueProperties = (preview: CuePropertiesPreview, targets: CueId[], groups: CuePropertyGroup[]) =>
  invoke<number>("paste_cue_properties", { request: {
    workspace_id: preview.workspace_id, cue_list_id: preview.cue_list_id,
    clipboard_revision: preview.clipboard_revision, targets, groups,
  } });
/** Properties merged into a cue. Deliberately loose: the backend merges this
 *  JSON into the cue's serialised form and rebuilds it, so every cue type's
 *  fields are valid here — typing it as one concrete cue was always a fiction. */
export type CueProperties = Record<string, unknown>;

export const updateCue = (cueId: CueId, properties: CueProperties) =>
  invoke<void>("update_cue", { cueId, properties });
export const setCueNotes = (workspaceId: string, listId: string, cueId: CueId, expectedNote: string, note: string) =>
  invoke<{ status: "saved"; note: string; changed: boolean } | { status: "conflict"; note: string }>(
    "set_cue_notes", { workspaceId, listId, cueId, expectedNote, note },
  );

/** One cue's share of a batch edit. */
export interface CueEdit {
  cue_id: CueId;
  properties: CueProperties;
}

/** Edit many cues in one undo step; keys a cue does not have are skipped.
 *  Resolves to the number of cues that changed. */
export const updateCues = (edits: CueEdit[]) =>
  invoke<number>("update_cues", { edits });

/** Create a cue (Fade, Stop, Devamp, Start…) aimed at `targetIds`, inserted
 *  after the last target. Resolves to the new cue's id. */
export const addTargetingCue = (cueType: CueType, targetIds: CueId[], properties: CueProperties = {}) =>
  invoke<CueId>("add_targeting_cue", { cueType, targetIds, properties });
export const setAudioFile = (cueId: CueId, filePath: string) =>
  invoke<void>("set_audio_file", { cueId, filePath });
export const setVideoFile = (cueId: CueId, filePath: string) =>
  invoke<void>("set_video_file", { cueId, filePath });
export const getWaveformPeaks = (cueId: CueId, bins: number) =>
  invoke<WaveformData>("get_waveform_peaks", { cueId, bins });
export const getNormalizeDb = (cueId: CueId) =>
  invoke<number>("get_normalize_db", { cueId });
export const getMediaThumbnail = (path: string, seekInto: boolean) =>
  invoke<string>("get_media_thumbnail", { path, seekInto });
export const getVideoFilmstrip = (path: string, tiles: number, tileWidth: number) =>
  invoke<string[]>("get_video_filmstrip", { path, tiles, tileWidth });
export const getVideoFilmstripRange = (
  path: string, startS: number, endS: number, tiles: number, tileWidth: number,
) =>
  invoke<string[]>("get_video_filmstrip_range", { path, startS, endS, tiles, tileWidth });
export const listVideoScreens = () => invoke<ScreenInfo[]>("list_video_screens");
export const listCameraDevices = () =>
  invoke<CameraDeviceInfo[]>("list_camera_devices");
export const identifyOutputScreen = (screenIndex: number | null, outputId?: string | null) =>
  invoke<void>("identify_output_screen", { screenIndex, outputId: outputId ?? null });
export interface OutputMaskImagePreview {
  name: string;
  png: string;
  width: number;
  height: number;
}
export const getOutputMaskImage = (outputId: string | null) =>
  invoke<{document: string; image: OutputMaskImagePreview | null}>("get_output_mask_image", { outputId });
export const setOutputMaskImage = (outputId: string | null, document: string, path: string | null) =>
  invoke<OutputMaskImagePreview | null>("set_output_mask_image", { outputId, document, path });

export const getOutputTransform = (outputId?: string | null) =>
  invoke<OutputTransform>("get_output_transform", { outputId: outputId ?? null });
export const setOutputTransform = (transform: OutputTransform, outputId?: string | null, expectedRegions?: OutputTransform["regions"]) =>
  invoke<void>("set_output_transform", { transform, outputId: outputId ?? null, expectedRegions: expectedRegions ?? null });
export const showTestPattern = (pattern: TestPattern, outputId?: string | null) =>
  invoke<void>("show_test_pattern", { pattern, outputId: outputId ?? null });
export const clearTestPattern = (outputId?: string | null) =>
  invoke<void>("clear_test_pattern", { outputId: outputId ?? null });

// ---------------------------------------------------------------------------
// Video outputs
// ---------------------------------------------------------------------------

export const listVideoOutputs = () => invoke<VideoOutputInfo[]>("list_video_outputs");
export const captureStageSnapshot = (workspaceId: string, outputId: string) =>
  invoke<StageSnapshot>("capture_stage_snapshot", { workspaceId, outputId });
export type OutputSplitDirection = "horizontal" | "vertical";
export interface OutputSplitSetup {
  document: string;
  outputs: VideoOutputInfo[];
}
export const getOutputSplitSetup = () => invoke<OutputSplitSetup>("get_output_split_setup");
export const applyOutputSplit = (document: string, outputs: [VideoOutputInfo, VideoOutputInfo], direction: OutputSplitDirection, overlap: number, seamGamma?: number) =>
  invoke<VideoOutputInfo[]>("apply_output_split", {
    document, sides: outputs.map(output => ({ id: output.id, expected_regions: output.transform.regions ?? [] })), direction, overlap, seamGamma: seamGamma ?? null,
  });
export const exportProjectionChart = (path: string, direction: OutputSplitDirection, overlap: number) =>
  invoke<void>("export_projection_chart", { path, direction, overlap });
export const addVideoOutput = (name: string) =>
  invoke<VideoOutputInfo>("add_video_output", { name });
export const renameVideoOutput = (outputId: string, name: string) =>
  invoke<void>("rename_video_output", { outputId, name });
/** Put an output on a monitor (null = floating window). Omit the id for the main output. */
export const setVideoOutputScreen = (outputId: string | null, screen: number | null) =>
  invoke<void>("set_video_output_screen", { outputId, screen });
export const deleteVideoOutput = (outputId: string) =>
  invoke<void>("delete_video_output", { outputId });
export const listSystemFonts  = () => invoke<string[]>("list_system_fonts");
export const previewOutputTimer = (
  font: string, fontSize: number, position: string, margin: number, text: string | null,
) => invoke<void>("preview_output_timer", { font, fontSize: fontSize, position, margin, text });
export const setImageFile = (cueId: CueId, filePath: string) =>
  invoke<void>("set_image_file", { cueId, filePath });
export const setMidiFile = (cueId: CueId, filePath: string) =>
  invoke<void>("set_midi_file", { cueId, filePath });

export const previewCue = (
  cueId: CueId, requestId: string, deviceId: string, monitorDb: number, startMs?: number, endMs?: number,
  audioFx = false,
) =>
  invoke<string>("preview_cue", {
    cueId, requestId, deviceId, monitorDb,
    startMs: startMs != null ? Math.round(startMs) : null,
    endMs: endMs != null ? Math.round(endMs) : null,
    ...(audioFx ? { audioFx } : {}),
  });

export const auditionRunning = (voiceId: string) =>
  invoke<boolean>("audition_running", { voiceId });

export const stopPreview = (voiceId: string) =>
  invoke<void>("stop_preview", { voiceId });

// ---------------------------------------------------------------------------
// Playhead
// ---------------------------------------------------------------------------

export const setPlayhead = (cueId: CueId | null) =>
  invoke<void>("set_playhead", { cueId });
export const goToCueNumber = (workspaceId: string, listId: string, number: string) =>
  invoke<CueId>("go_to_cue_number", { workspaceId, listId, number });
export const getPlayhead = () => invoke<CueId | null>("get_playhead");

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

type WorkspaceFileOwner = Pick<WorkspaceInfo, "instance_id" | "revision" | "file_path">;

export const newWorkspace = (owner: WorkspaceFileOwner, blank = false) =>
  invoke<void>("new_workspace", { owner, blank });
export const saveWorkspace = (path: string, owner: WorkspaceFileOwner) =>
  invoke<void>("save_workspace", { path, owner });
export const saveWorkspaceTemplate = (path: string, owner: WorkspaceFileOwner) =>
  invoke<void>("save_workspace_template", { path, owner });
export const newWorkspaceFromTemplate = (path: string, owner: WorkspaceFileOwner) =>
  invoke<void>("new_workspace_from_template", { path, owner });
export const getDefaultWorkspaceTemplate = () =>
  invoke<DefaultWorkspaceTemplate | null>("get_default_workspace_template");
export const setDefaultWorkspaceTemplate = (path: string | null) =>
  invoke<DefaultWorkspaceTemplate | null>("set_default_workspace_template", { path });
export const loadWorkspace = (path: string) =>
  invoke<void>("load_workspace", { path });
export interface AudioSheet {
  title: string;
  cues: { number: string; name: string; media: string; level_db: number }[];
}

export const previewAudioSheet = (title: string, text: string) =>
  invoke<AudioSheet>("preview_audio_sheet", { title, text });

export const acceptAudioSheet = (sheet: AudioSheet, folder: string) =>
  invoke<void>("accept_audio_sheet", { sheet, folder });

export const takeLaunchDocument = () => invoke<string | null>("take_launch_document");

export const openOperatorRehearsal = () => invoke<void>("open_operator_rehearsal");

export const importQlabWorkspace = (path: string) =>
  invoke<ImportReport>("import_qlab_workspace", { path });
export const getWorkspaceInfo = () => invoke<WorkspaceInfo>("get_workspace_info");
export const resetSilentSpeed = (workspaceId: string, cueId: CueId) =>
  invoke<void>("reset_silent_speed", { workspaceId, cueId });
export const collectAndSave = (targetDir: string) =>
  invoke<CollectReport>("collect_and_save_workspace", { targetDir });

/** Returns snapshot metadata if a previous session ended without a clean exit. */
export const checkRecovery = () => invoke<RecoveryInfo | null>("check_recovery");
export const restoreRecovery = () => invoke<void>("restore_recovery");
export const discardRecovery = () => invoke<void>("discard_recovery");

// ---------------------------------------------------------------------------
// Preflight (Check Workspace) + relink
// ---------------------------------------------------------------------------

/** Validate the whole workspace; returns one entry per cue that has problems. */
export const checkWorkspace = () => invoke<CueValidation[]>("check_workspace");
/** Re-point a cue's missing media file (and any sibling missing files in the same folder). */
export const relinkMedia = (cueId: CueId, newPath: string, owner: CueValidation["owner"]) =>
  invoke<RelinkResult>("relink_media", { cueId, newPath, owner });

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

export const getRecentLogs = (limit?: number) =>
  invoke<LogLine[]>("get_recent_logs", { limit });
export const clearLogs = () => invoke<void>("clear_logs");
export const openLogsFolder = () => invoke<void>("open_logs_folder");

// ---------------------------------------------------------------------------
// Runtime health (device/network faults)
// ---------------------------------------------------------------------------

export const getHealthAlerts = () => invoke<HealthAlert[]>("get_health_alerts");
export const restoreAudioDevice = () => invoke<void>("restore_audio_device");

// ---------------------------------------------------------------------------
// Cue Lists
// ---------------------------------------------------------------------------

export const getCueLists = () => invoke<CueListSummary[]>("get_cue_lists");
export const addCueList = (name: string) =>
  invoke<string>("add_cue_list", { name });
export const removeCueList = (id: string) =>
  invoke<void>("remove_cue_list", { id });
export const renameCueList = (id: string, name: string) =>
  invoke<void>("rename_cue_list", { id, name });
export const setActiveCueList = (id: string) =>
  invoke<void>("set_active_cue_list", { id });
export const openCartWindow = (listId: string) => invoke<void>("open_cart_window", { listId });
export const readCartWindow = () => invoke<CartWindowSnapshot | null>("read_cart_window");
export const cartWindowPad = (cueId: CueId, action: "fire" | "stop") =>
  invoke<void>("cart_window_pad", { cueId, action });
export const editCartLayout = (listId: string, edit: CartEdit) =>
  invoke<void>("edit_cart_layout", { listId, edit });
export const setCueListMode = (id: string, mode: CueListMode, chainCartCues?: boolean) =>
  invoke<void>("set_cue_list_mode", { id, mode, chainCartCues });

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Preferences
// ---------------------------------------------------------------------------

export const getPreferences = () => invoke<AppPreferences>("get_preferences");
export const getAvailableBackends = () =>
  invoke<string[]>("get_available_backends");
export const getAsioOutputPairs = () =>
  invoke<number>("get_asio_output_pairs");
export const getMachineAudioConfig = () =>
  invoke<MachineAudioConfig>("get_machine_audio_config");
export const updateMachineAudioConfig = (config: MachineAudioConfig) =>
  invoke<void>("update_machine_audio_config", { config });
export const updateAudioPreferences = (prefs: AudioPreferences) =>
  invoke<void>("update_audio_preferences", { prefs });
export const updateGeneralPreferences = (prefs: GeneralPreferences) =>
  invoke<void>("update_general_preferences", { prefs });
export const updateDisplayPreferences = (prefs: DisplayPreferences) =>
  invoke<void>("update_display_preferences", { prefs });
export async function listAudioDevices(backend?: string): Promise<DeviceInfo[]> {
  return invoke<DeviceInfo[]>("list_audio_devices", { backend: backend ?? null });
}
export const testAudioDevice = (deviceId: string, backend: string) =>
  invoke<void>("test_audio_device", { deviceId, backend });
export const getOutputScreen = () =>
  invoke<number | null>("get_output_screen");
export const setOutputScreen = (screen: number | null) =>
  invoke<void>("set_output_screen", { screen });
export const toggleOutputWindow = () => invoke<void>("toggle_output_window");
export const getOutputWindowVisible = () => invoke<boolean>("get_output_window_visible");
export const openPreferencesWindow = () => invoke<void>("open_preferences_window");

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/** Devices an Output Patch may target. Pass the backend currently selected in
 *  Preferences to see its device universe before it has been applied. */
export const listOutputDevices = (backend?: string) =>
  invoke<DeviceInfo[]>("list_output_devices", { backend: backend ?? null });

/** Workspace Output Patch table + default patch id. */
export interface OutputPatchTable {
  patches: OutputPatch[];
  default_patch_id: string | null;
  workspace_id: string;
}

export const getOutputPatchTable = () =>
  invoke<OutputPatchTable>("get_output_patches");
/** Flat patch list — convenience for patch-selector dropdowns. */
export const getOutputPatches = () =>
  getOutputPatchTable().then((t) => t.patches);
export const setOutputPatch = (
  patchId: string | null,
  name: string,
  deviceId: string,
  channels: number[],
  channelNames?: string[],
) => invoke<string>("set_output_patch", { patchId, name, deviceId, channels, ...(channelNames === undefined ? {} : { channelNames }) });
export const removeOutputPatch = (patchId: string) =>
  invoke<void>("remove_output_patch", { patchId });
export const setOutputPatchGain = (patchId: string, gainDb: number) =>
  invoke<void>("set_output_patch_gain", { patchId, gainDb });
export const setOutputPatchRequireDevice = (workspaceId: string, patchId: string, requireNamedDevice: boolean) =>
  invoke<void>("set_output_patch_require_device", { workspaceId, patchId, requireNamedDevice });
export const setOutputPatchEffects = (workspaceId: string, patchId: string, audioEffects: CueEffect[]) =>
  invoke<void>("set_output_patch_effects", { workspaceId, patchId, audioEffects });
export const openMixerWindow = () => invoke<void>("open_mixer_window");
export const setDefaultOutputPatch = (patchId: string | null) =>
  invoke<void>("set_default_output_patch", { patchId });
export const refreshDevices = () => invoke<void>("refresh_devices");

// ---------------------------------------------------------------------------
// Timecode
// ---------------------------------------------------------------------------

export const listTcMidiInputPorts = () => invoke<string[]>("list_tc_midi_input_ports");
export const getTcTriggerOverride = () => invoke<boolean>("get_tc_trigger_override");
export const setTcTriggerOverride = (suspended: boolean) => invoke<void>("set_tc_trigger_override", { suspended });
export const getTcConfig = () => invoke<TcMachineConfig>("get_tc_config");
export const setTcConfig = (config: TcMachineConfig) => invoke<void>("set_tc_config", { config });
export const getTcPosition = () => invoke<TcPosition | null>("get_tc_position");
export const getCueTcTrigger = (cueId: string) => invoke<TcTrigger | null>("get_cue_tc_trigger", { cueId });
export const setCueTcTrigger = (
  cueId: string,
  positionStr: string | null,
  rateStr: string | null,
  realTime: boolean,
) => invoke<void>("set_cue_tc_trigger", { cueId, positionStr, rateStr, realTime });
export const getCuelistTcConfig = (workspaceId: string, listId: string) =>
  invoke<CueListTcConfig>("get_cuelist_tc_config", { workspaceId, listId });
export const setCuelistTcConfig = (workspaceId: string, listId: string, config: CueListTcConfig) =>
  invoke<void>("set_cuelist_tc_config", { workspaceId, listId, config });

// ---------------------------------------------------------------------------
// Audio inputs + Input Patches (Mic Cues)
// ---------------------------------------------------------------------------

export const listInputDevices = () => invoke<DeviceInfo[]>("list_input_devices");
export const listInputPatches = () => invoke<InputPatch[]>("list_input_patches");
export const addInputPatch = (name: string, deviceId: string, channels: number[]) =>
  invoke<InputPatch>("add_input_patch", { name, deviceId, channels });
export const updateInputPatch = (patch: InputPatch) =>
  invoke<void>("update_input_patch", { patch });
export const removeInputPatch = (patchId: string) =>
  invoke<void>("remove_input_patch", { patchId });

// ---------------------------------------------------------------------------
// OSC Patches
// ---------------------------------------------------------------------------

export const listOscPatches = () => invoke<OscPatch[]>("list_osc_patches");
export const addOscPatch = (name: string, ip: string, port: number) =>
  invoke<OscPatch>("add_osc_patch", { name, ip, port });
export const updateOscPatch = (patch: OscPatch) =>
  invoke<void>("update_osc_patch", { patch });
export const removeOscPatch = (patchId: string) =>
  invoke<void>("remove_osc_patch", { patchId });

// ---------------------------------------------------------------------------
// OSC Receive Config
// ---------------------------------------------------------------------------

export const getOscConfig = () => invoke<OscReceiveConfig>("get_osc_config");
export const getOscListenerStatus = (plainText = false) => invoke<OscListenerStatus>("get_osc_listener_status", { plainText });
export const retryOscListener = (plainText = false) => invoke<void>("retry_osc_listener", { plainText });
export const setOscConfig = (config: OscReceiveConfig) =>
  invoke<void>("set_osc_config", { config });
export const sendOscTest = (patchId: string, message: import("./types").OscMessage) =>
  invoke<string>("send_osc_test", { patchId, message });

// ---------------------------------------------------------------------------
// Network interface selection
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// External links (tauri-plugin-opener)
// ---------------------------------------------------------------------------

export const openExternalUrl = (url: string) => openUrl(url);

export const listNetworkInterfaces = () =>
  invoke<import("./types").NetworkInterfaceInfo[]>("list_network_interfaces");
export const getNetworkConfig = () =>
  invoke<import("./types").NetworkInterfaceConfig>("get_network_config");
export const setNetworkConfig = (config: import("./types").NetworkInterfaceConfig) =>
  invoke<void>("set_network_config", { config });

// ---------------------------------------------------------------------------
// MIDI
// ---------------------------------------------------------------------------

export const listMidiOutputPorts = () => invoke<string[]>("list_midi_output_ports");
export const sendMidiTest = (
  portName: string,
  messageType: string,
  channel: number,
  data1: number,
  data2: number,
  msc?: MscMessage,
  requireUniqueOutput?: boolean,
  sysex?: string,
) => invoke<void>("send_midi_test", { portName, messageType, channel, data1, data2, msc, requireUniqueOutput, sysex });

// Per-cue MIDI triggers
export const listMidiInputPorts = () => invoke<string[]>("list_midi_input_ports");
export const getMidiTriggerConfig = () =>
  invoke<MidiTriggerConfig>("get_midi_trigger_config");
export const setMidiTriggerConfig = (config: MidiTriggerConfig) =>
  invoke<void>("set_midi_trigger_config", { config });
export const getMidiTriggerOverride = () => invoke<boolean>("get_midi_trigger_override");
export const setMidiTriggerOverride = (suspended: boolean) =>
  invoke<void>("set_midi_trigger_override", { suspended });
export const getCueMidiTrigger = (cueId: CueId) =>
  invoke<MidiTrigger | null>("get_cue_midi_trigger", { cueId });
export const setCueMidiTrigger = (cueId: CueId, trigger: MidiTrigger | null) =>
  invoke<void>("set_cue_midi_trigger", { cueId, trigger });
export const learnMidiTrigger = () => invoke<MidiTrigger | null>("learn_midi_trigger");
export const clearMidiLearn = () => invoke<void>("clear_midi_learn");

// ---------------------------------------------------------------------------
// DMX / Lighting
// ---------------------------------------------------------------------------

export const dmxSetOutputs = (outputs: UniverseOutput[]) =>
  invoke<void>("dmx_set_outputs", { outputs });
export const dmxGetOutputs = () => invoke<UniverseOutput[]>("dmx_get_outputs");
export const dmxSetChannel = (workspaceId: string, universe: number, address: number, value: number) =>
  invoke<void>("dmx_set_channel", { workspaceId, universe, address, value });
export const dmxSetBlackout = (on: boolean) => invoke<void>("dmx_set_blackout", { on });
export const dmxGetBlackout = () => invoke<boolean>("dmx_get_blackout");
export const dmxGetSnapshot = () => invoke<DmxUniverseSnapshot[]>("dmx_get_snapshot");

// ---------------------------------------------------------------------------
// DMX / Fixtures
// ---------------------------------------------------------------------------

export const listBuiltinFixtureTypes = () =>
  invoke<FixtureType[]>("list_builtin_fixture_types");
export const listFixtures = () => invoke<PatchedFixture[]>("list_fixtures");
export const loadFixturePatch = (workspaceId: string) =>
  invoke<{ fixtures: PatchedFixture[]; conflicts: FixtureConflict[] }>("load_fixture_patch", { workspaceId });
export const readFixtureProfile = (path: string) => invoke<FixtureType>("read_fixture_profile", { path });
export const exportFixtureProfile = (path: string, workspaceId: string, fixtureId: string, expected: FixtureType) =>
  invoke<void>("export_fixture_profile", { path, workspaceId, fixtureId, expected });
export const patchImportedFixture = (edit: {
  workspace_id: string; label: string; universe: number; base_address: number; profile: FixtureType;
}) => invoke<PatchedFixture>("patch_imported_fixture", { edit });

export const addFixture = (
  label: string,
  universe: number,
  baseAddress: number,
  fixtureType: FixtureType,
) => invoke<PatchedFixture>("add_fixture", { label, universe, baseAddress, fixtureType });
export const updateFixture = (fixture: PatchedFixture) =>
  invoke<void>("update_fixture", { fixture });
export const removeFixture = (fixtureId: string) =>
  invoke<void>("remove_fixture", { fixtureId });
export const getFixtureConflicts = () =>
  invoke<FixtureConflict[]>("get_fixture_conflicts");
export const dmxTestFixture = (fixtureId: string, on: boolean) =>
  invoke<void>("dmx_test_fixture", { fixtureId, on });

// Live Dashboard
export const loadFixtureDashboard = (workspaceId: string) =>
  invoke<{ fixtures: PatchedFixture[]; groups: FixtureGroup[]; snapshot: DmxUniverseSnapshot[] }>("load_fixture_dashboard", { workspaceId });
export const dmxSetFixtureParam = (workspaceId: string, fixtureId: string, paramIndex: number, value: number) =>
  invoke<void>("dmx_set_fixture_param", { workspaceId, fixtureId, paramIndex, value });
export const dmxSetFixtureCmy = (workspaceId: string, fixtureId: string, expected: [PatchedChannel, PatchedChannel, PatchedChannel], levels: [number, number, number]) =>
  invoke<void>("dmx_set_fixture_cmy", { workspaceId, fixtureId, expected, levels });
export const dmxSetFixtureHsi = (workspaceId: string, fixtureId: string, expected: [PatchedChannel, PatchedChannel, PatchedChannel], levels: [number, number, number]) =>
  invoke<void>("dmx_set_fixture_hsi", { workspaceId, fixtureId, expected, levels });
export const dmxSetFixtureHsv = (workspaceId: string, fixtureId: string, expected: [PatchedChannel, PatchedChannel, PatchedChannel], levels: [number, number, number]) =>
  invoke<void>("dmx_set_fixture_hsv", { workspaceId, fixtureId, expected, levels });
export const dmxSetGroupColor = (
  workspaceId: string, groupId: string, model: FixtureColorModel, expected: FixtureColorChannels[], levels: [number, number, number],
) => invoke<void>("dmx_set_group_color", { workspaceId, groupId, model, expected, levels });
export const dmxClearFixtures = (workspaceId: string) => invoke<void>("dmx_clear_fixtures", { workspaceId });
export const captureLiveTargets = () => invoke<ParamTarget[]>("capture_live_targets");

// Fixture groups
export const listFixtureGroups = () => invoke<FixtureGroup[]>("list_fixture_groups");
export const addFixtureGroup = (label: string, fixtureIds: string[]) =>
  invoke<FixtureGroup>("add_fixture_group", { label, fixtureIds });
export const updateFixtureGroup = (group: FixtureGroup) =>
  invoke<void>("update_fixture_group", { group });
export const removeFixtureGroup = (groupId: string) =>
  invoke<void>("remove_fixture_group", { groupId });

export interface AgentStatus {
  session_file: string | null;
  monitor_file: string | null;
  review_file: string | null;
  armed: boolean;
  performance_locked: boolean;
  pending: { id: string; label: string } | null;
  last_result: { label: string; outcome: string } | null;
}

export const setAgentPerformanceLock = (locked: boolean) => invoke<void>("set_agent_performance_lock", { locked });
export const agentStatus = () => invoke<AgentStatus>("agent_status");
export const getCueMonitorSnapshot = (workspaceId: string) =>
  invoke<CueMonitorSnapshot | null>("get_cue_monitor_snapshot", { workspaceId });
export const agentConnect = () => invoke<AgentStatus>("agent_connect");
export const agentDisconnect = () => invoke<void>("agent_disconnect");
export const agentReviewConnect = (workspaceId: string) =>
  invoke<AgentStatus>("agent_review_connect", { workspaceId });
export const agentReviewDisconnect = (workspaceId: string) =>
  invoke<AgentStatus>("agent_review_disconnect", { workspaceId });
export const getCueReviewHistory = (workspaceId: string) =>
  invoke<CueReviewHistory>("get_cue_review_history", { workspaceId });
export const agentArm = (enabled: boolean) => invoke<AgentStatus>("agent_arm", { enabled });
export const agentDecide = (id: string, approve: boolean) => invoke<AgentStatus>("agent_decide", { id, approve });

export type TransferSettings = Omit<GeneralPreferences, "timed_revisions"> &
  Pick<AudioPreferences, "default_fade_out_ms"> & Pick<DisplayPreferences, "cue_color_style" | "theme">;

export interface SettingsSnapshot {
  name: string;
  path: string | null;
  document: string;
  revision: number;
  values: TransferSettings;
}

export interface SettingsFile {
  source_workspace: string;
  settings: Partial<TransferSettings>;
}

export const getSettingsSnapshot = () => invoke<SettingsSnapshot>("get_settings_snapshot");
export const readSettingsFile = (path: string) => invoke<SettingsFile>("read_settings_file", { path });
export const exportSettingsFile = (path: string, expected: SettingsSnapshot, settings: Partial<TransferSettings>) =>
  invoke<void>("export_settings_file", { path, expected, settings });
export const applyWorkspaceSettings = (expected: SettingsSnapshot, settings: Partial<TransferSettings>) =>
  invoke<void>("apply_workspace_settings", { expected, settings });
export const previewPointMix = (layout: PointMix, outputs: number) =>
  invoke<number[]>("preview_point_mix", { layout, outputs });
export const listAudioMaps = () => invoke<AudioMapLibrary>("list_audio_maps");
export const readAudioMapPeaks = (workspaceId: string, patch: AudioMapPatch) =>
  invoke<number[] | null>("read_audio_map_peaks", { workspaceId, patch });
export const saveAudioMap = (workspaceId: string, previous: AudioMap | null, patch: AudioMapPatch, name: string, marks: AudioMark[]) =>
  invoke<AudioMap>("save_audio_map", { workspaceId, previous, patch, name, marks });
export const deleteAudioMap = (workspaceId: string, map: AudioMap) =>
  invoke<void>("delete_audio_map", { workspaceId, id: map.id, revision: map.revision });
