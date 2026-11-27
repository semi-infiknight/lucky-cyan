// TypeScript types mirroring the Rust backend serialised structs.

export type CueId = string; // UUID as string

export type CueType = "audio" | "memo" | "wait" | "group" | "fade" | "stop" | "devamp" | "video" | "image" | "osc" | "midi" | "midi_file" | "light" | "mic" | "timecode" | "text" | "camera" | "script" | CommandCueType;

/** Cues whose action is performed on *other* cues (QLab's control cues).
 *  Distinct types — own colour, own row label, 1:1 with QLab on import — over
 *  one shared Rust implementation. Grouped behind the toolbar's "+ Command". */
export type CommandCueType = "start" | "pause" | "resume" | "load" | "reset" | "goto" | "arm" | "disarm" | "target";

/** Hints are kept short enough to stay on one line in the toolbar dropdown —
 *  a wrapped row breaks the menu's rhythm. The inspector carries the longer
 *  explanation. */
export const COMMAND_CUE_TYPES: { type: CommandCueType; label: string; hint: string }[] = [
  { type: "start",  label: "Start",  hint: "Trigger the targets" },
  { type: "pause",  label: "Pause",  hint: "Pause the targets" },
  { type: "resume", label: "Resume", hint: "Resume the targets" },
  { type: "load",   label: "Load",   hint: "Bring them up paused" },
  { type: "reset",  label: "Reset",  hint: "Return them to standby" },
  { type: "goto",   label: "Goto",   hint: "Move the Playhead there" },
  { type: "arm",    label: "Arm",    hint: "Enable the targets" },
  { type: "target", label: "Target", hint: "Temporarily change another cue’s target" },
  { type: "disarm", label: "Disarm", hint: "Disable the targets" },
];

export const isCommandCueType = (t: CueType): t is CommandCueType =>
  COMMAND_CUE_TYPES.some((c) => c.type === t);

/** Accent color per cue type — the single source of truth for the toolbar
 *  buttons (App.tsx) and the cue-list context menu (CueListView.tsx). */
export const CUE_TYPE_COLORS: Record<CueType, string> = {
  audio:    "#3b82f6",          // blue
  video:    "#a78bfa",
  image:    "#86efac",
  stop:     "#ef4444",
  fade:     "#ec4899",
  wait:     "#fb923c",
  group:    "#fde047",
  midi:     "var(--wc-accent)", // the green audio used to be
  midi_file: "#2dd4bf",         // teal — a MIDI cue, but one that has a length
  osc:      "#06b6d4",
  light:    "#fbbf24",
  mic:      "#22d3ee",          // cyan
  timecode: "#ff2d9c",          // hot pink
  text:     "#e2e8f0",
  memo:     "#e2e8f0",
  camera:   "#34d399",          // emerald
  devamp:   "#facc15",          // amber — the "release the vamp" action
  // Command cues: one hue family so they read as a group in the cue list,
  // shaded by what they do — go, hold, restore, arm.
  start:    "#4ade80",
  resume:   "#4ade80",
  pause:    "#facc15",
  load:     "#eab308",
  reset:    "#38bdf8",
  goto:     "#0ea5e9",
  arm:      "#fb923c",
  disarm:   "#f97316",
  target:   "#22d3ee",
  script:   "#94a3b8",       // slate — a utility cue, not a stage element
};

/** Kinds of fade curve. `linear` is the hand-editable one: points set where
 *  the curve goes, Alt-dragging a segment sets how it gets there. (An earlier
 *  build had a separate smoothing mode; Alt-bending replaced it.) */
export type CurveKind = "linear" | "s_curve" | "exponential" | "parametric";

/** One control point. Both axes normalised 0..1: `t` is elapsed fade time,
 *  `v` is progress towards the target. The (0,0) and (1,1) endpoints are
 *  implicit and never stored, so a curve always reaches its target. */
export interface CurvePoint {
  t: number;
  v: number;
}

/** One fade envelope. */
export interface CurveShape {
  kind: CurveKind;
  /** Shaping parameter for the parametric kind, roughly -10..10. */
  intensity: number;
  /** Control points, meaningful for the linear and custom kinds. */
  points: CurvePoint[];
  /** Per-segment bow, -1..1, one per gap between resolved points. Alt-drag on
   *  a segment sets it; 0 leaves the segment as the kind draws it. */
  bends: number[];
}

/** The rising and falling envelopes of one fade. QLab's lock button mirrors
 *  them; `mirrored` is that lock, and is the default because a single curve
 *  used to drive both directions. */
export interface FadeShapes {
  up: CurveShape;
  down: CurveShape;
  mirrored: boolean;
}

export const DEFAULT_CURVE_SHAPE: CurveShape = { kind: "s_curve", intensity: 0, points: [], bends: [] };

export const CURVE_KIND_LABELS: Record<CurveKind, string> = {
  s_curve: "S-Curve",
  linear: "Linear (editable)",
  parametric: "Parametric Curve",
  exponential: "Exponential",
};

/** Whether control points do anything for this kind. */
export const curveUsesPoints = (kind: CurveKind) => kind === "linear";

/** One line of the QLab import report: what a QLab cue became. */
export interface ImportedCue {
  qlab_class: string;
  cue_number: string | null;
  cue_name: string;
  inkue_type: string;
  /** Set when the cue could not be represented faithfully. */
  note: string | null;
}

/** Outcome of importing a QLab workspace. */
export interface ImportReport {
  workspace_name: string;
  cue_count: number;
  cue_list_count: number;
  /** Cues needing a look — placeholders, or imported deliberately incomplete. */
  needs_attention: number;
  media_found: number;
  media_missing: string[];
  cart_layout_notes?: string[];
  lighting_patch_notes?: string[];
  audio_map_notes?: string[];
  /** Extra video outputs created from QLab's stages / surfaces (names). */
  video_outputs: string[];
  cues: ImportedCue[];
}

/** Kinds of MIDI message that can fire a cue. */
export type MidiTriggerType = "note_on" | "note_off" | "control_change" | "program_change";

