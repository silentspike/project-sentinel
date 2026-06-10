import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { apiJson, type PhaseMetrics } from "../api";
import { formatMs, formatNumber } from "./format";
import { barWidthPct } from "./profilingMath";

// Profiling-View (#381): p50/p95-Dauer pro SimulationPhase als Live-Balken.
// Datenquelle: /api/metrics/phases (dashboard-backend Read-Proxy auf den
// :9090-Prometheus-Text des Daemons). Lifetime-Quantile (kumulativ, kein
// Rolling-Window) — nur der ECS-Anteil des Ticks, Daemon-Post-Tick fehlt.

export function ProfilingView(): JSX.Element {
  const [data, setData] = createSignal<PhaseMetrics | null>(null);

  const load = async () => {
    try {
      setData(await apiJson<PhaseMetrics>("/api/metrics/phases"));
    } catch {
      setData({ available: false, phases: [], prometheus: "offline" });
    }
  };

  onMount(() => {
    void load();
    const timer = window.setInterval(() => void load(), 5_000);
    onCleanup(() => window.clearInterval(timer));
  });

  return (
    <section class="col view-panel" data-testid="view-profiling">
      <div class="col__head view-head">
        <span>Phase Profiling</span>
        <span class={`pill ${data()?.available ? "pill-ok" : "pill-warn"}`}>
          {data()?.available ? "Prometheus ok" : "Prometheus offline"}
        </span>
      </div>
      <div class="col__body view-body">
        <Show
          when={data()?.available && (data()?.phases.length ?? 0) > 0}
          fallback={
            <div class="degraded-panel" data-testid="profiling-offline">
              Keine Phase-Timings verfuegbar (Daemon-Warmup oder phase_timing_enabled=false).
            </div>
          }
        >
          <p class="muted" style={{ margin: 0 }}>
            ECS-Anteil des Ticks, Lifetime-Quantile seit Daemon-Start. p50/p95 sind auf die
            Histogramm-Buckets quantisiert.
          </p>
          <div class="phase-table" data-testid="phase-table">
            <For each={data()?.phases ?? []}>
              {(row) => (
                <div class="phase-row" data-testid={`phase-row-${row.phase}`}>
                  <div class="phase-row__label">{row.phase}</div>
                  <div class="phase-row__bars">
                    <div
                      class="phase-bar phase-bar--p50"
                      style={{ width: `${barWidthPct(row.p50_ms, data()?.phases ?? [])}%` }}
                      title={`p50 ${formatMs(row.p50_ms)}`}
                    />
                    <div
                      class="phase-bar phase-bar--p95"
                      style={{ width: `${barWidthPct(row.p95_ms, data()?.phases ?? [])}%` }}
                      title={`p95 ${formatMs(row.p95_ms)}`}
                    />
                  </div>
                  <div class="phase-row__values">
                    <span>p50 {formatMs(row.p50_ms)}</span>
                    <span>p95 {formatMs(row.p95_ms)}</span>
                    <span class="muted">avg {formatMs(row.avg_ms)}</span>
                    <span class="muted">n={formatNumber(row.count)}</span>
                  </div>
                </div>
              )}
            </For>
          </div>
          <div class="phase-legend muted">
            <span class="phase-legend__swatch phase-legend__swatch--p50" /> p50
            <span class="phase-legend__swatch phase-legend__swatch--p95" /> p95
          </div>
        </Show>
      </div>
    </section>
  );
}
