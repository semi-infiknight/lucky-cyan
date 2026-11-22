// Geometry tab for visual cues: fit mode, position, scale, rotation and crop.
// Edits apply live when the cue is on the output window. Compositing controls
// (layer order / opacity / blend) live in the Layer tab.

import type { CameraCueData, FitMode, ImageCueData, VideoCueData, VideoGeometry } from "../../lib/types";
import { DEFAULT_GEOMETRY } from "../../lib/types";
import { Grid2, MiniField, NumberInput, Section, Segmented, SliderRow, ToggleRow } from "./Field";

const FIT_MODES: { value: FitMode; label: string; hint: string }[] = [
  { value: "fit", label: "Fit", hint: "keep aspect, letterbox" },
  { value: "fill", label: "Fill", hint: "keep aspect, crop overflow" },
  { value: "stretch", label: "Stretch", hint: "ignore aspect ratio" },
];

export function GeometryTab({
  cue,
  onSave,
}: {
  cue: VideoCueData | ImageCueData | CameraCueData;
  onSave: (p: Partial<VideoCueData | ImageCueData | CameraCueData>) => void;
}) {
  const geometry: VideoGeometry = cue.geometry ?? DEFAULT_GEOMETRY;
  const patch = (partial: Partial<VideoGeometry>) =>
    onSave({ geometry: { ...geometry, ...partial } });
  const sourceAngle = cue.cue_type === "image" ? geometry.source_rotation_degrees : null;
  const pivot = geometry.source_rotation_pivot ?? [0.5, 0.5];
  const centrePivot = pivot[0] === 0.5 && pivot[1] === 0.5;
  const savePivot = (axis: 0 | 1, percent: number) => {
    if (pivot[axis] === percent / 100) return;
    const next: [number, number] = [pivot[0], pivot[1]];
    next[axis] = percent / 100;
    patch({ source_rotation_pivot: next.every(value => value === 0.5) ? null : next });
  };
  const canChooseSourceRotation = cue.cue_type === "image"
    && (sourceAngle != null || /\.(?:png|jpe?g)$/i.test(cue.file_path ?? ""));

  const isDefault =
    geometry.fit_mode === "fit" &&
    geometry.pan_x === 0 &&
    geometry.pan_y === 0 &&
    geometry.scale === 1 &&
    geometry.rotation === 0 &&
    geometry.source_rotation_degrees == null &&
    geometry.source_rotation_pivot == null &&
    geometry.crop_left === 0 &&
    geometry.crop_right === 0 &&
    geometry.crop_top === 0 &&
    geometry.crop_bottom === 0;

  return (
    <>
      <Section title="Fit" hint="Changes apply live while this cue is on the output window.">
        <Segmented
          options={FIT_MODES}
          value={geometry.fit_mode}
          onChange={(v) => patch({ fit_mode: v })}
        />
        <div style={{ fontSize: 11, color: "var(--wc-text-faint)", marginTop: -4, marginBottom: 6 }}>
          {FIT_MODES.find((m) => m.value === geometry.fit_mode)?.hint}
        </div>
      </Section>

      <Section title="Position & Scale">
        <Grid2>
          <MiniField label="Position X (± of width)">
            <NumberInput
              value={geometry.pan_x}
              step={0.01}
              min={-1}
              max={1}
              onCommit={(v) => patch({ pan_x: v })}
            />
          </MiniField>
          <MiniField label="Position Y (± of height)">
            <NumberInput
              value={geometry.pan_y}
              step={0.01}
              min={-1}
              max={1}
              onCommit={(v) => patch({ pan_y: v })}
            />
          </MiniField>
        </Grid2>
        <SliderRow
          label="Scale"
          value={geometry.scale}
          min={0.05}
          max={8}
          step={0.05}
          format={(v) => `${Math.round(v * 100)}%`}
          onChange={(v) => patch({ scale: v })}
        />
        {canChooseSourceRotation && (
          <ToggleRow
            label="Continuous still rotation"
            checked={sourceAngle != null}
            onToggle={enabled => patch({ source_rotation_degrees: enabled ? geometry.rotation : null })}
          >
            <span style={{ fontSize: 11, color: "var(--wc-text-faint)" }}>
              PNG or JPEG stills with orientation baked in. The mode starts on the next GO.
            </span>
          </ToggleRow>
        )}
        {sourceAngle == null ? (
          <SliderRow
            label="Rotation"
            value={geometry.rotation}
            min={0}
            max={359}
            step={1}
            format={v => `${Math.round(v)}°`}
            onChange={v => patch({ rotation: Math.round(v) })}
          />
        ) : (
          <>
            <SliderRow
              label="Rotation"
              value={sourceAngle}
              min={0}
              max={359.9}
              step={0.1}
              format={v => `${v.toFixed(1)}°`}
              onChange={source_rotation_degrees => patch({ source_rotation_degrees })}
              valueEditor={<NumberInput ariaLabel="Rotation angle (degrees)" width={66} value={sourceAngle}
                min={0} max={359.9} step={0.1}
                onCommit={source_rotation_degrees => {
                  if (source_rotation_degrees !== sourceAngle) patch({ source_rotation_degrees });
                }} />}
            />
            <Grid2>
              <MiniField label="Pivot X (% from left)">
                <NumberInput ariaLabel="Rotation pivot X (percent from left)" value={+(pivot[0] * 100).toFixed(4)}
                  min={0} max={100} step={0.1} onCommit={value => savePivot(0, value)} />
              </MiniField>
              <MiniField label="Pivot Y (% from top)">
                <NumberInput ariaLabel="Rotation pivot Y (percent from top)" value={+(pivot[1] * 100).toFixed(4)}
                  min={0} max={100} step={0.1} onCommit={value => savePivot(1, value)} />
              </MiniField>
            </Grid2>
            <button type="button" disabled={centrePivot} onClick={() => patch({ source_rotation_pivot: null })}>
              Centre pivot
            </button>
          </>
        )}
      </Section>

      <Section title="Crop" hint="Fraction of each edge (0 – 0.45).">
        <Grid2>
          <MiniField label="Left">
            <NumberInput value={geometry.crop_left} step={0.01} min={0} max={0.45}
              onCommit={(v) => patch({ crop_left: v })} />
          </MiniField>
          <MiniField label="Right">
            <NumberInput value={geometry.crop_right} step={0.01} min={0} max={0.45}
              onCommit={(v) => patch({ crop_right: v })} />
          </MiniField>
          <MiniField label="Top">
            <NumberInput value={geometry.crop_top} step={0.01} min={0} max={0.45}
              onCommit={(v) => patch({ crop_top: v })} />
          </MiniField>
          <MiniField label="Bottom">
            <NumberInput value={geometry.crop_bottom} step={0.01} min={0} max={0.45}
              onCommit={(v) => patch({ crop_bottom: v })} />
          </MiniField>
        </Grid2>
      </Section>

      <button
        disabled={isDefault}
        onClick={() => onSave({ geometry: { ...DEFAULT_GEOMETRY } })}
        style={{
          padding: "5px 14px",
          fontSize: 12,
          borderRadius: 4,
          border: "1px solid var(--wc-border-strong)",
          background: "var(--wc-bg-surface)",
          color: isDefault ? "var(--wc-text-faint)" : "var(--wc-text)",
          cursor: isDefault ? "default" : "pointer",
        }}
      >
        Reset to defaults
      </button>
    </>
  );
}