/** A MIDI message bound to a cue. Stored on the Cue List, not the cue, so any
 *  cue type can carry one and it survives the rebuild every inspector edit does. */
export interface MidiTrigger {
  message_type: MidiTriggerType;
  /** 1–16, or 0 for any channel (omni). */
  channel: number;
  /** Note number, controller number, or program number. */
  data1: number;
  /** Required velocity / value. `null` = any — right for a note, whereas a
   *  footswitch that also sends 0 on release wants 127. */
  data2: number | null;
}

/** Machine-level MIDI trigger settings: which input drives per-cue triggers. */
export interface MidiTriggerConfig {
  enabled: boolean;
  port: string | null;
  msc_device_id: number | null;
}

export type MidiTransportAction = "go" | "pause_all" | "resume_all" | "stop_all" | "hard_stop"
  | "next_standby" | "previous_standby" | "next_sequence" | "previous_sequence";
export type MidiValueMatch = "exactly" | "above" | "below";
export interface MidiTransportBinding {
  trigger: MidiTrigger;
  value_match: MidiValueMatch;
}
export interface MidiWorkspaceControls {
  enabled: boolean;
  bindings: Partial<Record<MidiTransportAction, MidiTransportBinding>>;
}
export interface WorkspaceMidiSnapshot {
  workspace_id: string;
  controls: MidiWorkspaceControls;
}

/** Memo Cue payload. A Memo performs no action; `memo_text` is its whole
 *  point — the note the operator reads in the cue list. */
export interface MemoCueData extends CueSummary {
  notes: string;
  memo_text: string;
}

/** MIDI File Cue payload. `sequence_duration_ms`, `track_count`, `channels`
 *  and `parse_error` are read-only: the backend derives them from the file
 *  every time it is parsed, and ignores them on the way back in. */
export interface MidiFileCueData extends CueSummary {
  notes: string;
  file_path: string | null;
  port_name: string;
  playback_rate: number;
  sequence_duration_ms: number | null;
  track_count: number | null;
  channels: number[] | null;
  parse_error: string | null;
}

/** Script Cue payload. Arguments are pre-split: no shell is involved, so a
 *  path containing spaces is one argument and needs no quoting. */
export interface ScriptCueData extends CueSummary {
  notes: string;
  command: string;
  args: string[];
  working_dir: string | null;
  timeout_ms: number;
}

export type CueState = "standby" | "running" | "paused" | "completed";

export type ContinueMode = "do_not_continue" | "auto_continue" | "auto_follow";

export type CueColor =
  | "none"
  | "red"
  | "orange"
  | "yellow"
  | "green"
  | "cyan"
  | "blue"
  | "purple"
  | "pink"
  | "white"
  | "black"
  | "berry"
  | "crimson"
  | "forest"
  | "gray"
  | "hot_pink"
  | "indigo"
  | "lavender"
  | "magenta"
  | "midnight"
  | "olive"
  | "peach"
  | "plum"
  | "sky_blue";

export type FadeCurve = "linear" | "s_curve" | "exponential";

export type GroupMode = "simultaneous" | "sequential" | "start_first" | "start_first_and_enter" | "playlist" | "start_random";

export interface FadeSpec {
  duration_ms: number;
  curve: FadeCurve;
}

export interface WallClockTrigger {
  time: string;
  /** Weekday bit mask, starting with Monday in bit zero. */
  weekdays: number;
}

/** Compact row data used to render the cue list table. */
export interface GroupTimelineOwner {
  workspace_id: string;
  list_id: string;
}

export type PlaylistSecondTrigger = "ignore" | "next" | "previous";

export interface PlaylistTriggerSetting {
  workspace_id: string;
  list_id: string;
  trigger: PlaylistSecondTrigger;
}

export interface CueSummary {
  flagged?: boolean;
  has_flagged_child?: boolean;
  hotkey?: string;
  cue_list_owner?: GroupTimelineOwner;
  wall_clock_trigger?: WallClockTrigger;
  wall_clock_armed?: boolean;
  temporary_target_id?: string;
  id: CueId;
  cue_type: CueType;
  name: string;
  number: string | null;
  notes: string;
  state: CueState;
  continue_mode: ContinueMode;
  color: CueColor;
  /** Saved accent after the first start. `null` keeps the original accent. */
  second_color?: CueColor | null;
  /** Accent the desk currently displays. `none` explicitly hides it. */
  display_color?: CueColor;
  pre_wait_ms: number;
  supports_timeline_entrance?: boolean;
  /** Programmed slice boundaries on a direct Simultaneous Group child's clock. */
  timeline_slices?: { offsets_ms: number[]; duration_ms: number | null };
  timeline_owner?: GroupTimelineOwner;
  post_wait_ms: number;
  duration_ms: number | null;
  file_path: string | null;
  /** The saved Image cue has opted into centre rotation of a PNG. */
  still_rotation_target?: boolean;
  /** Exact live picture this summary refers to. */
  picture_voice_id?: string;
  /** Memo Cue only: the note shown in the Target column, where a media cue
   *  shows its filename. */
  memo_text?: string;
  /** True while the audio file is being decoded in a background thread. */
  is_loading: boolean;
  /** True when this cue is disabled — skipped by the transport on GO. */
  is_disabled: boolean;
  /** True when this cue's media file was assigned but is now missing from disk. */
  is_broken: boolean;
  /** True for non-critical problems (no file assigned, zero duration, empty group). */
  is_warning: boolean;
  /** True when the cue can be scrubbed while it plays (seekCue moves it). */
  seekable: boolean;
  /** One-shot rehearsal entrance as time from the saved clip start. */
  loaded_time_ms?: number;
  is_prepared?: boolean;
  /** Human-readable warning description, present when is_warning is true. */
  warning_message?: string;
  /** Output Patch name this cue plays through (explicit or workspace default).
   *  Absent for cue types with no audio output. */
  output_patch_name?: string;
  /** One pass in ms (untrimmed media or authored loop period), if known. */
  file_duration_ms: number | null;
  /** For Group cues: direct child cue summaries (recursive). */
  children?: CueSummary[];
  /** For Group cues: playback mode. */
  group_mode?: GroupMode;
  /** For Playlist Group cues: whether the playlist loops (wraps last → first). */
  playlist_loop?: boolean;
  playlist_advance_playhead?: boolean;
  playlist_second_trigger?: PlaylistTriggerSetting;
  playlist_auto_shuffle?: boolean;
  /** Direct child IDs in the current Playlist run's playback order. */
  playlist_run_order?: CueId[];
  /** For running Sequential/Playlist Group cues: ID of the currently active child. */
  active_child_id?: string;
}

