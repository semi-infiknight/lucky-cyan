import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { adjacentFindMatch, CueFindBar, restoreFindFocus } from "../CueFindBar";

describe("cue Find navigation", () => {
  it("visits matches in cue order and wraps in both directions", () => {
    const found = ["a", "b", "c"];
    expect(adjacentFindMatch(found, null, 1)).toBe("a");
    expect(adjacentFindMatch(found, "other", -1)).toBe("c");
    expect(adjacentFindMatch(found, "a", -1)).toBe("c");
    expect(adjacentFindMatch(found, "c", 1)).toBe("a");
    expect(adjacentFindMatch([], "other", 1)).toBeUndefined();
  });

  it("exposes accessible navigation and disables selection while finding", () => {
    const html = renderToStaticMarkup(createElement(CueFindBar, {
      inputRef: { current: null }, workspaceId: "show", listId: "list", revision: 1,
      query: "Understudy", cues: [], selected: null,
      onQueryChange: () => {}, onSelect: () => {}, onClose: () => {},
    }));
    expect(html).toContain('aria-label="Find cues"');
    expect(html).toContain('aria-label="Previous match"');
    expect(html).toContain('aria-label="Next match"');
    expect(html).toContain('disabled="">Select Found</button>');
    expect(html).toContain('role="status"');
    expect(html).toContain("Finding");
  });

  it("returns focus to the cart surface and avoids landing on an armed pad", () => {
    const cart = { focus: vi.fn() };
    const querySelector = vi.fn((selector: string) =>
      selector === "[data-cue-cart-focus]" ? cart : null,
    );
    restoreFindFocus({ querySelector } as unknown as HTMLElement, "pad");
    expect(querySelector).toHaveBeenCalledExactlyOnceWith("[data-cue-cart-focus]");
    expect(cart.focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
  });

  it("returns focus to the selected cue row or an empty cue tree", () => {
    const row = { focus: vi.fn() };
    const tree = { focus: vi.fn() };
    const querySelector = vi.fn((selector: string) => {
      if (selector === '[role="treeitem"][data-cue-id="cue"]') return row;
      return selector === '[role="tree"]' ? tree : null;
    });
    vi.stubGlobal("CSS", { escape: (value: string) => value });
    try {
      const desk = { querySelector } as unknown as HTMLElement;
      restoreFindFocus(desk, "cue");
      restoreFindFocus(desk, null);
      expect(row.focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
      expect(tree.focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
