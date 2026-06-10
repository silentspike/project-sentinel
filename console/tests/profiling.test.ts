import { describe, it, expect } from "vitest";
import { barWidthPct } from "../src/views/profilingMath";
import type { PhaseRow } from "../src/api";

function row(phase: string, p50: number, p95: number): PhaseRow {
  return { phase, p50_ms: p50, p95_ms: p95, count: 10, sum_ms: p50 * 10, avg_ms: p50 };
}

describe("barWidthPct (#381 profiling bars)", () => {
  it("scales relative to the largest p95", () => {
    const phases = [row("input", 0.05, 0.25), row("persist", 5, 25)];
    expect(barWidthPct(25, phases)).toBe(100);
    expect(barWidthPct(12.5, phases)).toBe(50);
    expect(barWidthPct(0.25, phases)).toBe(1);
  });

  it("returns 0 for empty phase list instead of dividing by zero", () => {
    expect(barWidthPct(0, [])).toBe(0);
    expect(barWidthPct(5, [])).toBeLessThanOrEqual(100);
    expect(Number.isFinite(barWidthPct(5, []))).toBe(true);
  });

  it("clamps to 100 when value exceeds max p95 (e.g. p99 outlier)", () => {
    const phases = [row("mood", 0.01, 0.05)];
    expect(barWidthPct(0.5, phases)).toBe(100);
  });

  it("never returns negative widths", () => {
    const phases = [row("chaos", 1, 2)];
    expect(barWidthPct(-1, phases)).toBe(0);
  });

  it("all-zero histograms (warmup) stay at 0 width", () => {
    const phases = [row("input", 0, 0), row("biology", 0, 0)];
    expect(barWidthPct(0, phases)).toBe(0);
  });
});