/** Full cue data returned by get_cue. */
/** Play count meaning "loop this slice forever" (a vamp) — u32::MAX. */
export const PLAY_COUNT_INFINITE = 4294967295;

/** QLab-style slices: markers split the clip into segments, each with a play
 *  count (PLAY_COUNT_INFINITE = vamp until a Devamp Cue releases it). */
export interface SliceList {
  /** Marker positions in ms from file start, sorted ascending. */
  markers: number[];
  /** Play count per segment (markers.length + 1 entries). */
  play_counts: number[];
}

export const EMPTY_SLICES: SliceList = { markers: [], play_counts: [1] };

export interface PointPosition { x: number; y: number; z?: number; }
export interface AudioMapPatch {
  id: string | null;
  device_id: string | null;
  channels: number[];
}

export interface AudioOutputTrim {
  patch: AudioMapPatch;
  levels_db: number[];
}

export interface AudioMark {
  id: string;
  name: string;
  position: PointPosition;
  levels: number[];
}

export interface AudioMap {
  id: string;
  revision: string;
  name: string;
  patch: AudioMapPatch;
  marks: AudioMark[];
}

export interface AudioMapLibrary {
  workspace_id: string;
  maps: AudioMap[];
}

export interface PointMix {
  input: number;
  source: PointPosition;
  speakers?: (PointPosition & { output: number })[];
  map?: AudioMap | null;
}

type CueEffectFilter = {
  id: string;
  enabled: boolean;
  frequency_hz: number;
};

export type CueEffect =
  | (CueEffectFilter & { kind: "high_pass" | "low_pass" })
  | (CueEffectFilter & { kind: "peak"; gain_db: number; q: number });

export interface AudioCueData extends CueSummary {
  notes: string;
  volume_db: number;
  trim_db?: number;
  output_trim?: AudioOutputTrim | null;
  pan: number;
  /** Crosspoint levels in dB, `[input channel][patch channel]`. `null` = the
   *  cue routes with Pan instead. Replaces pan when set. */
  level_matrix?: number[][] | null;
  /** Names in source channel order, independent of the patch or matrix. */
  source_channel_names?: string[];
  point_mix?: PointMix | null;
  audio_effects?: CueEffect[];
  fade_in_ms: number | null;
  fade_in_curve: FadeCurve | null;
  fade_out_ms: number | null;
  fade_out_curve: FadeCurve | null;
  start_time_ms: number | null;
  end_time_ms: number | null;
  loop_count: number;
  output_patch_id: string | null;
  rate: number;
  slices: SliceList;
}

/** How the source frame is mapped onto the output surface. */
export type FitMode = "fit" | "fill" | "stretch";

/** Per-cue visual geometry for Video and Image cues. */
export interface VideoGeometry {
  fit_mode: FitMode;
  /** Horizontal offset as a fraction of the scaled video width (−1..1). */
  pan_x: number;
  /** Vertical offset as a fraction of the scaled video height (−1..1). */
  pan_y: number;
  /** Linear scale factor (1.0 = 100%). */
  scale: number;
  /** Clockwise rotation in degrees (0–359). */
  rotation: number;
  /** Opt-in angle for source-space placement on supported still Image cues. */
  source_rotation_degrees?: number | null;
  /** Pivot fractions measured from the left and top of the cropped still. */
  source_rotation_pivot?: [number, number] | null;
  /** Crop per edge as a fraction of the source size (0..0.45 each). */
  crop_left: number;
  crop_right: number;
  crop_top: number;
  crop_bottom: number;
}

/** Neutral geometry (engine defaults). */
export const DEFAULT_GEOMETRY: VideoGeometry = {
  fit_mode: "fit",
  pan_x: 0,
  pan_y: 0,
  scale: 1,
  rotation: 0,
  crop_left: 0,
  crop_right: 0,
  crop_top: 0,
  crop_bottom: 0,
};

/** How a visual cue's pixels combine with the layers below it. */
export type BlendMode =
  | "normal" | "add" | "multiply" | "screen" | "overlay"
  | "soft_light" | "hard_light" | "darken" | "lighten"
  | "color_dodge" | "color_burn" | "difference" | "exclusion" | "subtract"
  | "linear_burn" | "pin_light" | "hue" | "saturation" | "color" | "luminosity"
  | "source_atop";

/** Compositing properties of a visual cue (QLab layer model). */
export interface LayerStyle {
  /** Stacking order 1–1000 (higher = closer to the viewer). null = automatic (newest on top). */
  layer: number | null;
  /** Base opacity 0.0–1.0. */
  opacity: number;
  blend_mode: BlendMode;
  /** 0 is monochrome, 1 is unchanged, 2 doubles the color difference. */
  saturation?: number;
  picture_brightness?: number;
  contrast?: number;
  hue_degrees?: number;
}

/** Default compositing (automatic layer, fully opaque, normal blending). */
export const DEFAULT_LAYER_STYLE: LayerStyle = {
  layer: null,
  opacity: 1,
  blend_mode: "normal",
};

