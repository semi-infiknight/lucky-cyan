import { afterEach, describe, expect, it, vi } from "vitest";
import { followDeskTheme } from "../deskTheme";

function themeWindow(initialTheme: string | null) {
  let savedTheme = initialTheme;
  const getItem = vi.fn(() => savedTheme);
  const setAttribute = vi.fn();
  const system = Object.assign(new EventTarget(), { matches: true });
  const browser = Object.assign(new EventTarget(), {
    localStorage: { getItem },
    matchMedia: () => system,
  });
  vi.stubGlobal("window", browser);
  vi.stubGlobal("document", { documentElement: { setAttribute } });
  return {
    getItem, setAttribute, browser, system,
    saveTheme: (value: string | null) => { savedTheme = value; },
    storageChanged: (key: string | null) => {
      browser.dispatchEvent(Object.assign(new Event("storage"), { key }));
    },
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("detached desk appearance", () => {
  it("updates an open window when the applied desk theme changes", () => {
    const window = themeWindow("dark");
    const stop = followDeskTheme();
    expect(window.setAttribute).toHaveBeenLastCalledWith("data-theme", "dark");
    window.saveTheme("light");
    window.storageChanged("wc_theme");
    expect(window.setAttribute).toHaveBeenLastCalledWith("data-theme", "light");
    window.saveTheme("navy");
    window.storageChanged("wc_theme");
    expect(window.setAttribute).toHaveBeenLastCalledWith("data-theme", "navy");
    stop();
  });

  it("rereads the theme on focus and ignores unrelated storage writes", () => {
    const window = themeWindow("navy");
    const stop = followDeskTheme();
    window.saveTheme("light");
    window.storageChanged("inkue_recent_files");
    expect(window.setAttribute).toHaveBeenCalledTimes(1);
    window.browser.dispatchEvent(new Event("focus"));
    expect(window.setAttribute).toHaveBeenLastCalledWith("data-theme", "light");
    stop();
  });

  it("follows system appearance when the stored theme is unavailable", () => {
    const window = themeWindow("navy");
    const stop = followDeskTheme();
    window.system.matches = false;
    window.system.dispatchEvent(new Event("change"));
    expect(window.setAttribute).toHaveBeenLastCalledWith("data-theme", "navy");
    window.saveTheme(null);
    window.storageChanged(null);
    expect(window.setAttribute).toHaveBeenLastCalledWith("data-theme", "light");
    window.getItem.mockImplementation(() => { throw new Error("Storage unavailable"); });
    window.system.matches = true;
    window.system.dispatchEvent(new Event("change"));
    expect(window.setAttribute).toHaveBeenLastCalledWith("data-theme", "dark");
    stop();
  });

  it("stops updating after the window unmounts", () => {
    const window = themeWindow("dark");
    const stop = followDeskTheme();
    stop();
    window.saveTheme("light");
    window.storageChanged("wc_theme");
    window.browser.dispatchEvent(new Event("focus"));
    window.system.dispatchEvent(new Event("change"));
    expect(window.setAttribute).toHaveBeenCalledTimes(1);
  });
});
