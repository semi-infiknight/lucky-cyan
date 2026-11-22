// Fade Cue main tab: targets, fade parameters, audio/visual goals, on-complete.
// Extracted from BasicsTab so Basics stays identity-only.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getOutputPatches, revertPictureFade } from "../../lib/commands";
import type { CueSummary, FadeCueData, FadeShapes, OutputPatch } from "../../lib/types";
import { CurveEditor } from "../Curve/CurveEditor";

/** Locked S-Curve — what a fade has always done, for a cue saved before
 *  shapes existed and re-read before the backend fills them in. */
const DEFAULT_FADE_SHAPES: FadeShapes = {
  up: { kind: "s_curve", intensity: 0, points: [], bends: [] },
  down: { kind: "s_curve", intensity: 0, points: [], bends: [] },
  mirrored: true,
};
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { flattenCues, makesSound, showsPicture } from "../../lib/cueTree";
import { Grid2, MiniField, NumberInput, Section, SliderRow, ToggleRow, inputStyle } from "./Field";
import { CueTargetPicker } from "./CueTargetPicker";
import { MapMotionFields } from "./MapMotionFields";
import { Select } from "../common/Select";
import { PositionWaypoints } from "./PositionWaypoints";
import { AudioEffectFadeSection } from "./AudioEffectFadeSection";
import { PictureSaturationPreview } from "./PictureSaturationPreview";

const hintStyle = { fontSize: 11, color: "var(--wc-text-faint)", margin: "6px 0 10px", lineHeight: 1.5 } as const;

const colorFadeFields = [
  { key: "target_picture_brightness", label: "picture brightness", initial: 0, min: -1, max: 1, step: 0.01 },
  { key: "target_contrast", label: "contrast", initial: 1, min: 0.25, max: 4, step: 0.01 },
  { key: "target_hue_degrees", label: "hue angle", initial: 0, min: -180, max: 180, step: 1 },
] as const;
const clearColorFade = {
  target_saturation: null, target_picture_brightness: null, target_contrast: null, target_hue_degrees: null,
};