/** Full cue data returned by get_cue for a Video Cue. */
export interface VideoCueData extends CueSummary {
  notes: string;
  volume_db: number;
  /** Crosspoint levels in dB for the video's audio track. See AudioCueData. */
  level_matrix?: number[][] | null;
  /** Names in source channel order, independent of the patch or matrix. */
  source_channel_names?: string[];
  /** Audio track fade-in. */
  fade_in_ms: number | null;
  fade_in_curve: FadeCurve | null;
  /** Audio track fade-out. */
  fade_out_ms: number | null;
  fade_out_curve: FadeCurve | null;
  /** Visual (GL overlay) fade-in — independent from audio. */
  video_fade_in_ms: number | null;
  video_fade_in_curve: FadeCurve | null;
  /** Visual (GL overlay) fade-out — independent from audio. */
  video_fade_out_ms: number | null;
  video_fade_out_curve: FadeCurve | null;
  start_time_ms: number | null;
  end_time_ms: number | null;
  loop_count: number;
  /** Video output to play on. null = the main output. */
  output_id: string | null;
  output_patch_id: string | null;
  /** Freeze on the last frame at natural EOF instead of cutting to black. */
  hold_last_frame: boolean;
  allow_silent_speed?: boolean;
  geometry: VideoGeometry;
  layer_style: LayerStyle;
  slices: SliceList;
  /** Full media duration (serialized form only — summaries carry
   *  file_duration_ms instead). */
  cached_duration_ms?: number | null;
}

/** 9-point position grid for TextCue. */
export type TextPosition =
  | "top_left" | "top_center" | "top_right"
  | "middle_left" | "center" | "middle_right"
  | "bottom_left" | "bottom_center" | "bottom_right";

/** Full cue data returned by get_cue for a Text Cue. */
export interface TextCueData extends CueSummary {
  notes: string;
  /** Text content to display (multi-line supported). */
  text: string;
  /** Font family name. */
  font: string;
  /** Font size in mpv OSD/ASS points. */
  font_size: number;
  /** Text colour as "#RRGGBB". */
  text_color: string;
  /** Position on the output surface. */
  position: TextPosition;
  /** Video output to show on. null = the main output. */
  output_id: string | null;
  /** Auto-complete after this duration in ms. null = hold until stopped. */
  display_duration_ms: number | null;
}

/** Full cue data returned by get_cue for an Image Cue. */
export interface ImageCueData extends CueSummary {
  notes: string;
  /** Video output to show on. null = the main output. */
  output_id: string | null;
  fade_in_ms: number | null;
  fade_in_curve: FadeCurve | null;
  fade_out_ms: number | null;
  fade_out_curve: FadeCurve | null;
  /** How long the image stays on screen in ms. null = infinite (hold until stopped). */
  display_duration_ms: number | null;
  geometry: VideoGeometry;
  layer_style: LayerStyle;
}

// ---------------------------------------------------------------------------
// Camera types
// ---------------------------------------------------------------------------

/** Where a Camera Cue's live feed comes from (matches the Rust enum tagging). */
export type CameraSource =
  | { kind: "device"; id: string; name: string }
  | { kind: "url"; url: string };

/** One connected capture device (from list_camera_devices). */
export interface CameraDeviceInfo {
  id: string;
  name: string;
  /** Existing Linux numbered device node when id uses a persistent port. */
  numbered_id?: string;
}

/** Full cue data returned by get_cue for a Camera Cue. */
export interface CameraCueData extends CueSummary {
  notes: string;
  /** Video output to show on. null = the main output. */
  output_id: string | null;
  source: CameraSource;
  /** Visual (GL overlay) fade-in from black. */
  video_fade_in_ms: number | null;
  video_fade_in_curve: FadeCurve | null;
  /** Visual (GL overlay) fade-out to black on stop. */
  video_fade_out_ms: number | null;
  video_fade_out_curve: FadeCurve | null;
  geometry: VideoGeometry;
  layer_style: LayerStyle;
}

// ---------------------------------------------------------------------------
// MIDI types
// ---------------------------------------------------------------------------

export type MidiMessageType =
  | "note_on" | "note_off" | "key_pressure" | "control_change"
  | "program_change" | "channel_pressure" | "pitch_bend" | "msc" | "sysex";

export interface MscMessage {
  device_id: number;
  command_format: number;
  command: "go" | "stop" | "resume" | "load" | "all_off" | "restore" | "reset" | "go_off"
    | "standby_next" | "standby_previous" | "sequence_next" | "sequence_previous";
  q_number: string;
  q_list: string;
  q_path: string;
}

export interface MidiFade {
  /** Fade duration in milliseconds, 1–86,400,000. */
  duration_ms: number;
  /** End value, 0–127 for controllers and pressure, −8192–8191 for bend. */
  end_value: number;
  /** An absent envelope interpolates linearly. */
  curve?: CurveShape;
}

export interface MidiMessage {
  port_name: string;
  message_type: MidiMessageType;
  /** MIDI channel 1–16 */
  channel: number;
  /** First 7-bit data byte (LSB for pitch bend). */
  data1: number;
  /** Second 7-bit data byte (MSB for pitch bend), unused for two-byte messages. */
  data2: number;
  msc?: MscMessage;
  /** Hexadecimal SysEx data with optional F0/F7 framing. */
  sysex?: string;
  fade?: MidiFade;
}

/** Full cue data returned by get_cue for a MIDI Cue. */
export interface MidiCueData extends CueSummary {
  notes: string;
  messages: MidiMessage[];
}

/** An ordered position waypoint in fractions of the scaled picture dimensions. */
export type TranslationWaypoint = [x: number, y: number];
export interface MapMotion {
  map_id: string;
  revision: string;
  start_mark: string;
  end_mark: string;
}

