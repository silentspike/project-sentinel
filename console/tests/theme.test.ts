import { describe, it, expect, beforeEach } from "vitest";
import { theme, setTheme, toggleTheme } from "../src/theme";

// Regressionstest gegen den „toten Toggle" (vorher: ThemeToggle flippte nur sein Label,
// ohne das Theme tatsaechlich umzuschalten). Hier: Toggle MUSS data-theme + Persistenz wirken.
describe("theme toggle (#419)", () => {
  beforeEach(() => setTheme("dark"));

  it("toggles dark <-> light and applies data-theme to <html>", () => {
    expect(theme()).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");

    const next = toggleTheme();
    expect(next).toBe("light");
    expect(theme()).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");

    toggleTheme();
    expect(theme()).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("persists the choice to localStorage (survives reload)", () => {
    setTheme("light");
    expect(localStorage.getItem("sentinel-theme")).toBe("light");
    setTheme("dark");
    expect(localStorage.getItem("sentinel-theme")).toBe("dark");
  });
});
