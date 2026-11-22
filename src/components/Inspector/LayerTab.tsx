// Layer tab for visual cues (Video / Image / Camera): which output the cue
// plays on and how it composites with the other layers there — stacking
// order, opacity, blend mode and per-source color.

import type { BlendMode, CameraCueData, ImageCueData, LayerStyle, VideoCueData } from "../../lib/types";
import type { CueProperties } from "../../lib/commands";
import { DEFAULT_LAYER_STYLE } from "../../lib/types";
import { NumberInput, Section, SliderRow, ToggleRow, inputStyle } from "./Field";
import { Select } from "../common/Select";
import { OutputPicker } from "./OutputPicker";

const colorButtonStyle = { ...inputStyle, width: "auto", fontSize: 12 };

export const BLEND_MODES: { value: BlendMode; label: string }[] = [
  { value: "normal", label: "Normal" },
  { value: "add", label: "Add" },
  { value: "multiply", label: "Multiply" },
  { value: "screen", label: "Screen" },
  { value: "overlay", label: "Overlay" },
  { value: "soft_light", label: "Soft Light" },
  { value: "hard_light", label: "Hard Light" },
  { value: "darken", label: "Darken" },
  { value: "lighten", label: "Lighten" },
  { value: "color_dodge", label: "Color Dodge" },
  { value: "color_burn", label: "Color Burn" },
  { value: "difference", label: "Difference" },
  { value: "exclusion", label: "Exclusion" },
  { value: "subtract", label: "Subtract" },
  { value: "linear_burn", label: "Linear Burn" },
  { value: "pin_light", label: "Pin Light" },
  { value: "hue", label: "Hue" },
  { value: "saturation", label: "Saturation" },
  { value: "color", label: "Color" },
  { value: "luminosity", label: "Luminosity" },
  { value: "source_atop", label: "Source Atop" },
];

export function LayerTab({
  cue,
  onSave,
}: {
  cue: VideoCueData | ImageCueData | CameraCueData;
  onSave: (p: Partial<VideoCueData | ImageCueData | CameraCueData>, nativePatch?: CueProperties) => void;
}) {
  const layerStyle: LayerStyle = cue.layer_style ?? DEFAULT_LAYER_STYLE;
  const saturation = layerStyle.saturation ?? 1;
  const patch = (partial: Partial<LayerStyle>) =>
    onSave({ layer_style: { ...layerStyle, ...partial } }, { layer_style_patch: partial });

  return (
    <>
      <Section
        title="Compositing"
        hint="Visual cues stack as layers on the output. Changes apply live."
      >
        <OutputPicker value={cue.output_id ?? null} onChange={(output_id) => onSave({ output_id })} />
        <ToggleRow
          label="Automatic layer order (newest on top)"
          checked={layerStyle.layer === null}
          onToggle={(v) => patch({ layer: v ? null : 500 })}
        />
        {layerStyle.layer !== null && (
          <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10, paddingLeft: 23 }}>
            <span style={{ fontSize: 12, color: "var(--wc-text-secondary)" }}>Layer (0–1000, higher = in front)</span>
            <NumberInput
              value={layerStyle.layer}
              step={1}
              min={0}
              max={1000}
              width={80}
              onCommit={(v) => patch({ layer: Math.round(v) })}
            />
          </div>
        )}
        <SliderRow
          label="Opacity"
          value={layerStyle.opacity}
          min={0}
          max={1}
          step={0.01}
          format={(v) => `${Math.round(v * 100)}%`}
          onChange={(v) => patch({ opacity: v })}
        />
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
          <label style={{ width: 100, color: "var(--wc-text-secondary)", flexShrink: 0, fontSize: 12 }}>
            Blend Mode
          </label>
          <Select
            style={{ ...inputStyle, cursor: "pointer" }}
            value={layerStyle.blend_mode}
            onChange={(e) => patch({ blend_mode: e.target.value as BlendMode })}
          >
            {BLEND_MODES.map((m) => (
              <option key={m.value} value={m.value}>{m.label}</option>
            ))}
          </Select>
        </div>
      </Section>
      <Section title="Color Controls">
        <SliderRow
          label="Picture brightness"
          value={layerStyle.picture_brightness ?? 0}
          min={-1}
          max={1}
          step={0.01}
          format={value => value.toFixed(2)}
          onChange={picture_brightness => patch({ picture_brightness })}
        />
        <SliderRow
          label="Contrast"
          value={layerStyle.contrast ?? 1}
          min={0.25}
          max={4}
          step={0.01}
          format={value => `${value.toFixed(2)}×`}
          onChange={contrast => patch({ contrast })}
        />
        <SliderRow
          label="Hue angle"
          value={layerStyle.hue_degrees ?? 0}
          min={-180}
          max={180}
          step={1}
          format={value => `${Math.round(value)}°`}
          onChange={hue_degrees => patch({ hue_degrees })}
        />
        <SliderRow
          label="Saturation"
          value={saturation}
          min={0}
          max={2}
          step={0.01}
          format={value => `${Math.round(value * 100)}%`}
          onChange={saturation => patch({ saturation })}
        />
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6, margin: "0 0 6px 108px" }}>
          <button
            type="button"
            style={colorButtonStyle}
            onClick={() => patch({ saturation: 0 })}
          >
            Monochrome
          </button>
          <button
            type="button"
            style={colorButtonStyle}
            onClick={() => patch({ saturation: 1 })}
          >
            Reset saturation
          </button>
          <button
            type="button"
            style={colorButtonStyle}
            onClick={() => patch({ saturation: 1, picture_brightness: 0, contrast: 1, hue_degrees: 0 })}
          >
            Reset color controls
          </button>
        </div>
      </Section>
    </>
  );
}
