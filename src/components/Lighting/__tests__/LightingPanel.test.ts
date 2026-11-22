import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { LightingPanel } from "../LightingPanel";

describe("lighting panel modes", () => {
  it("keeps blackout and the monitor available during a show", () => {
    const html = renderToStaticMarkup(createElement(LightingPanel, { blackout: false, onBlackoutChange() {}, onClose() {}, showMode: true }));
    expect(html).toContain("Blackout");
    expect(html).toContain("Live output");
    for (const label of ["Test channel", "Fixtures (patch)", "Dashboard (live)", "Universe outputs"]) {
      expect(html).not.toContain(label);
    }
  });

  it("exposes patch and live programming tools to the programmer", () => {
    const html = renderToStaticMarkup(createElement(LightingPanel, { blackout: false, onBlackoutChange() {}, onClose() {} }));
    expect(html).toContain("Fixtures (patch)");
    expect(html).toContain("Live controls");
    expect(html).toContain("Output monitor");
    expect(html).not.toContain("Test channel");
  });
});