/** Full cue data returned by get_cue for a Fade Cue. */
export interface FadeCueData extends CueSummary {
  notes: string;
  /** UUIDs of cues to fade (empty = no-op). */
  target_cue_ids: string[];
  /** Display labels kept in sync with target_cue_ids. */
  target_cue_numbers: string[];
  /** Target audio volume in dB (−60 = silence, 0 = unity). */
  patch_target_id?: string | null;
  patch_target_db?: number;
  target_volume_db: number;
  /** Target visual brightness in percent (0 = black, 100 = fully visible). Independent from volume. */
  target_brightness_pct: number;
  /** Set false to preserve the target picture's existing opacity throughout the fade. */
  fade_brightness?: boolean;
  /** Target stereo pan (-1 = left, +1 = right); null = leave pan untouched. */
  target_pan: number | null;
  /** Absolute Audio playback rate; null leaves the rate alone. */
  target_rate?: number | null;
  target_silent_speed?: number | null;
  /** Absolute cutoff on a saved filter. This Fade runs only the selected filter. */
  target_cutoff?: { filter_id: string; frequency_hz: number } | null;
  target_eq_gain?: { filter_id: string; gain_db: number } | null;
  /** Absolute picture position, in fractions of its scaled width and height. */
  target_translation?: { x: number | null; y: number | null } | null;
  /** Intermediate X/Y positions in playback order. */
  translation_waypoints?: TranslationWaypoint[];
  smooth_translation_path?: boolean;
  loop_translation_path?: boolean;
  /** Absolute uniform visual scale; null leaves it alone. */
  target_scale?: number | null;
  /** Signed position offsets and a multiplier against the live picture. */
  target_relative_geometry?: {
    translation: { x: number | null; y: number | null } | null;
    scale_factor: number | null;
  } | null;
  /** Absolute clockwise angle in degrees for an opted-in still Image. */
  target_rotation_degrees?: number | null;
  /** Signed degrees to travel from the still's live angle, up to ten turns. */
  target_rotation_delta_degrees?: number | null;
  /** Absolute compositor saturation, 0 through 2; null leaves it alone. */
  target_saturation?: number | null;
  target_picture_brightness?: number | null;
  target_contrast?: number | null;
  target_hue_degrees?: number | null;
  target_map_motion?: MapMotion | null;
  /** When true (default) the fade drives volume toward target_volume_db; false = pan-only. */
  fade_volume: boolean;
  /** Fade duration in milliseconds. */
  fade_duration_ms: number;
  /** Legacy single-curve name. Still written for older builds; `fade_shapes`
   *  is what the engine uses. */
  fade_curve: FadeCurve;
  /** Rising and falling envelopes — QLab's Curve tab. */
  fade_shapes: FadeShapes;
  /** Stop the target cue(s) once the fade completes. */
  stop_at_end: boolean;
  /** Crossfade: a visual cue started by this Fade so the picture dissolves from
   *  the targets into it. null = plain fade. */
  crossfade_into_id: string | null;
}

/** Full cue data returned by get_cue for a Wait Cue. */
export interface WaitCueData extends CueSummary {
  notes: string;
  /** The configured wait duration in milliseconds. */
  wait_duration_ms: number;
}

/** Full cue data returned by get_cue for a Stop Cue. */
export interface StopCueData extends CueSummary {
  replacement_id?: string | null;
  replacement_number?: string;
  load_time_ms?: number | null;
  notes: string;
  /** UUIDs of cues to stop. Empty = stop all running cues. */
  target_cue_ids: string[];
  /** Display labels kept in sync with target_cue_ids. */
  target_cue_numbers: string[];
  /** true = immediate cut (no fade); false = soft stop with workspace fade-out. */
  hard_stop_mode: boolean;
}

export interface DevampCueData extends CueSummary {
  notes: string;
  /** UUIDs of the cues to devamp. */
  target_cue_ids: string[];
  /** Display labels kept in sync with target_cue_ids. */
  target_cue_numbers: string[];
  scope?: "current_slice" | "whole_cue";
  /** true = the target stops at the end of its current slice. */
  stop_at_end: boolean;
}

/** Information about a connected monitor. */
export interface ScreenInfo {
  index: number;
  width: number;
  height: number;
  x: number;
  y: number;
  is_primary: boolean;
}

export interface DeviceInfo {
  id: string;
  name: string;
  channels: number;
  sample_rate: number;
}

export interface OutputPatch {
  id: string;
  name: string;
  device_id: string;
  channels: number[];
  /** Logical output labels in patch order. Older shows omit this field. */
  channel_names?: string[];
  /** Mixer fader in dB (0 = unity). */
  gain_db: number;
  /** File Audio starts only on this named device. Older patches omit this. */
  require_named_device?: boolean;
  /** Native effects on the summed Audio, Video and Mic signals of this patch. */
  audio_effects?: CueEffect[];
  /** Effects on logical outputs from Audio, Video and Mic cues before device routing. */
  cue_output_effects?: Array<{ output: number; effects: CueEffect[] }>;
}

/** A named live-audio input mapping (Mic Cues). Mirror of OutputPatch, stored in the workspace. */
export interface InputPatch {
  id: string;
  name: string;
  device_id: string;
  /** Zero-based input device channel indices this patch exposes. */
  channels: number[];
}

/** Full cue data returned by get_cue for a Mic Cue. */
export interface MicCueData extends CueSummary {
  notes: string;
  input_patch_id: string | null;
  /** Device channel indices to take (empty = use the patch's own channels). */
  input_channels: number[];
  output_patch_id: string | null;
  volume_db: number;
  pan: number;
  level_matrix?: number[][] | null;
  fade_in_ms: number | null;
  fade_in_curve: FadeCurve | null;
  fade_out_ms: number | null;
  fade_out_curve: FadeCurve | null;
}

export type CueListMode = "sequential" | "cart";

export interface CartCell { row: number; column: number }
export interface CartLayout {
  rows: number;
  columns: number;
  pads: Record<CueId, CartCell | null>;
}
export type CartEdit =
  | { action: "resize"; rows: number; columns: number }
  | { action: "place"; cue_id: CueId; cell: CartCell }
  | { action: "compact" };

export interface CueListWindowSnapshot {
  workspace_id: string;
  name: string;
  number: string | null;
  layout: CartLayout | null;
  standby_id: CueId | null;
  standby_child_id: CueId | null;
  cues: CueSummary[];
}

