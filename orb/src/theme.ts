export type ThemePref = "auto" | "light" | "dark";

const KEY = "orb-theme";

export function getThemePref(): ThemePref {
  const v = localStorage.getItem(KEY);
  return v === "light" || v === "dark" ? v : "auto";
}

function resolved(pref: ThemePref): "light" | "dark" {
  if (pref !== "auto") return pref;
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

function apply(pref: ThemePref) {
  document.documentElement.dataset.theme = resolved(pref);
  // `withGlobalTauri` is enabled in tauri.conf.json. Keep this optional so
  // the same bundle remains a faithful web preview instead of requiring a
  // desktop-only import at startup.
  const invoke = (window as Window & { __TAURI__?: { core?: { invoke?: (cmd: string, args: unknown) => Promise<unknown> } } })
    .__TAURI__?.core?.invoke;
  void invoke?.("set_window_theme", { theme: pref }).catch(() => {
    // The browser preview and older desktop shells have no native bridge.
  });
}

export function setThemePref(pref: ThemePref) {
  localStorage.setItem(KEY, pref);
  apply(pref);
}

export function initTheme() {
  apply(getThemePref());
  window.matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => {
    if (getThemePref() === "auto") apply("auto");
  });
}
