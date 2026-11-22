import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { CLEAR_OUTPUT_MASK } from "../../lib/types";
import { maskPreview, OutputMaskEditor } from "./OutputMaskEditor";

describe("per-output mask controls", () => {
  it("offers all four trims and feathers and disables clear on a legacy output", () => {
    const markup = renderToStaticMarkup(createElement(OutputMaskEditor, {mask: CLEAR_OUTPUT_MASK, onChange: () => {}}));
    for (const side of ["Left", "Right", "Top", "Bottom"]) {
      for (const name of ["trim", "feather"]) expect(markup).toContain(`aria-label="${side} ${name} percent"`);
    }
    expect(markup).toContain('disabled=""');
    expect(markup).toContain("Load PNG…");
    expect(markup).not.toContain("Remove image mask");
  });

  it("places each feather after its own opaque trim in the unwarped preview", () => {
    const gradient = maskPreview({trim: [0.1, 0, 0.15, 0], feather: [0.05, 0.2, 0, 0.4]});
    expect(gradient).toContain("to right, black 10%, transparent 15%");
    expect(gradient).toContain("to left, black 0%, transparent 20%");
    expect(gradient).toContain("to bottom, black 15%, transparent 15%");
    expect(gradient).toContain("to top, black 0%, transparent 40%");
  });
});