export type SequentialWindowAction =
  | { kind: "go"; cue_id: CueId; child_id: CueId | null }
  | { kind: "set_standby"; cue_id: CueId }
  | { kind: "pause_list" }
  | { kind: "resume_list" }
  | { kind: "stop_list" };

export interface CueListSummary {
  id: string;
  name: string;
  number?: string | null;
  mode: CueListMode;
  chain_cart_cues?: boolean;
  cart_layout?: CartLayout | null;
}

export interface MonitorCue {
  id: CueId;
  number: string | null;
  name: string;
  kind: string;
  is_disabled: boolean;
}

export interface MonitorRun {
  cue: MonitorCue;
  parent_id: CueId | null;
  state: "running" | "paused";
}

export interface CueMonitorSnapshot {
  workspace_id: string;
  name: string;
  active_cue_list_id: string;
  lists: {
    id: string;
    name: string;
    standby: MonitorCue | null;
    active: MonitorRun[];
  }[];
}

export interface DefaultWorkspaceTemplate {
  path: string;
  name: string;
  available: boolean;
}

export interface WorkspaceInfo {
  instance_id: string;
  revision: number;
  name: string;
  is_modified: boolean;
  file_path: string | null;
}

export interface CueReviewHistoryRow {
  event_index: number;
  time: string;
  list_name: string | null;
  cue_number: string | null;
  cue_name: string | null;
  change?: "note" | "flag_set" | "flag_cleared" | "name";
}

export interface CueReviewHistory {
  workspace_id: string;
  workspace_name: string;
  rows: CueReviewHistoryRow[];
}

/** Metadata for an unsaved-work snapshot left by an abnormally-terminated session. */
export interface RecoveryInfo {
  name: string;
  original_path: string | null;
  modified_at: string | null;
}

// --- Preflight (Check Workspace) -------------------------------------------

export type Severity = "error" | "warning";

export interface CueIssue {
  severity: Severity;
  message: string;
}

/** One cue's preflight result (only cues with at least one issue are returned). */
export interface CueValidation {
  owner: Pick<WorkspaceInfo, "instance_id" | "revision" | "file_path">;
  cue_id: CueId;
  cue_number: string | null;
  cue_name: string;
  cue_type: CueType;
  issues: CueIssue[];
  /** The unresolved media path when the problem is a missing file (drives relink). */
  missing_file: string | null;
}

export interface RelinkResult {
  relinked: number;
}

// --- Logs ------------------------------------------------------------------

export interface LogLine {
  ts: string;
  level: string;
  target: string;
  message: string;
}

// --- Runtime health (device/network faults) --------------------------------

export type HealthLevel = "error" | "warning" | "info";

export interface HealthAlert {
  key: string;
  level: HealthLevel;
  message: string;
  /** Action id the banner maps to a command (e.g. "restore_audio_device"). */
  action: string | null;
  action_label: string | null;
}

export interface CollectReport {
  workspace_path: string;
  files_copied: number;
  files_skipped: number;
  files_missing: string[];
}

export interface WaveformData {
  peaks: number[];
  /** RMS per bin — the "body" of the sound, drawn inside the peak envelope. */
  rms: number[];
  file_duration_s: number;
}

// ---------------------------------------------------------------------------
// OSC types
// ---------------------------------------------------------------------------

export type OscArgType = "int" | "float" | "str" | "bool";

export type OscArg =
  | { type: "int";   value: number }
  | { type: "float"; value: number }
  | { type: "str";   value: string }
  | { type: "bool";  value: boolean };

export interface OscMessage {
  raw?: { format: "text" | "hex"; value: string };
  patch_id: string;
  address: string;
  args: OscArg[];
}

export interface OscPatch {
  id: string;
  name: string;
  ip: string;
  port: number;
}

/** Full cue data returned by get_cue for an OSC Cue. */
export interface OscCueData extends CueSummary {
  notes: string;
  messages: OscMessage[];
}

export interface OscReceiveConfig {
  enabled: boolean;
  port: number;
  plain_text_enabled: boolean;
  plain_text_port: number;
  allowed_ips: string[];
  feedback_enabled: boolean;
  feedback_host: string;
  feedback_port: number;
  /** Send rate (Hz) for /inkue/cue/{i}/progress|elapsed|remaining|duration.
   *  0 disables progress feedback. */
  feedback_progress_hz: number;
}

export type OscListenerStatus =
  | { state: "binding" }
  | { state: "disabled" }
  | { state: "listening"; address: string }
  | { state: "bind_failed"; address: string; error: string };

/** One IPv4-capable network interface (Preferences → Network). */
export interface NetworkInterfaceInfo {
  name: string;
  ip: string;
  is_loopback: boolean;
}

/** Machine-level network interface selection. All-null = Automatic. */
export interface NetworkInterfaceConfig {
  interface_name: string | null;
  interface_ip: string | null;
}

// ---------------------------------------------------------------------------
// DMX / Lighting
// ---------------------------------------------------------------------------

export type OutputProtocol = "Sacn" | "ArtNet";

/** One workspace-level universe output mapping (matches `engine::dmx_sink::UniverseOutput`). */
export interface UniverseOutput {
  universe: number;
  protocol: OutputProtocol;
  /** Destination IP string, or null for the sACN multicast group. */
  destination: string | null;
  enabled: boolean;
}

/** Live output bytes of one universe, pushed via the `dmx-monitor` event. */
export interface DmxUniverseSnapshot {
  universe: number;
  channels: number[];
}

/** Resolution of one fixture parameter on the wire (matches `engine::dmx_engine::ChannelWidth`). */
export type ChannelWidth = "Bit8" | "Bit16";

/** What a fixture parameter controls. */
export type ParamKind =
  | "intensity"
  | "red"
  | "green"
  | "blue"
  | "cyan"
  | "magenta"
  | "yellow"
  | "hue"
  | "saturation"
  | "hsi_intensity"
  | "hsv_value"
  | "white"
  | "amber"
  | "uv"
  | "pan"
  | "tilt"
  | "generic";

export interface PatchedChannel {
  universe: number;
  channel: number;
  width: ChannelWidth;
  fine_channel?: number;
}

