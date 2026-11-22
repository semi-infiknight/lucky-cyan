import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { LevelMatrixGrid } from "../LevelMatrixGrid";

it("shows a bounded matrix viewport and labels the routed hardware channels", () => {
  const html = renderToStaticMarkup(createElement(LevelMatrixGrid, {
    cueId: "rehearsal", matrix: Array.from({ length: 128 }, () => Array(128).fill(-9)),
    patchName: "Backstage", deviceChannels: [6, 7], channelNames: ["", "Backstage"], onSave: () => undefined,
  }));
  expect(html.match(/class="route-cell"/g)).toHaveLength(32);
  expect(html).toContain("121–128");
  expect(html).toContain("125–128");
  expect(html).toContain("Output 2 (Backstage) → device channel 8");
  expect(html).toContain("Output 3 → unassigned");
  expect(html).toContain("Input 8 to output 4, gain in decibels");
});

it("labels unpatched columns with their same-numbered device channels", () => {
  const html = renderToStaticMarkup(createElement(LevelMatrixGrid, {
    cueId: "rehearsal", matrix: [[0]], patchName: "Default output device",
    deviceChannels: [], onSave: () => undefined,
  }));
  expect(html).toContain("Output 3 → device channel 3");
});

it("shows source names on input rows and crosspoints independently of patch labels", () => {
  const html = renderToStaticMarkup(createElement(LevelMatrixGrid, {
    cueId: "dialogue", matrix: [[0]], patchName: "PA", deviceChannels: [6],
    sourceNames: ["", "", "Dialogue"], channelNames: ["Front fill"],
    onSaveSourceNames: () => undefined, onSave: () => undefined,
  }));
  expect(html).toContain('scope="row">3<small>Dialogue</small>');
  expect(html).toContain("Input 3 (Dialogue) to output 1, gain in decibels");
  expect(html).toContain("Output 1 (Front fill) → device channel 7");
  expect(html).toContain('aria-label="Name for source channel 3"');
});

it("allows naming source channels in pan mode without creating a matrix", () => {
  const html = renderToStaticMarkup(createElement(LevelMatrixGrid, {
    cueId: "video", matrix: null, patchName: "Default", deviceChannels: [],
    onSaveSourceNames: () => undefined, onSave: () => undefined,
  }));
  expect(html).toContain("Source channel names");
  expect(html).toContain("121–128");
  expect(html).not.toContain('class="route-cell"');
});
