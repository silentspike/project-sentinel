import type { PhaseRow } from "../api";

// Reine Skalierungslogik der Profiling-Balken (#381) — JSX-frei, damit
// Unit-Tests sie ohne solid-js Client-Runtime importieren koennen.

/** Balkenbreite in % relativ zum groessten p95 (min 0.001 gegen 0-Division). */
export function barWidthPct(valueMs: number, phases: PhaseRow[]): number {
  const max = Math.max(0.001, ...phases.map((p) => p.p95_ms));
  const pct = (valueMs / max) * 100;
  if (!Number.isFinite(pct) || pct < 0) return 0;
  return Math.min(100, pct);
}
