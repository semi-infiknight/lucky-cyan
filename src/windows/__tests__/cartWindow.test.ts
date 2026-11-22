import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { CartPlaybackGrid } from "../../components/CueList/CartView";
import type { CueSummary } from "../../lib/types";

const base = { cue_type: "wait", color: "none", state: "standby", duration_ms: 1000 };
const cues = [
  { ...base, id: "alpha", number: "7", name: "Rain" },
  { ...base, id: "beta", number: "8", name: "Wind", is_disabled: true },
  { ...base, id: "hidden", number: "9", name: "Out of view" },
] as CueSummary[];

describe("detached performance grid", () => {
  it("renders the saved holes, cue numbers and disarmed state without firing", () => {
    const action = vi.fn();
    const html = renderToStaticMarkup(createElement(CartPlaybackGrid, { cues,
      layout: { rows: 1, columns: 3, pads: {
        alpha: { row: 1, column: 3 }, beta: { row: 1, column: 1 }, hidden: null,
      } }, onFire: action, onStop: action }));
    expect(html).toContain('aria-label="Row 1, column 2, empty"');
    expect(html).toContain('aria-label="Fire cue 8 Wind, Disarmed"');
    expect(html).toContain("disabled");
    expect(html.indexOf("8 Wind")).toBeLessThan(html.indexOf("7 Rain"));
    expect(html).not.toContain("Out of view");
    expect(html).not.toContain("Edit pads");
    expect(action).not.toHaveBeenCalled();
  });
  it("preserves cue order in a compact cart", () => {
    const html = renderToStaticMarkup(createElement(CartPlaybackGrid, { cues, onFire: vi.fn(), onStop: vi.fn() }));
    expect(html.indexOf("7 Rain")).toBeLessThan(html.indexOf("8 Wind"));
    expect(html).toContain("9 Out of view");
  });
});
