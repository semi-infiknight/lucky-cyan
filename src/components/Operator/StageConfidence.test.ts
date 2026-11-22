import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { captureStageSnapshot } from "../../lib/commands";
import type { StageSnapshot } from "../../lib/types";
import { requestStageStill, StageStillComparison } from "./StageConfidence";
import { decodeStagePreview } from "./stagePreviewFrame";

vi.mock("../../lib/commands", () => ({ captureStageSnapshot: vi.fn() }));

const baseline: StageSnapshot = {
  source_width: 1920,
  source_height: 1080,
  png_base64: "baseline",
};
const latest: StageSnapshot = {
  source_width: 1280,
  source_height: 720,
  png_base64: "latest",
};

describe("Stage snapshot comparison", () => {
  it("decodes only complete raw preview frames on both Tauri IPC routes", () => {
    const packet = new ArrayBuffer(24);
    const header = new DataView(packet);
    [7, 1, 1, 1920, 1080].forEach((value, index) => header.setUint32(index * 4, value, true));
    new Uint8Array(packet, 20).set([255, 0, 0, 255]);
    for (const response of [packet, Array.from(new Uint8Array(packet))]) {
      const frame = decodeStagePreview(response);
      expect(frame).toMatchObject({ serial: 7, width: 1, height: 1, sourceWidth: 1920, sourceHeight: 1080 });
      expect(Array.from(frame!.pixels)).toEqual([255, 0, 0, 255]);
    }
    expect(decodeStagePreview(new ArrayBuffer(0))).toBeNull();
    expect(() => decodeStagePreview(packet.slice(0, 23))).toThrow("Invalid stage preview frame.");
  });

  it("labels the two captured outputs independently and supports a single still", () => {
    const both = renderToStaticMarkup(createElement(StageStillComparison, { latest, reference: baseline }));
    expect(both).toContain('data-comparing="true"');
    expect(both).toContain('src="data:image/png;base64,baseline" alt="Reference frame"');
    expect(both).toContain('src="data:image/png;base64,latest" alt="Latest capture"');
    expect(both).toContain("1920 × 1080");
    expect(both).toContain("1280 × 720");

    const one = renderToStaticMarkup(createElement(StageStillComparison, { latest, reference: null }));
    expect(one).toContain('data-comparing="false"');
    expect(one).not.toContain("Reference frame");
  });

  it("blocks overlapping requests across owner changes and releases the gate after failure", async () => {
    const native = vi.mocked(captureStageSnapshot);
    native.mockClear();
    let finish: (frame: StageSnapshot) => void = () => {};
    native.mockReturnValueOnce(new Promise<StageSnapshot>(resolve => { finish = resolve; }));

    const first = requestStageStill("old-show", "first-output");
    expect(requestStageStill("old-show", "second-output")).toBeNull();
    expect(requestStageStill("new-show", "first-output")).toBeNull();
    finish(baseline);
    await expect(first).resolves.toEqual(baseline);
    expect(native).toHaveBeenCalledExactlyOnceWith("old-show", "first-output");

    native.mockRejectedValueOnce(new Error("The open workspace changed"));
    await expect(requestStageStill("old-show", "first-output")).rejects.toThrow("workspace changed");

    native.mockResolvedValueOnce(latest);
    await expect(requestStageStill("new-show", "first-output")).resolves.toEqual(latest);
    expect(native).toHaveBeenLastCalledWith("new-show", "first-output");
  });
});
