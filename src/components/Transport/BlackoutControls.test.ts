import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { BlackoutStatus, HardStopBlackoutButton } from "./BlackoutControls";

it("exposes the combined stop only when authored", () => {
  const render = (enabled: boolean) => renderToStaticMarkup(createElement(HardStopBlackoutButton, { enabled, run: vi.fn() }));
  expect(render(false)).toBe("");
  expect(render(true)).toContain("Hard stop + blackout");
});

it("presents an independent restore action only for active blackout", () => {
  const status = (blackout: boolean | null) => renderToStaticMarkup(createElement(BlackoutStatus, { blackout, restore: vi.fn() }));
  expect(status(false)).toBe("");
  expect(status(null)).toContain("DMX state unavailable");
  expect(status(null)).not.toContain("Restore lights");
  expect(status(true)).toContain("DMX blackout active");
  expect(status(true)).toContain("Restore lights");
});
