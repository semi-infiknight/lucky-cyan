// Contextual inspector panel shown on the right side.
//
// Tab model: Basics (identity) | the cue type's main tab (Fade / Stop / Group /
// Camera / Messages / Light / Mic / Timecode / Text) | Time | Levels | Fade |
// Layer | Geometry | Triggers — only the tabs that apply to the selected cue
// type are shown.

import { useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import "./inspector.css";
import type { ScriptCueData, AudioCueData, CameraCueData, CueSummary, CueType, CurveShape, DevampCueData, FadeCueData, ImageCueData, LightCueData, MemoCueData, MicCueData, MidiCueData, MidiFileCueData, OscCueData, StopCueData, TextCueData, TimecodeCueData, VideoCueData, WaitCueData } from "../../lib/types";
import { CUE_TYPE_COLORS, isCommandCueType } from "../../lib/types";
import type { GroupMode } from "../../lib/types";
import { getCue, updateCue, setAudioFile, setVideoFile, setImageFile, setMidiFile } from "../../lib/commands";
import type { CueProperties } from "../../lib/commands";
import { AUDIO_EXTENSIONS, VIDEO_EXTENSIONS, IMAGE_EXTENSIONS, MIDI_EXTENSIONS } from "../../lib/mediaTypes";
import { open } from "@tauri-apps/plugin-dialog";
import { BasicsTab } from "./BasicsTab";
import { TimeTab } from "./TimeTab";
import { LevelsTab } from "./LevelsTab";
import { AudioTrimTab } from "./AudioTrimTab";
import { AudioEffectsTab } from "./AudioEffectsTab";
import { FadeTab } from "./FadeTab";
import { FadeCueTab } from "./FadeCueTab";
import { StopTab } from "./StopTab";
import { CommandTab } from "./CommandTab";
import { ScriptTab } from "./ScriptTab";
import { DevampTab } from "./DevampTab";
import { GroupTab } from "./GroupTab";
import { GroupTimelineTab } from "./GroupTimelineTab";
import { LayerTab } from "./LayerTab";
import { GeometryTab } from "./GeometryTab";
import { MidiTab } from "./MidiTab";
import { MidiFileTab } from "./MidiFileTab";
import { MemoTab } from "./MemoTab";
import { OscTab } from "./OscTab";
import { LightTab } from "./LightTab";
import { MicTab } from "./MicTab";
import { TextTab } from "./TextTab";
import { TimecodeTab } from "./TimecodeTab";
import { TriggersTab } from "./TriggersTab";
import { CameraTab } from "./CameraTab";
import { useWorkspaceStore } from "../../stores/workspaceStore";

interface Props {
  selectedCue: CueSummary | null;
  selectedCueIds: string[];
  onRefresh: () => void;
  /** Open the clip editor dock (trim + slices) for a cue. */
  onOpenEditor?: (cueId: string) => void;
  /** Open the fade curve editor dock for a cue. */
  onOpenCurveEditor?: (cueId: string) => void;
  /** Bump to force a re-fetch of the inspected cue (dock edits). */
  reloadToken?: number;
  /** Called after every inspector save, so the clip editor dock can reload. */
  onCueSaved?: () => void;
}

type Tab =
  | "basics" | "time" | "levels" | "fade" | "layer" | "geometry" | "messages"
  | "fade-cue" | "stop" | "devamp" | "group" | "timeline" | "light" | "mic" | "timecode"
  | "text" | "camera" | "triggers" | "command" | "script" | "midi-file" | "memo" | "audio-effects" | "trim";

type CueData =
  | AudioCueData | VideoCueData | ImageCueData | WaitCueData | FadeCueData
  | MidiCueData | OscCueData | StopCueData | DevampCueData | LightCueData
  | MicCueData | TimecodeCueData | TextCueData | CameraCueData;

/** Keep identity and trigger assignments in the same place for every cue. */
function tabsFor(type: CueType, groupMode?: GroupMode): { id: Tab; label: string }[] {
  const isAV = type === "audio" || type === "video";
  const isVisual = type === "video" || type === "image" || type === "camera";
  const hasAvFade = isAV || type === "image" || type === "camera";

  const tabs: { id: Tab; label: string }[] = [{ id: "basics", label: "Basics" }, { id: "triggers", label: "Triggers" }];
  if (type === "fade") tabs.push({ id: "fade-cue", label: "Fade" });
  if (type === "stop") tabs.push({ id: "stop", label: "Stop" });
  if (isCommandCueType(type)) tabs.push({ id: "command", label: "Command" });
  if (type === "devamp") tabs.push({ id: "devamp", label: "Devamp" });
  if (type === "group") tabs.push({ id: "group", label: "Group" });
  if (type === "group" && groupMode === "simultaneous") tabs.push({ id: "timeline", label: "Timeline" });
  if (type === "camera") tabs.push({ id: "camera", label: "Camera" });
  if (type === "osc" || type === "midi") tabs.push({ id: "messages", label: "Messages" });
  if (type === "midi_file") tabs.push({ id: "midi-file", label: "MIDI File" });
  if (type === "memo") tabs.push({ id: "memo", label: "Memo" });
  if (type === "light") tabs.push({ id: "light", label: "Light" });
  if (type === "mic") tabs.push({ id: "mic", label: "Mic" });
  if (type === "timecode") tabs.push({ id: "timecode", label: "Timecode" });
  if (type === "text") tabs.push({ id: "text", label: "Text" });
  if (type === "script") tabs.push({ id: "script", label: "Script" });
  tabs.push({ id: "time", label: "Time" });
  if (isAV) tabs.push({ id: "levels", label: "Levels" });
  if (type === "audio") tabs.push({ id: "trim", label: "Trim" }, { id: "audio-effects", label: "Audio FX" });
  if (hasAvFade) tabs.push({ id: "fade", label: "Fade" });
  if (isVisual) tabs.push({ id: "layer", label: "Layer" });
  if (isVisual) tabs.push({ id: "geometry", label: "Geometry" });
  return tabs;
}

export function InspectorPanel({ selectedCue, selectedCueIds, onRefresh, onOpenEditor, onOpenCurveEditor, reloadToken, onCueSaved }: Props) {
  const [cueData, setCueData] = useState<CueData | null>(null);
  const [activeTab, setActiveTab] = useState<Tab>("basics");
  const [copiedMidiCurve, setCopiedMidiCurve] = useState<CurveShape | null>();

  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const [saveError, setSaveError] = useState<{ cueId: string; message: string } | null>(null);
  const pendingSaves = useRef<Promise<void>>(Promise.resolve());
  const panelId = useId();
  const id = selectedCue?.id;
  const cueType = selectedCue?.cue_type;
  const groupMode = selectedCue?.group_mode;
  const workspaceId = useWorkspaceStore(state => state.workspaceInfo?.instance_id);
  const activeListId = useWorkspaceStore(state => state.activeCueListId);

  useEffect(() => {
    let current = true;
    setError("");
    setCueData(null);
    if (!id || !cueType) return;
    const available = tabsFor(cueType, groupMode);
    setActiveTab(prev => available.some(tab => tab.id === prev) ? prev : "basics");
    // Wait for any edits committed just before switching or reloading the inspector.
    pendingSaves.current.then(() => getCue(id)).then(data => {
      if (current) setCueData({ ...data, cue_type: cueType } as CueData);
    }).catch(e => { if (current) setError(String(e)); });
    return () => { current = false; };
  }, [id, cueType, groupMode, reloadToken, retry]);

  if (!selectedCue || !cueData || cueData.id !== selectedCue.id) {
    return <div className="inspector-empty" role="status">
      {error ? <><p>{error}</p><button onClick={() => setRetry(n => n + 1)}>Reload inspector</button></>
        : selectedCue ? "Loading cue…" : "Select a cue to inspect it."}
    </div>;
  }

  const type = selectedCue.cue_type;
  const isAudio = type === "audio";
  const isVideo = type === "video";
  const isImage = type === "image";
  const isWait  = type === "wait";
  const isFade  = type === "fade";
  const isCamera = type === "camera";
  const isMidiFile = type === "midi_file";

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const save = (partial: Partial<CueData>, nativePatch: CueProperties = partial) => {
    const cueId = cueData.id;
    // Preserve commit order when an operator tabs rapidly between fields.
    // Immediate local updates keep successive matrix edits based on the latest edit.
    setCueData(prev => prev?.id === cueId ? { ...prev, ...partial } as CueData : prev);
    const write = async () => {
      if ("color" in partial && selectedCueIds.length > 1) {
        const { color, ...rest } = partial;
        await Promise.all(selectedCueIds.map(selectedId => updateCue(selectedId, { color })));
        if (Object.keys(rest).length) await updateCue(cueId, rest);
      } else {
        await updateCue(cueId, nativePatch);
      }
      onRefresh();
      onCueSaved?.();
    };
    pendingSaves.current = pendingSaves.current.then(write).catch(e => {
      setSaveError({ cueId, message: String(e) });
    });
    return pendingSaves.current;
  };

  // Point mixes can fail if another window edits the map or the patch. Read
  // back the baked matrix only after the backend accepts the applied revision.
  const savePointMix: React.ComponentProps<typeof LevelsTab>["onPointMix"] = (point_mix, workspaceId) => {
    const cueId = cueData.id;
    const write = pendingSaves.current.then(async () => {
      await updateCue(cueId, { point_mix, audio_map_workspace: workspaceId });
      const data = await getCue(cueId);
      setCueData(prev => prev?.id === cueId ? { ...data, cue_type: "audio" } as CueData : prev);
      onRefresh();
      onCueSaved?.();
    });
    pendingSaves.current = write.catch(() => {});
    return write;
  };

  const saveOutputTrim: React.ComponentProps<typeof AudioTrimTab>["onSaveOutputs"] = (output_trim, workspaceId) => {
    const cueId = cueData.id;
    const write = pendingSaves.current.then(async () => {
      await updateCue(cueId, { output_trim, output_trim_workspace: workspaceId });
      setCueData(prev => prev?.id === cueId && prev.cue_type === "audio" ? { ...prev, output_trim } : prev);
      onRefresh();
      onCueSaved?.();
    });
    pendingSaves.current = write.catch(() => {});
    return write;
  };

  const browseMedia = (kind: "audio" | "video" | "image" | "midi") => async () => {
    const filters = {
      audio: { name: "Audio Files", extensions: [...AUDIO_EXTENSIONS] },
      video: { name: "Video Files", extensions: [...VIDEO_EXTENSIONS] },
      image: { name: "Image Files", extensions: [...IMAGE_EXTENSIONS] },
      midi: { name: "MIDI Files", extensions: [...MIDI_EXTENSIONS] },
    }[kind];
    const setFile = { audio: setAudioFile, video: setVideoFile, image: setImageFile, midi: setMidiFile }[kind];
    try {
      const result = await open({ multiple: false, filters: [filters] });
      if (typeof result !== "string") return;
      await pendingSaves.current;
      await setFile(cueData.id, result);
      const data = await getCue(cueData.id);
      setCueData(prev => prev?.id === cueData.id ? { ...data, cue_type: cueData.cue_type } as CueData : prev);
      onRefresh();
      onCueSaved?.();
    } catch (e) {
      setSaveError({ cueId: cueData.id, message: String(e) });
    }
  };

  const tabs = tabsFor(type, groupMode);
  const typeLabel = type === "osc" ? "Network / OSC" : type === "midi_file" ? "MIDI file"
    : type === "midi" ? "MIDI" : type[0].toUpperCase() + type.slice(1);

  const moveTab = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let next: number;
    switch (event.key) {
      case "ArrowLeft": next = (index - 1 + tabs.length) % tabs.length; break;
      case "ArrowRight": next = (index + 1) % tabs.length; break;
      case "Home": next = 0; break;
      case "End": next = tabs.length - 1; break;
      default: return;
    }
    event.preventDefault();
    event.stopPropagation();
    setActiveTab(tabs[next].id);
    document.getElementById(`${panelId}-${tabs[next].id}`)?.focus();
  };

  return (
    <section className="cue-inspector" aria-label="Cue inspector" onKeyDown={event => {
      // A focused editor, tab or button handles its own Space and navigation keys.
      const target = event.target as HTMLElement;
      if (event.ctrlKey || event.metaKey || ["Escape", "F5", "F9"].includes(event.key)) return;
      if (target.closest("input, textarea, select, [contenteditable=true]")
        || (target.closest("button") && [" ", "Enter"].includes(event.key))) event.stopPropagation();
    }}>
      <header className="inspector-heading">
        <span className="inspector-cue-number" aria-label={`Cue ${selectedCue.number ?? "unnumbered"}`}
          style={{ borderInlineStartColor: CUE_TYPE_COLORS[type] }}>{selectedCue.number || "—"}</span>
        <div><span className="inspector-type">{typeLabel} cue</span>
          <h2 title={cueData.name}>{cueData.name || "Untitled cue"}</h2></div>
        {selectedCueIds.length > 1 && <small>{selectedCueIds.length} selected</small>}
      </header>
      <div className="inspector-tabs" role="tablist" aria-label="Cue properties">
        {tabs.map((tab, index) => <button type="button" role="tab" key={tab.id}
          id={`${panelId}-${tab.id}`} aria-selected={activeTab === tab.id} tabIndex={activeTab === tab.id ? 0 : -1}
          aria-controls={`${panelId}-contents`} onClick={() => setActiveTab(tab.id)}
          onKeyDown={event => moveTab(event, index)}>{tab.label}</button>)}
      </div>
      {saveError?.cueId === cueData.id && <div className="inspector-error" role="alert">
        Changes could not be saved. {saveError.message}
        <button onClick={() => { setSaveError(null); setRetry(n => n + 1); }}>Reload saved values</button>
      </div>}
      <div className="inspector-contents" role="tabpanel" id={`${panelId}-contents`}
        aria-labelledby={`${panelId}-${activeTab}`} tabIndex={0}>
        {activeTab === "basics" && (
          <BasicsTab
            cue={cueData}
            isAudio={isAudio}
            isVideo={isVideo}
            isImage={isImage}
            isMidiFile={isMidiFile}
            onSave={save}
            onBrowse={browseMedia("audio")}
            onBrowseVideo={browseMedia("video")}
            onBrowseImage={browseMedia("image")}
            onBrowseMidi={browseMedia("midi")}
          />
        )}
        {activeTab === "fade-cue" && isFade && (
          <FadeCueTab
            cue={cueData as FadeCueData}
            onSave={save}
            onOpenCurveEditor={
              onOpenCurveEditor ? () => onOpenCurveEditor(cueData.id) : undefined
            }
          />
        )}
        {activeTab === "script" && type === "script" && (
          <ScriptTab cue={cueData as unknown as ScriptCueData} onSave={save} />
        )}
        {activeTab === "command" && isCommandCueType(type) && (
          <CommandTab cue={cueData as StopCueData} onSave={save} />
        )}
        {activeTab === "stop" && type === "stop" && (
          <StopTab cue={cueData as StopCueData} onSave={save} />
        )}
        {activeTab === "devamp" && type === "devamp" && (
          <DevampTab cue={cueData as DevampCueData} onSave={save} />
        )}
        {activeTab === "group" && type === "group" && (
          <GroupTab cue={cueData}
            isPlaying={selectedCue?.state === "running" || selectedCue?.state === "paused"}
            playlistSummary={selectedCue}
            onRefresh={onRefresh} />
        )}
        {activeTab === "timeline" && type === "group" && groupMode === "simultaneous" && (
          <GroupTimelineTab key={[workspaceId, activeListId, selectedCue.id].join(":")}
            group={selectedCue} workspaceId={workspaceId} listId={activeListId}
            onRefresh={onRefresh} />
        )}
        {activeTab === "time" && (
          <TimeTab
            cue={cueData}
            selectedCue={selectedCue}
            isAudio={isAudio}
            isVideo={isVideo}
            isImage={isImage}
            isWait={isWait}
            isFade={isFade}
            onSave={save}
            onOpenWaveform={() => onOpenEditor?.(cueData.id)}
          />
        )}
        {activeTab === "levels" && (isAudio || isVideo) && (
          <LevelsTab cue={cueData as AudioCueData | VideoCueData} isAudio={isAudio} onSave={save} onPointMix={savePointMix} />
        )}
        {activeTab === "trim" && isAudio && (
          <AudioTrimTab key={cueData.id} cue={cueData as AudioCueData} onSaveMain={trim_db => save({ trim_db })}
            onSaveOutputs={saveOutputTrim} />
        )}
        {activeTab === "audio-effects" && isAudio && (
          <AudioEffectsTab cue={cueData as AudioCueData} onSave={save} />
        )}
        {activeTab === "fade" && (isAudio || isVideo || isImage || isCamera) && (
          <FadeTab cue={cueData as AudioCueData | VideoCueData | ImageCueData | CameraCueData} onSave={save} />
        )}
        {activeTab === "layer" && (isVideo || isImage || isCamera) && (
          <LayerTab cue={cueData as VideoCueData | ImageCueData | CameraCueData} onSave={save} />
        )}
        {activeTab === "geometry" && (isVideo || isImage || isCamera) && (
          <GeometryTab cue={cueData as VideoCueData | ImageCueData | CameraCueData} onSave={save} />
        )}
        {activeTab === "camera" && isCamera && (
          <CameraTab cue={cueData as CameraCueData} onSave={save} />
        )}
        {activeTab === "messages" && type === "osc" && (
          <OscTab cue={cueData as OscCueData} onSave={save} />
        )}
        {activeTab === "messages" && type === "midi" && (
          <MidiTab cue={cueData as MidiCueData} onSave={save}
            copiedCurve={copiedMidiCurve} onCopyCurve={setCopiedMidiCurve} />
        )}
        {activeTab === "midi-file" && isMidiFile && (
          <MidiFileTab cue={cueData as unknown as MidiFileCueData} onSave={save} />
        )}
        {activeTab === "memo" && type === "memo" && (
          <MemoTab cue={cueData as unknown as MemoCueData} onSave={save} />
        )}
        {activeTab === "light" && type === "light" && (
          <LightTab cue={cueData as LightCueData} onSave={save} />
        )}
        {activeTab === "mic" && type === "mic" && (
          <MicTab cue={cueData as MicCueData} onSave={save} />
        )}
        {activeTab === "timecode" && type === "timecode" && (
          <TimecodeTab cue={cueData as TimecodeCueData} onSave={save} />
        )}
        {activeTab === "text" && type === "text" && (
          <TextTab cue={cueData as TextCueData} onSave={save} />
        )}
        {activeTab === "triggers" && (
          <TriggersTab cue={selectedCue} onSave={onRefresh} />
        )}
      </div>

    </section>
  );
}