export interface FixtureColorChannels {
  id: string;
  channels: [PatchedChannel, PatchedChannel, PatchedChannel];
}

/** One controllable parameter of a fixture, offset from its base address. */
export interface FixtureParam {
  kind: ParamKind;
  name: string;
  channel_offset: number;
  fine_channel_offset?: number;
  width: ChannelWidth;
  default: number;
}

/** The channel layout of a kind of lighting instrument. */
export interface FixtureType {
  manufacturer?: string;
  mode?: string;
  name: string;
  parameters: FixtureParam[];
}

export interface FixtureLibraryEntry {
  id: string;
  name: string;
  manufacturer: string;
  mode: string;
  channels: number;
}

export interface FixtureLibrary {
  profiles: FixtureLibraryEntry[];
  unreadable: number;
}

/** A fixture placed at a DMX address in the workspace patch. */
export interface PatchedFixture {
  id: string;
  label: string;
  universe: number;
  base_address: number;
  fixture_type: FixtureType;
}

/** A detected address clash between two patched fixtures. */
export interface FixtureConflict {
  fixture_a: string;
  fixture_b: string;
  universe: number;
  message: string;
}

/** A named set of fixtures driven together by one Light Cue control. */
export interface FixtureGroup {
  id: string;
  label: string;
  fixture_ids: string[];
}

export type FixtureColorModel = "cmy" | "hsi" | "hsv";
export type ColorLevels = [number, number, number];

/** A scalar fixture parameter, a group parameter role, or a native group color. */
export type ParamTarget =
  | { kind: "fixture"; fixture_id: string; param_index: number; value: number }
  | { kind: "group"; group_id: string; param_kind: ParamKind; value: number }
  | { kind: "group_color"; group_id: string; model: FixtureColorModel; levels: ColorLevels | null };

/** Full cue data returned by get_cue for a Light Cue. */
export interface LightCueData extends CueSummary {
  qlab_light_source?: { commandText?: string; alwaysCollate?: boolean; [key: string]: unknown } | null;
  notes: string;
  targets: ParamTarget[];
  fade: FadeSpec;
}

// ---------------------------------------------------------------------------
// Timecode
// ---------------------------------------------------------------------------

export type TcRate = "24" | "25" | "29.97" | "29.97df" | "30";

export type TcSource = "mtc" | "ltc";

export interface TcPosition {
  h: number;
  m: number;
  s: number;
  f: number;
  rate: TcRate;
}

export interface TcTrigger {
  /** SMPTE string HH:MM:SS:FF or HH:MM:SS;FF */
  position: string;
  /** true = position was entered as Real Time (ms) */
  real_time: boolean;
  rate: TcRate;
}

export type TcOnStop = "continue" | "pause" | "stop";
export type TcOnStart = "future_marks" | "join_active";

export interface CueListTcConfig {
  enabled: boolean;
  rate: TcRate;
  freewheel_ms: number;
  on_start: TcOnStart;
  on_stop: TcOnStop;
}

export type TcOutputType = "mtc" | "ltc";

export interface TimecodeCueData extends CueSummary {
  tc_type: TcOutputType;
  midi_port: string | null;
  output_patch_id: string | null;
  rate: TcRate;
  /** SMPTE string */
  start_frame: TcPosition;
  end_frame: TcPosition | null;
}

export interface TcMachineConfig {
  enabled: boolean;
  receiver_config: {
    source: TcSource;
    midi_port: string | null;
    ltc_device_id: string | null;
  };
}

// ---------------------------------------------------------------------------
// Preferences
// ---------------------------------------------------------------------------

export type AudioBackend = "wasapi_shared" | "wasapi_exclusive" | "asio";

/** Hardware-specific settings — stored in %APPDATA%\Inkue\audio.json, not in the workspace. */
export interface MachineAudioConfig {
  backend: AudioBackend;
  device_id: string | null;
  /** Friendly name of the selected output device, captured at selection time. */
  device_name: string | null;
  /** Selected audio input device for Mic Cues. null = system default input. */
  input_device_id: string | null;
  /** Dedicated audition/monitor output (OSC /auditionGo, cue monitor). null = not configured. */
  audition_device_id: string | null;
  /** Friendly name of the audition output, captured at selection time. */
  audition_device_name: string | null;
  /** Samples. Only applied for WASAPI Exclusive. */
  buffer_size: number;
  /** ASIO output pair index (0 = Out 1-2, 1 = Out 3-4, …). */
  asio_out_pair: number;
}

export const DEFAULT_MACHINE_AUDIO_CONFIG: MachineAudioConfig = {
  backend: "wasapi_shared",
  device_id: null,
  device_name: null,
  input_device_id: null,
  audition_device_id: null,
  audition_device_name: null,
  buffer_size: 256,
  asio_out_pair: 0,
};

/** Show-specific audio defaults — travel with the .inkue workspace file. */
export interface AudioPreferences {
  default_volume_db: number;
  default_fade_out_ms: number;
  default_fade_curve: FadeCurve;
}

export type CueRowHeight = "compact" | "normal" | "tall";

export interface GeneralPreferences {
  timed_revisions: "off" | "one_minute" | "five_minutes" | "fifteen_minutes";
  show_hard_stop_blackout: boolean;
  double_go_protection_ms: number;
  confirm_before_delete: boolean;
  auto_scroll_to_playhead: boolean;
  /** QLab "always audition": GO previews on the audition output instead of starting. */
  always_audition: boolean;
  cue_row_height: CueRowHeight;
  /** When true, reordering cues rewrites all numbers to position; when false
   * (default), numbers are stable and resequencing is an explicit action. */
  auto_renumber_on_reorder: boolean;
}

export const DEFAULT_GENERAL_PREFS: GeneralPreferences = {
  timed_revisions: "off",
  show_hard_stop_blackout: false,
  double_go_protection_ms: 500,
  confirm_before_delete: false,
  auto_scroll_to_playhead: true,
  always_audition: false,
  cue_row_height: "normal",
  auto_renumber_on_reorder: false,
};

