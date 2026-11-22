import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { PatchedFixture } from "../../../lib/types";
import { cmyToHex, fixtureColorBinding, groupColorBindings, groupColorLevels, hexToCmy, hsiToHex, hsvToHex, rgbChannelChanges } from "../../../lib/fixtureColor";
import { FixtureProfile } from "../FixtureProfile";
import { FixtureColorControl } from "../FixtureColorControl";

describe("fixture profiles", () => {
  it("fans picker colors out to repeated RGB roles and leaves other channels alone", () => {
    const params = ["white", "green", "red", "uv", "blue", "red"].map(kind => ({ kind }));
    expect(rgbChannelChanges(params, "#ff0080")).toEqual([
      { paramIndex: 1, value: 0 }, { paramIndex: 2, value: 1 },
      { paramIndex: 4, value: 128 / 255 }, { paramIndex: 5, value: 1 },
    ]);
  });

  it("shows editable identification and control roles next to the physical addresses", () => {
    const fixture: PatchedFixture = { id: "wash", label: "Stage right", universe: 1, base_address: 20,
      fixture_type: { manufacturer: "Example", name: "LED batten", mode: "Pixels", parameters: [
        { name: "Pixel A", kind: "red", width: "Bit8", channel_offset: 1, default: 0 },
      ] } };
    const html = renderToStaticMarkup(createElement(FixtureProfile, { fixture, save: async () => {}, close() {} }));
    for (const expected of ["Example", "LED batten", "Pixels", "Control role for Pixel A", "<td>21</td>", "HSV value", "Save profile", "Cancel"]) {
      expect(html).toContain(expected);
    }
  });

  it("shows the adjacent fine address and exposes export of the saved layout", () => {
    const fixture: PatchedFixture = {
      id: "head", label: "Back light", universe: 7, base_address: 48,
      fixture_type: { name: "Moving head", parameters: [
        { name: "Pan", kind: "pan", width: "Bit16", channel_offset: 2, default: .5 },
      ] },
    };
    const html = renderToStaticMarkup(createElement(FixtureProfile, {
      fixture, save: async () => {}, exportSaved: async () => {}, saveToLibrary: async () => {}, close() {},
    }));
    expect(html).toContain("<td>50–51</td>");
    expect(html).toContain("Export saved profile…");
    expect(html).toContain("Add saved profile to library");
    fixture.fixture_type.parameters[0].fine_channel_offset = 7;
    const separated = renderToStaticMarkup(createElement(FixtureProfile, {
      fixture, save: async () => {}, close() {},
    }));
    expect(separated).toContain("<td>50 / 55</td>");
    expect(separated).toContain('aria-label="Fine slot for Pan"');
  });

  it("resolves one explicit CMY triplet without including the dimmer or guessing channel names", () => {
    const fixture: PatchedFixture = { id: "cmy", label: "Filter wash", universe: 2, base_address: 115,
      fixture_type: { name: "Example", parameters: [
        { name: "Dimmer", kind: "intensity", width: "Bit8", channel_offset: 0, default: 0 },
        { name: "Filter Z", kind: "yellow", width: "Bit8", channel_offset: 4, default: 0 },
        { name: "Filter X", kind: "cyan", width: "Bit16", channel_offset: 1, default: 0 },
        { name: "Filter Y", kind: "magenta", width: "Bit8", channel_offset: 3, default: 0 },
      ] } };
    expect(fixtureColorBinding(fixture, "cmy")).toEqual({ indices: [2, 3, 1], channels: [
      { universe: 2, channel: 115, width: "Bit16" },
      { universe: 2, channel: 117, width: "Bit8" },
      { universe: 2, channel: 118, width: "Bit8" },
    ] });
    expect(fixtureColorBinding({ ...fixture, base_address: 511 }, "cmy")).toBeNull();
    fixture.fixture_type.parameters[3].channel_offset = 2;
    expect(fixtureColorBinding(fixture, "cmy")).toBeNull();
    fixture.fixture_type.parameters[3].channel_offset = 3;
    fixture.fixture_type.parameters[0].kind = "cyan";
    expect(fixtureColorBinding(fixture, "cmy")).toBeNull();
  });

  it("shows absolute component levels and requires a separate Apply action", () => {
    expect(hexToCmy("#00ffff")).toEqual([1, 0, 0]);
    expect(cmyToHex([0, 1, 1])).toBe("#ff0000");
    const html = renderToStaticMarkup(createElement(FixtureColorControl, { model: "cmy", levels: [0.15, 0.5, 1], onApply() {} }));
    for (const expected of ["Cyan %", "Magenta %", "Yellow %", 'value="15"', 'value="50"', 'value="100"', "Apply CMY to cue"]) {
      expect(html).toContain(expected);
    }
    expect(html).not.toContain("Apply to live output");
  });

  it("requires an explicit HSI intensity role and does not include the master dimmer", () => {
    const fixture: PatchedFixture = { id: "hsi", label: "Color wash", universe: 5, base_address: 401,
      fixture_type: { name: "Example", parameters: [
        { name: "Master", kind: "intensity", width: "Bit8", channel_offset: 3, default: 0 },
        { name: "Color B", kind: "saturation", width: "Bit8", channel_offset: 1, default: 0 },
        { name: "Color A", kind: "hue", width: "Bit8", channel_offset: 0, default: 0 },
        { name: "Color C", kind: "generic", width: "Bit8", channel_offset: 2, default: 0 },
      ] } };
    expect(fixtureColorBinding(fixture, "hsi")).toBeNull();
    fixture.fixture_type.parameters[3].kind = "hsi_intensity";
    expect(fixtureColorBinding(fixture, "hsi")).toEqual({
      indices: [2, 1, 3],
      channels: [400, 401, 402].map(channel => ({ universe: 5, channel, width: "Bit8" })),
    });
    expect(fixtureColorBinding(fixture, "cmy")).toBeNull();
    fixture.fixture_type.parameters[0].kind = "hsi_intensity";
    expect(fixtureColorBinding(fixture, "hsi")).toBeNull();
  });

  it("uses degrees and percentages for HSI with a read-only, approximate swatch", () => {
    expect(hsiToHex([1, 1, 1 / 3])).toBe("#ff0000");
    expect(hsiToHex([1 / 3, 1, 1 / 3])).toBe("#00ff00");
    expect(hsiToHex([2 / 3, 1, 1 / 3])).toBe("#0000ff");
    expect(hsiToHex([0.2, 0, 0.5])).toBe("#808080");
    const html = renderToStaticMarkup(createElement(FixtureColorControl, { model: "hsi", levels: [1 / 3, 0.6, 0.4], onApply() {} }));
    for (const expected of ["Native color (HSI)", "Hue °", "Saturation %", "HSI intensity %", 'max="360"',
      'value="120"', 'value="60"', 'value="40"', "Approximate HSI preview", "Apply HSI to cue"]) {
      expect(html).toContain(expected);
    }
    expect(html).not.toContain('type="color"');
  });

  it("treats HSV value as its own physical role and uses an HSV swatch", () => {
    const hsv: PatchedFixture = { id: "a", label: "Color wash", universe: 7, base_address: 101,
      fixture_type: { name: "Example", parameters: [
        { name: "Color A", kind: "hue", width: "Bit8", channel_offset: 0, default: 0 },
        { name: "Color B", kind: "saturation", width: "Bit8", channel_offset: 1, default: 0 },
        { name: "Color C", kind: "hsv_value", width: "Bit16", channel_offset: 2, default: 0 },
        { name: "Master", kind: "intensity", width: "Bit8", channel_offset: 4, default: 0 },
      ] } };
    expect(fixtureColorBinding(hsv, "hsv")?.indices).toEqual([0, 1, 2]);
    expect(fixtureColorBinding(hsv, "hsi")).toBeNull();
    const hsi = { ...structuredClone(hsv), id: "b", base_address: 201 };
    hsi.fixture_type.parameters[2].kind = "hsi_intensity";
    const group = { id: "pair", label: "Color wash", fixture_ids: ["a", "b"] };
    expect(groupColorBindings(group, [hsv, hsi])).toEqual([]);
    hsi.fixture_type.parameters[2].kind = "hsv_value";
    expect(groupColorBindings(group, [hsv, hsi]).map(mix => mix.model)).toEqual(["hsv"]);
    hsv.fixture_type.parameters[3].kind = "hsi_intensity";
    expect(fixtureColorBinding(hsv, "hsv")).toBeNull();
    expect(fixtureColorBinding(hsv, "hsi")).toBeNull();

    expect(hsvToHex([1, 1, 0.5])).toBe("#800000");
    expect(hsvToHex([1 / 3, 1, 0.5])).toBe("#008000");
    expect(hsvToHex([0.2, 0, 0.5])).toBe("#808080");
    expect(hsvToHex([2 / 3, 0.5, 0.75])).toBe("#6060bf");
    const html = renderToStaticMarkup(createElement(FixtureColorControl, { model: "hsv", levels: [2 / 3, 0.5, 0.75], onApply() {} }));
    for (const expected of ["Native color (HSV)", "Hue °", "Saturation %", "HSV value %", 'value="240"', 'value="50"',
      'value="75"', "Approximate HSV preview", "Apply HSV to cue"]) {
      expect(html).toContain(expected);
    }
  });

  it("requires a common color model on every group member and protects other physical slots", () => {
    const first: PatchedFixture = { id: "a", label: "Filter left", universe: 1, base_address: 11,
      fixture_type: { name: "Example", parameters: [
        { name: "Cyan", kind: "cyan", width: "Bit16", channel_offset: 0, default: 0 },
        { name: "Magenta", kind: "magenta", width: "Bit8", channel_offset: 2, default: 0 },
        { name: "Yellow", kind: "yellow", width: "Bit8", channel_offset: 3, default: 0 },
        { name: "Dimmer", kind: "intensity", width: "Bit8", channel_offset: 4, default: 0 },
      ] } };
    const second = { ...structuredClone(first), id: "b", universe: 2 };
    const group = { id: "group", label: "Filters", fixture_ids: ["a", "b"] };
    const [mix] = groupColorBindings(group, [first, second]);
    expect(mix.model).toBe("cmy");
    expect(mix.mapping.map(member => member.channels.map(channel => channel.channel))).toEqual([[10, 12, 13], [10, 12, 13]]);
    expect(groupColorLevels(mix.mapping, { a: [0.2, 0.4, 0.6, 1], b: [0.2, 0.8, 0.6, 0] })).toEqual([0.2, null, 0.6]);
    expect(groupColorBindings(group, [first])).toEqual([]);
    expect(groupColorBindings({ ...group, fixture_ids: ["a", "a"] }, [first, second])).toEqual([]);
    expect(groupColorBindings(group, [first, { ...second, universe: 1 }])).toEqual([]);

    const peer: PatchedFixture = { id: "peer", label: "Other dimmer", universe: 1, base_address: 12,
      fixture_type: { name: "Example", parameters: [{ ...first.fixture_type.parameters[3], channel_offset: 0 }] } };
    expect(groupColorBindings(group, [first, second, peer])).toEqual([]);
    second.fixture_type.parameters[0].kind = "hue";
    expect(groupColorBindings(group, [first, second])).toEqual([]);
  });

  it("shows mixed group levels without supplying a default or enabling Apply", () => {
    const html = renderToStaticMarkup(createElement(FixtureColorControl, {
      model: "cmy", live: true, levels: [0.2, null, 0.6], onApply() {},
    }));
    expect(html).toContain('placeholder="Mixed" value=""');
    expect(html).toContain('value="20"');
    expect(html).toContain('value="60"');
    expect(html).toContain('<button type="button" disabled="">Apply to live output</button>');
    expect(html).not.toContain('type="color"');
  });
});
