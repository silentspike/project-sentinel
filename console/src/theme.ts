import { createSignal } from "solid-js";

// Zentrales Theme-Signal (#419) — SOTA-Toggle: persistiert (localStorage) + respektiert die
// OS-Praeferenz (prefers-color-scheme), wendet `data-theme` auf <html> an (tokens.css ueberschreibt
// die CSS-Vars je Theme). Spaetere Consumer (z.B. Floorplan-WebGL) lesen `theme()` reaktiv.

export type Theme = "dark" | "light";
const STORAGE_KEY = "sentinel-theme";

function initialTheme(): Theme {
  if (typeof localStorage !== "undefined") {
    const s = localStorage.getItem(STORAGE_KEY);
    if (s === "light" || s === "dark") return s;
  }
  if (typeof window !== "undefined" && window.matchMedia?.("(prefers-color-scheme: light)").matches) {
    return "light";
  }
  return "dark";
}

function apply(t: Theme) {
  if (typeof document !== "undefined") document.documentElement.dataset.theme = t;
  if (typeof localStorage !== "undefined") localStorage.setItem(STORAGE_KEY, t);
}

const [theme, setThemeSignal] = createSignal<Theme>(initialTheme());
apply(theme()); // initialen Zustand sofort auf <html> spiegeln

export { theme };

export function setTheme(t: Theme) {
  setThemeSignal(t);
  apply(t);
}

export function toggleTheme(): Theme {
  const next: Theme = theme() === "dark" ? "light" : "dark";
  setTheme(next);
  return next;
}