export type TimerPosition = "center" | "top_left" | "top_right" | "bottom_left" | "bottom_right";

/** How a cue's colour tag is rendered in the Cue List. */
export type CueColorStyle = "stripe" | "full_row";

export interface DisplayPreferences {
  /** Monitor index for the unified output surface. null = floating window. */
  output_screen: number | null;
  /** When true, a countdown timer is shown on the output window. */
  show_output_timer: boolean;
  /** When true the timer counts down (remaining); when false it counts up (elapsed). */
  timer_count_down: boolean;
  /** Font family for the OSD timer (e.g. "Arial"). */
  timer_font: string;
  /** Font size in mpv OSD points for the timer (default 120). */
  timer_font_size: number;
  /** Position of the timer on the output window. */
  timer_position: TimerPosition;
  /** When true, milliseconds are shown (e.g. 00:00.000). */
  timer_show_ms: boolean;
  /** Margin in pixels from the edge for corner positions. */
  timer_margin: number;
  /** When true (and show_output_timer is true), show timer as floating Win32 window instead of OSD overlay. */
  timer_floating: boolean;
  /** Appearance in the main desk and its auxiliary windows. */
  theme: "dark" | "light" | "navy" | "system";
  /** How a cue's colour tag is rendered in the Cue List. */
  cue_color_style: CueColorStyle;
  /** Projector-alignment transform of the main output. */
  output_transform?: OutputTransform;
  /** Video output that shows the on-output timer. null = the main output. */
  timer_output?: string | null;
}

export interface OutputMask {
  /** Fractions of the picture, ordered left, right, top, bottom. */
  trim: [number, number, number, number];
  feather: [number, number, number, number];
}

export const CLEAR_OUTPUT_MASK: OutputMask = { trim: [0, 0, 0, 0], feather: [0, 0, 0, 0] };

/** Rectangles [left, top, width, height] in hundredths of a percent. */
export interface OutputRegion {
  source: [number, number, number, number];
  destination: [number, number, number, number];
  seam?: {
    partner: string;
    direction: "horizontal" | "vertical";
    overlap: number;
    gamma: number;
    rising: boolean;
  };
}

/** Global projector-alignment transform (Preferences → Display). */
export interface OutputTransform {
  /** Horizontal offset as a fraction of the output width (−1..1). */
  pan_x: number;
  /** Vertical offset as a fraction of the output height (−1..1). */
  pan_y: number;
  /** Linear scale factor (1.0 = 100%). */
  scale: number;
  /** Clockwise rotation in degrees — fractional values supported (0.1° steps). */
  rotation: number;
  /**
   * Corner-pin offsets in fractions of the output size, storage order
   * TL, TR, BL, BR; positive = right/down. Applied after scale/rotation/pan.
   */
  corners: [[number, number], [number, number], [number, number], [number, number]];
  mask?: OutputMask;
  regions?: OutputRegion[];
}

/** Identity transform (engine defaults). */
export const DEFAULT_OUTPUT_TRANSFORM: OutputTransform = {
  pan_x: 0,
  pan_y: 0,
  scale: 1,
  rotation: 0,
  corners: [[0, 0], [0, 0], [0, 0], [0, 0]],
};

/** Built-in calibration patterns; custom_image carries the file path. */
export type TestPatternKind =
  | "grid" | "smpte_bars" | "rgb_test" | "test_card"
  | "white" | "gray" | "black" | "custom_image";

/** Serialised form matching the Rust `TestPattern` enum (tag + content). */
export interface TestPattern {
  kind: TestPatternKind;
  /** Only for custom_image. */
  path?: string;
}

export const DEFAULT_DISPLAY_PREFS: Pick<DisplayPreferences, "theme" | "cue_color_style"> = {
  theme: "system",
  cue_color_style: "stripe",
};

export interface MscBroadcastDestination {
  /** MIDI output port name (QLab's "MIDI patch"). */
  port_name: string;
  /** MSC command format (e.g. 48 = Lighting (General)). */
  command_format: number;
  /** Individual device ID 0..=111, or 127 for the all-call address. */
  device_id: number;
}

export interface MidiPreferences {
  /** QLab's MSC Broadcast tab checkbox. */
  msc_broadcast_enabled: boolean;
  msc_broadcast_destinations: MscBroadcastDestination[];
}

export interface AppPreferences {
  audio: AudioPreferences;
  general: GeneralPreferences;
  network: Record<string, never>;
  midi: MidiPreferences;
  display: DisplayPreferences;
}

// ---------------------------------------------------------------------------
// Events emitted by the backend
export interface CueListsChangedEvent {
  cue_lists: CueListSummary[];
  active_cue_list_id: string;
}

export interface CueStateChangedEvent {
  cue_id: CueId;
  old_state: CueState;
  new_state: CueState;
}

export interface PlayheadMovedEvent {
  cue_id: CueId | null;
}

export interface WorkspaceModifiedEvent {
  /* empty */
}

export interface DeviceChangedEvent {
  devices: DeviceInfo[];
}

/** Key pressed while the output window has focus, forwarded by the backend. */
export interface OutputKeyEvent {
  key: string;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
}

export interface CueTimeUpdateEvent {
  cue_id: CueId;
  workspace_id: string;
  elapsed_ms: number;
  action_elapsed_ms: number;
  remaining_ms: number | null;
}

/** One video output (a window on a screen). The main output comes first. */
export interface VideoOutputInfo {
  /** The main output's id is the nil UUID. */
  id: string;
  name: string;
  is_main: boolean;
  /** Monitor index (0 = primary); null = a floating window. */
  screen: number | null;
  transform: OutputTransform;
}

export interface StageSnapshot {
  source_width: number;
  source_height: number;
  png_base64: string;
}

export interface StageMonitorInfo {
  workspace_id: string;
  output_id: string;
  name: string;
}

/** Id of the main output. */
export const MAIN_OUTPUT_ID = "00000000-0000-0000-0000-000000000000";
