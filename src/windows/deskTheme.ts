const THEME_KEY = "wc_theme";

/** Follow the main desk's applied theme for the lifetime of a detached window. */
export function followDeskTheme(): () => void {
  const systemTheme = window.matchMedia("(prefers-color-scheme: dark)");
  const apply = () => {
    let savedTheme: string | null = null;
    try {
      savedTheme = window.localStorage.getItem(THEME_KEY);
    } catch {
      // A WebView can have storage disabled. Use its system appearance.
    }
    const theme = savedTheme === "dark" || savedTheme === "light" || savedTheme === "navy"
      ? savedTheme : systemTheme.matches ? "dark" : "light";
    document.documentElement.setAttribute("data-theme", theme);
  };
  const onStorage = (event: StorageEvent) => {
    if (event.key === THEME_KEY || event.key === null) apply();
  };

  window.addEventListener("storage", onStorage);
  window.addEventListener("focus", apply);
  systemTheme.addEventListener("change", apply);
  apply();

  return () => {
    window.removeEventListener("storage", onStorage);
    window.removeEventListener("focus", apply);
    systemTheme.removeEventListener("change", apply);
  };
}