export function FadeCueTab({
  cue,
  onSave,
  onOpenCurveEditor,
}: {
  cue: FadeCueData;
  onSave: (p: Partial<FadeCueData>) => void;
  /** Opens the full-size curve editor in the dock under the cue list. */
  onOpenCurveEditor?: () => void;
}) {
  const allCues = useWorkspaceStore((s) => s.cues);

  const [patches, setPatches] = useState<OutputPatch[]>([]);
  useEffect(() => {
    let mounted = true;
    const refresh = () => { void getOutputPatches().then(rows => { if (mounted) setPatches(rows); }).catch(console.error); };
    const listener = listen("output-patches-changed", refresh);
    void listener.then(() => { if (mounted) refresh(); }).catch(console.error);
    return () => { mounted = false; void listener.then(stop => stop()).catch(console.error); };
  }, []);

  const everyCue = flattenCues(allCues);
  const fadeState = everyCue.find(summary => summary.id === cue.id)?.state;
  const targetIds: string[] = cue.target_cue_ids ?? [];
  const targetCues = targetIds
    .map((id) => everyCue.find((c) => c.id === id))
    .filter((c): c is CueSummary => !!c);
  // Groups count through their children: a group holding a video is both.
  const hasSound = targetCues.some(makesSound);
  const hasPicture = targetCues.some(showsPicture);
  const noTarget = targetCues.length === 0;
  const crossfadeInto = cue.crossfade_into_id ?? null;
  const waypoints = cue.translation_waypoints ?? [];
  const fadeVolume: boolean = cue.fade_volume ?? true;
  const panEnabled: boolean = cue.target_pan != null;
  // Show configured volume or pan so either can be turned off for a position-only path.
  const showVolume = hasSound || noTarget || (waypoints.length > 0 && (fadeVolume || panEnabled));
  const hasFileAudio = flattenCues(targetCues).some(target => target.cue_type === "audio");
  const showRate = hasFileAudio || noTarget || cue.target_rate != null;
  // Show visual goals when targets show a picture, or no target yet.
  const showBrightness = hasPicture || noTarget;

  // Anything with a picture can be dissolved into, nested in a group or not.
  const pictureCues = everyCue.filter(
    (c) => c.id !== cue.id && (c.cue_type === "video" || c.cue_type === "image" || c.cue_type === "camera"),
  );
  const volDb: number = cue.target_volume_db ?? -60;
  const brightnessPercent: number = cue.target_brightness_pct ?? 0;
  const panValue: number = cue.target_pan ?? 0;
  const rateValue = cue.target_rate ?? 1;
  const audioTarget = targetIds.length === 1 && targetCues[0]?.cue_type === "audio" ? targetIds[0] : undefined;
  const effect = cue.target_eq_gain ? { kind: "eq_gain", ...cue.target_eq_gain } as const
    : cue.target_cutoff ? { kind: "cutoff", ...cue.target_cutoff } as const : null;
  const effectOnly = effect != null;
  const mapMotionOnly = cue.target_map_motion != null;
  const silentSpeed = cue.target_silent_speed ?? null;
  const silentSpeedTarget = targetIds.length === 1 && targetCues[0]?.cue_type === "video";
  const dedicatedFade = effectOnly || mapMotionOnly || silentSpeed != null;
  const relative = cue.target_relative_geometry ?? null;
  const position = relative ? relative.translation : cue.target_translation;
  const translation = position ?? { x: null, y: null };
  const scale = relative ? relative.scale_factor : cue.target_scale ?? null;
  const scaleLabel = relative ? "Scale multiplier (%)" : "Destination scale (%)";
  const specifiedTurn = cue.target_rotation_delta_degrees != null;
  const rotation = cue.target_rotation_delta_degrees ?? cue.target_rotation_degrees ?? null;
  const rotationLabel = specifiedTurn ? "Turn by (°)" : "Destination angle (°)";
  const saveRotation = (value: number | null, turn = specifiedTurn) => onSave({
    target_rotation_degrees: turn ? null : value,
    target_rotation_delta_degrees: turn ? value : null,
  });
  const saturation = cue.target_saturation ?? null;
  const hasColorFade = saturation != null || colorFadeFields.some(({ key }) => cue[key] != null);
  const loopPath = cue.loop_translation_path ?? false;
  const durationMs = cue.fade_duration_ms ?? 2000;
  const canFadeGeometry = crossfadeInto === null && targetIds.length === 1 &&
    pictureCues.some((picture) => picture.id === targetIds[0]);
  const canFadeRotation = canFadeGeometry && targetCues[0]?.still_rotation_target === true;
  const canLoopPath = !fadeVolume && !(cue.fade_brightness ?? true) && !panEnabled && cue.target_rate == null
    && scale == null && rotation == null && !hasColorFade && cue.patch_target_id == null
    && !cue.stop_at_end && cue.continue_mode === "do_not_continue";
  const savePosition = (next: typeof translation | null) => {
    onSave(relative ? { target_relative_geometry: { ...relative, translation: next } } : { target_translation: next });
  };
  const saveScale = (next: number | null) => {
    onSave(relative ? { target_relative_geometry: { ...relative, scale_factor: next } } : { target_scale: next });
  };
  const setTranslation = (axis: "x" | "y", destination: number | null) => {
    const next = { ...translation, [axis]: destination };
    savePosition(next.x == null && next.y == null ? null : next);
  };

  return (
    <>
      <Section title="Targets">
        <CueTargetPicker
          allCues={allCues}
          selfId={cue.id}
          selectedIds={targetIds}
          filterTypes={["audio", "video", "image", "camera", "group"]}
          onChange={(ids) => {
            const nums = ids
              .map((id) => allCues.find((c) => c.id === id)?.number)
              .filter((n): n is string => n != null);
            onSave({
              target_cue_ids: ids,
              target_cue_numbers: nums,
              ...(effectOnly && (ids.length !== 1 || ids[0] !== audioTarget)
                ? { target_cutoff: null, target_eq_gain: null } : {}),
              ...(mapMotionOnly && (ids.length !== 1 || ids[0] !== audioTarget) ? { target_map_motion: null } : {}),
              ...(silentSpeed != null && (ids.length !== 1 || everyCue.find(c => c.id === ids[0])?.cue_type !== "video")
                ? { target_silent_speed: null } : {}),
            });
          }}
        />
        <div style={{ height: 8 }} />
      </Section>

      {((audioTarget && !effectOnly) || mapMotionOnly) && (
        <MapMotionFields
          targetId={audioTarget}
          motion={cue.target_map_motion}
          onApply={(motion) => onSave(motion ? {
            target_map_motion: motion, target_cutoff: null, target_eq_gain: null,
            fade_volume: false, fade_brightness: false, target_pan: null, target_rate: null,
            target_translation: null, translation_waypoints: [], target_scale: null,
            target_relative_geometry: null,
            target_rotation_degrees: null, target_rotation_delta_degrees: null, ...clearColorFade,
            patch_target_id: null, crossfade_into_id: null,
            continue_mode: "do_not_continue", stop_at_end: false,
          } : { target_map_motion: null })}
        />
      )}

      {((audioTarget && !mapMotionOnly) || effectOnly) && (
        <AudioEffectFadeSection targetId={audioTarget} destination={effect}
          onChange={destination => {
            const targets = {
              target_cutoff: destination?.kind === "cutoff"
                ? { filter_id: destination.filter_id, frequency_hz: destination.frequency_hz } : null,
              target_eq_gain: destination?.kind === "eq_gain"
                ? { filter_id: destination.filter_id, gain_db: destination.gain_db } : null,
            };
            if (destination?.kind === "eq_gain") {
              onSave({
                ...targets,
                fade_volume: false, fade_brightness: false, stop_at_end: false,
                patch_target_id: null, target_pan: null, target_rate: null, target_silent_speed: null,
                target_translation: null, translation_waypoints: [], target_scale: null,
                target_rotation_degrees: null, target_rotation_delta_degrees: null, ...clearColorFade,
                target_map_motion: null, crossfade_into_id: null,
              });
            } else {
              onSave(destination ? {
                ...targets, target_map_motion: null, target_rotation_degrees: null, target_rotation_delta_degrees: null, target_silent_speed: null,
              } : targets);
            }
          }} />
      )}

      {((silentSpeedTarget && !effectOnly && !mapMotionOnly) || silentSpeed != null) && (
        <Section title="Silent movie speed">
          <ToggleRow label="Fade silent movie speed" checked={silentSpeed != null}
            onToggle={enabled => onSave(enabled ? {
              target_silent_speed: 1, target_cutoff: null, target_eq_gain: null, target_map_motion: null,
              fade_volume: false, fade_brightness: false, target_pan: null, target_rate: null,
              target_translation: null, translation_waypoints: [], target_scale: null,
              target_relative_geometry: null,
              target_rotation_degrees: null, target_rotation_delta_degrees: null, ...clearColorFade,
              patch_target_id: null, crossfade_into_id: null, continue_mode: "do_not_continue", stop_at_end: false,
            } : { target_silent_speed: null })}>
            <Grid2>
              <MiniField label="Destination speed (×)">
                <NumberInput ariaLabel="Destination movie speed (×)" value={silentSpeed}
                  min={0.25} max={4} step={0.01} onCommit={target_silent_speed => onSave({ target_silent_speed })} />
              </MiniField>
            </Grid2>
            <div style={hintStyle}>Enable silent movie speed in the Video cue’s Time inspector and start its picture first.</div>
          </ToggleRow>
        </Section>
      )}

      {!dedicatedFade && (hasPicture || noTarget || crossfadeInto !== null) && (
        <Section title="Crossfade">
          <Select
            style={inputStyle}
            value={crossfadeInto ?? ""}
            onChange={(e) => onSave({ crossfade_into_id: e.target.value || null })}
          >
            <option value="">None — plain fade</option>
            {pictureCues.map((c) => (
              <option key={c.id} value={c.id}>
                {c.number ? `${c.number} — ` : ""}{c.name || "(untitled)"}
              </option>
            ))}
            {crossfadeInto !== null && !pictureCues.some((c) => c.id === crossfadeInto) && (
              <option value={crossfadeInto}>(missing cue)</option>
            )}
          </Select>
          <div style={hintStyle}>
            {crossfadeInto
              ? "GO starts this cue. From its first frame, the targets' pictures dissolve into it over the fade time, along the curve — on another output they fade out on their own screen. Their sound fades out with them, and they stop when the dissolve ends."
              : "Pick a Video, Image or Camera cue to dissolve the targets into, without the dip to black of two crossing fades."}
          </div>
        </Section>
      )}

      {!dedicatedFade && <Section title="Output patch master">
        <Grid2>
          <MiniField label="Patch">
            <Select style={inputStyle} value={cue.patch_target_id ?? ""}
              onChange={e => onSave({ patch_target_id: e.target.value || null })}>
              <option value="">None</option>
              {patches.map(patch => <option key={patch.id} value={patch.id}>{patch.name}</option>)}
              {cue.patch_target_id && !patches.some(patch => patch.id === cue.patch_target_id) &&
                <option value={cue.patch_target_id}>Missing output patch</option>}
            </Select>
          </MiniField>
          {cue.patch_target_id && <MiniField label="Patch gain (dB)">
            <NumberInput value={cue.patch_target_db ?? 0} min={-60} max={12} step={0.5}
              onCommit={patch_target_db => onSave({ patch_target_db })} />
          </MiniField>}
        </Grid2>
      </Section>}

      <Section title="Fade">
        <Grid2>
          <MiniField label="Time (s)">
            <NumberInput
              value={durationMs / 1000}
              step={0.1}
              min={waypoints.length ? 0.001 : 0}
              max={3600}
              onCommit={(v) => onSave({ fade_duration_ms: Math.round(v * 1000) })}
            />
          </MiniField>
        </Grid2>
      </Section>

      <Section title="Curve">
        <CurveEditor
          compact
          shapes={cue.fade_shapes ?? DEFAULT_FADE_SHAPES}
          onChange={(fade_shapes) => onSave({ fade_shapes })}
        />
        {onOpenCurveEditor && (
          <button
            onClick={onOpenCurveEditor}
            title="Open the curve editor below the cue list"
            style={{
              marginTop: 8, padding: "3px 10px",
              background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)",
              borderRadius: 4, color: "var(--wc-text-secondary)", fontSize: 11, cursor: "pointer",
            }}
          >
            ⤢ Open curve editor
          </button>
        )}
      </Section>

      {!dedicatedFade && (showVolume || showRate) && (
        <Section title="Audio">
          {showVolume && <ToggleRow
            label="Fade volume"
            checked={fadeVolume}
            onToggle={(v) => onSave({ fade_volume: v })}
          >
            <Grid2>
              <MiniField label="To (dB)">
                <NumberInput
                  value={volDb}
                  step={0.5}
                  min={-60}
                  max={12}
                  onCommit={(v) => onSave({ target_volume_db: v })}
                />
              </MiniField>
            </Grid2>
          </ToggleRow>}
          {showVolume && <ToggleRow
            label="Fade pan"
            checked={panEnabled}
            onToggle={(v) => onSave({ target_pan: v ? panValue : null })}
          >
            <SliderRow
              label="To"
              value={panValue}
              min={-1}
              max={1}
              step={0.01}
              format={(v) => (v === 0 ? "C" : `${v > 0 ? "R" : "L"}${Math.round(Math.abs(v) * 100)}`)}
              onChange={(v) => onSave({ target_pan: v })}
            />
          </ToggleRow>}
          {showRate && <ToggleRow
            label="Fade playback rate"
            checked={cue.target_rate != null}
            onToggle={enabled => onSave({ target_rate: enabled ? rateValue : null })}
          >
            <Grid2>
              <MiniField label="To (×)">
                <NumberInput
                  value={rateValue}
                  min={0.03}
                  max={33}
                  step={0.01}
                  onCommit={target_rate => onSave({ target_rate })}
                />
              </MiniField>
            </Grid2>
            <div style={hintStyle}>
              {hasFileAudio || noTarget
                ? "Changes speed and pitch of Audio cues. 1× is normal speed."
                : "Select an Audio cue or a Group containing Audio cues."}
            </div>
          </ToggleRow>}
        </Section>
      )}

      {!dedicatedFade && (showBrightness || hasColorFade) && (
        <Section
          title="Visual"
          hint={crossfadeInto ? "Used only if there is nothing to dissolve into." : undefined}
        >
          {showBrightness && <ToggleRow
            label="Fade brightness"
            checked={cue.fade_brightness ?? true}
            onToggle={(enabled) => onSave({ fade_brightness: enabled })}
          >
            <SliderRow
              label="To"
              value={brightnessPercent}
              min={0}
              max={100}
              step={1}
              format={(v) => `${Math.round(v)}%`}
              onChange={(v) => onSave({ target_brightness_pct: Math.round(v) })}
            />
          </ToggleRow>}
          {canFadeGeometry ? (
            <>
            <ToggleRow label="Fade saturation" checked={saturation != null}
              onToggle={(enabled) => onSave({ target_saturation: enabled ? 0 : null })}>
              <Grid2>
                <MiniField label="Destination saturation (%)">
                  <NumberInput ariaLabel="Destination saturation (%)"
                    value={saturation == null ? null : Number((saturation * 100).toPrecision(15))}
                    min={0} max={200} step={1} onCommit={(value) => onSave({ target_saturation: value / 100 })} />
                </MiniField>
              </Grid2>
              {saturation != null && targetCues[0] && <PictureSaturationPreview fadeId={cue.id}
                target={targetCues[0]} saturation={saturation} fadeRunning={fadeState === "running" || fadeState === "paused"} />}
            </ToggleRow>
            {colorFadeFields.map(({ key, label, initial, min, max, step }) => (
              <ToggleRow key={key} label={`Fade ${label}`} checked={cue[key] != null}
                onToggle={enabled => onSave({ [key]: enabled ? initial : null })}>
                <Grid2>
                  <MiniField label={`Destination ${label}`}>
                    <NumberInput ariaLabel={`Destination ${label}`} value={cue[key] ?? null}
                      min={min} max={max} step={step} onCommit={value => onSave({ [key]: value })} />
                  </MiniField>
                </Grid2>
              </ToggleRow>
            ))}
            </>
          ) : hasColorFade && (
            <>
              <div style={hintStyle}>Select one Video, Image or Camera target and set Crossfade to None.</div>
              <button type="button" onClick={() => onSave(clearColorFade)}>Clear color fades</button>
            </>
          )}
        </Section>
      )}

      {!dedicatedFade && (hasPicture || noTarget || position != null || scale != null || rotation != null) && (
        <Section title="Position, scale and rotation">
          {canFadeGeometry ? (
            <>
              <Grid2>
                <MiniField label="Position and scale values">
                  <Select aria-label="Position and scale values" style={inputStyle}
                    value={relative ? "relative" : "absolute"}
                    onChange={(event) => onSave(event.target.value === "relative" ? {
                      target_translation: null, target_scale: null,
                      target_relative_geometry: { translation: position ?? null, scale_factor: scale },
                    } : {
                      target_relative_geometry: null,
                      target_translation: position ?? null, target_scale: scale,
                    })}>
                    <option value="absolute">Absolute</option>
                    <option value="relative" disabled={waypoints.length > 0}>Relative</option>
                  </Select>
                </MiniField>
              </Grid2>
              {waypoints.length > 0 && <div style={hintStyle}>Remove the waypoints to switch to relative position.</div>}
              {relative && <div style={hintStyle}>Offsets and scale use the picture’s live values when the Fade starts.</div>}
              <ToggleRow label="Fade scale" checked={scale != null}
                onToggle={(enabled) => saveScale(enabled ? 1 : null)}>
                <Grid2>
                  <MiniField label={scaleLabel}>
                    <NumberInput ariaLabel={scaleLabel}
                      value={scale == null ? null : Number((scale * 100).toPrecision(15))} min={5} max={800} step={1}
                      onCommit={(value) => saveScale(value / 100)} />
                  </MiniField>
                </Grid2>
              </ToggleRow>
              {(["x", "y"] as const).map((axis) => {
                const label = `${relative ? "Offset" : "Destination"} ${axis.toUpperCase()}`;
                const destination = <Grid2>
                  <MiniField label={`${label} (fraction of ${axis === "x" ? "width" : "height"})`}>
                    <NumberInput ariaLabel={label}
                      value={translation[axis]} step={0.01} min={-1} max={1}
                      onCommit={(value) => setTranslation(axis, value)} />
                  </MiniField>
                </Grid2>;
                return waypoints.length ? <div key={axis}>{destination}</div> : (
                  <ToggleRow key={axis} label={axis === "x" ? "Move horizontally" : "Move vertically"}
                    checked={translation[axis] != null} onToggle={(enabled) => setTranslation(axis, enabled ? 0 : null)}>
                    {destination}
                  </ToggleRow>
                );
              })}
              {!relative && (translation.x != null && translation.y != null ? (
                durationMs > 0 ? <PositionWaypoints points={waypoints} destination={[translation.x, translation.y]}
                  smooth={cue.smooth_translation_path ?? false}
                  looping={loopPath} canLoop={canLoopPath}
                  onSmoothChange={smooth_translation_path => onSave({ smooth_translation_path })}
                  onLoopChange={loop_translation_path => onSave({ loop_translation_path })}
                  onChange={translation_waypoints => onSave({ translation_waypoints })} /> :
                  <div style={hintStyle}>Set a positive Fade duration to add a waypoint.</div>
              ) : translation.x != null || translation.y != null ? (
                <div style={hintStyle}>Enable both destination axes to add waypoints.</div>
              ) : null)}
            </>
          ) : (
            <>
              <div style={hintStyle}>Select one Video, Image or Camera target and set Crossfade to None.</div>
              {position != null && (
                <button type="button" onClick={() => {
                  if (relative) savePosition(null);
                  else onSave({ target_translation: null, translation_waypoints: [] });
                }}>Clear position fade</button>
              )}
              {scale != null && (
                <button type="button" onClick={() => saveScale(null)}>Clear scale fade</button>
              )}
            </>
          )}
          {canFadeRotation ? (
            <ToggleRow label="Fade rotation around pivot" checked={rotation != null}
              onToggle={enabled => saveRotation(enabled ? 0 : null)}>
              <Grid2>
                <MiniField label="Rotation motion">
                  <Select aria-label="Rotation motion" style={inputStyle}
                    value={specifiedTurn ? "turn" : "angle"}
                    onChange={event => saveRotation(0, event.target.value === "turn")}>
                    <option value="angle">Shortest path to angle</option>
                    <option value="turn">Specified turn</option>
                  </Select>
                </MiniField>
                <MiniField label={rotationLabel}>
                  <NumberInput ariaLabel={rotationLabel} value={rotation}
                    min={specifiedTurn ? -3600 : 0} max={specifiedTurn ? 3600 : 359.9}
                    step={0.1} onCommit={value => saveRotation(value)} />
                </MiniField>
              </Grid2>
              <div style={hintStyle}>{specifiedTurn
                ? "Positive values turn clockwise from the live angle. Enter 360° for one full turn."
                : "The picture takes the shorter turn from its live angle around its saved pivot."}</div>
            </ToggleRow>
          ) : (rotation != null || (canFadeGeometry && targetCues[0]?.cue_type === "image")) && (
            <>
              <div style={hintStyle}>Select one PNG or JPEG Image with continuous still rotation enabled and set Crossfade to None.</div>
              {rotation != null &&
                <button type="button" onClick={() => saveRotation(null)}>Clear rotation fade</button>}
            </>
          )}
        </Section>
      )}

      {!dedicatedFade && !loopPath && !cue.stop_at_end && crossfadeInto === null &&
        (position != null || scale != null || rotation != null || hasColorFade) && (
          <RevertPictureFade key={cue.id} cueId={cue.id} running={fadeState === "running" || fadeState === "paused"} />
        )}

      {!dedicatedFade && !loopPath && <Section
        title="On Complete"
        hint={crossfadeInto ? "A crossfade always stops the targets it dissolved away." : undefined}
      >
        <ToggleRow
          label="Stop targets when the fade ends"
          checked={cue.stop_at_end ?? false}
          onToggle={(v) => onSave({ stop_at_end: v })}
        />
      </Section>}
    </>
  );
}

function RevertPictureFade({ cueId, running }: { cueId: string; running: boolean }) {
  const workspaceId = useWorkspaceStore(store => store.workspaceInfo?.instance_id);
  const listId = useWorkspaceStore(store => store.activeCueListId);
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState<{ text: string; error: boolean }>();

  const revert = async () => {
    if (!workspaceId || !listId || busy || running) return;
    setBusy(true);
    setFeedback(undefined);
    try {
      const result = await revertPictureFade(workspaceId, listId, cueId);
      const restored = [
        ...(result.geometry ? ["geometry"] : []),
        ...(result.saturation ? ["saturation"] : []),
        ...result.other_color.map(parameter => parameter === "brightness" ? "picture brightness" : parameter),
      ];
      setFeedback({ text: `Restored the picture's ${restored.join(", ")}.`, error: false });
    } catch (error) {
      setFeedback({ text: String(error), error: true });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title="Live picture" hint="Restore geometry and color settings from before the last completed Fade.">
      <button type="button" disabled={busy || running || !workspaceId || !listId}
        onClick={() => { void revert(); }}>Revert Picture Fade</button>
      {feedback && <div role={feedback.error ? "alert" : "status"} style={hintStyle}>{feedback.text}</div>}
    </Section>
  );
}
