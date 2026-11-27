import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { OverrideControls } from "./OverrideControls";
import { OVERRIDE_LABELS } from "../../hooks/useGlobalOverrides";

const render = (overrides: Record<string, boolean> | null, open: boolean) =>
  renderToStaticMarkup(createElement(OverrideControls, {
    overrides, open, setOpen: vi.fn(), toggle: vi.fn(), pending: "", error: "",
  }));

it("hides the panel until opened and lists every override", () => {
  const closed = render(null, false);
  expect(closed).toContain("Overrides");
  expect(closed).not.toContain("DMX output");
  const open = render(Object.fromEntries(OVERRIDE_LABELS.map(([key]) => [key, true])), true);
  for (const [, label] of OVERRIDE_LABELS) expect(open).toContain(label);
  expect(open).not.toContain(">off<");
});

it("marks disabled paths", () => {
  const overrides = Object.fromEntries(OVERRIDE_LABELS.map(([key]) => [key, true]));
  overrides.dmx_output = false;
  const markup = render(overrides, true);
  expect(markup).toContain('data-off="true"');
  expect(markup).toContain(">off<");
});
