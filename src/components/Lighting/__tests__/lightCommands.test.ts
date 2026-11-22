import { describe, expect, it } from "vitest";
import type { FixtureGroup, PatchedFixture } from "../../../lib/types";
import { previewLightCommands } from "../lightCommands";

function dimmer(label: string): PatchedFixture {
  return { id: `id-${label}`, label, universe: 1, base_address: 1,
    fixture_type: { name: "Dimmer", parameters: [{ kind: "intensity", name: "Intensity", channel_offset: 0, width: "Bit8", default: .25 }] } };
}
const patch = [dimmer("1"), dimmer("2"), dimmer("3"), dimmer("Front.Wash")];
const groups: FixtureGroup[] = [{ label: "Front", id: "front", fixture_ids: ["id-1", "id-2", "id-3"] }];
const preview = (text: string) => previewLightCommands(text, patch, groups, "intensity", "percent");

describe("light command preview", () => {
  it("expands a group, applies home, pass and later overrides in command order", () => {
    const result = preview("front=75\r\n2=pass\r\n3=home\n1.intensity = 12.5");
    expect(result.errors).toEqual([]);
    expect(result.targets.map(t => [t.fixture_id, t.value])).toEqual([["id-1", .125], ["id-3", .25]]);
  });

  it("expands numeric ranges and lists, and resolves a label containing a period", () => {
    const result = preview("[1 - 2, 3].intensity = 99%\nFront.Wash = 50");
    expect(result.targets.map(t => t.value)).toEqual([.99, .99, .99, .5]);
  });

  it("review is atomic when a referenced device or a cue pull cannot be mapped", () => {
    const result = preview("1 = 50\n3 = cue A * .5\n2 = 55\n99 = 10");
    expect(result.errors).toHaveLength(2);
    expect(result.targets).toEqual([]);
  });

  it("validates level units, fixture identity and bounds", () => {
    expect(preview("1=101").errors).toHaveLength(1);
    expect(previewLightCommands("1=128", patch, groups, "intensity", "dmx8").targets[0].value).toBeCloseTo(128 / 255);
    expect(previewLightCommands("1=65535", patch, groups, "intensity", "dmx16").targets[0].value).toBe(1);
    expect(previewLightCommands("1=12.5", patch, groups, "intensity", "dmx8").errors).toHaveLength(1);
    expect(previewLightCommands("1=50", [...patch, dimmer("1")], [], "intensity", "percent").errors).toHaveLength(1);
    expect(preview("1 - 40000000 = 50").errors).toHaveLength(1);
    expect(preview("[1-4096, 1-4096] = 50").errors[0]).toContain("up to 4096");
    expect(preview("1.iris = 50").targets).toEqual([]);
  });
});
