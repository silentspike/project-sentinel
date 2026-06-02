import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { apiJson, type EbpfMetrics, type PipelineMetrics, type TickMetrics } from "../api";
import { consoleStore } from "../stores/console";
import { formatBucket, formatBytes, formatMs, formatNumber } from "./format";

const BENCHMARK_ROWS = [
  ["Physics", "26.86% schneller", "RoomPhysicsWorkspace ohne per-tick HashMap-Allokation"],
  ["Perception", "26.34% schneller", "generate_perception_into mit wiederverwendeten Puffern"],
  ["Persist e2e", "52.57% schneller", "26 Events in einer SQLite-Transaktion"],
  ["Persist write-only", "86.95% schneller", "prebuilt Event-Pfad isoliert Store-Writes"],
  ["Full tick", "17.23% schneller", "26-Agenten-Tick Regression-Guard"],
] as const;

function MetricCard(props: { label: string; value: string; tone?: "warn" | "danger" | "ok" }): JSX.Element {
  return (
    <div class={`metric-card ${props.tone ? `metric-card--${props.tone}` : ""}`}>
      <div class="metric-card__value">{props.value}</div>
      <div class="metric-card__label">{props.label}</div>
    </div>
  );
}

export function MetricsView(): JSX.Element {
  const [ebpf, setEbpf] = createSignal<EbpfMetrics | null>(null);
  const [pipeline, setPipeline] = createSignal<PipelineMetrics | null>(null);
  const [tick, setTick] = createSignal<TickMetrics | null>(null);

  const loadExtras = async () => {
    const [ebpfResult, pipelineResult, tickResult] = await Promise.allSettled([
      apiJson<EbpfMetrics>("/api/metrics/ebpf"),
      apiJson<PipelineMetrics>("/api/metrics/pipeline"),
      apiJson<TickMetrics>("/api/metrics/tick"),
    ]);
    setEbpf(ebpfResult.status === "fulfilled" ? ebpfResult.value : { available: false, mode: "unavailable", stalled_count: 0, stalled_agents: [], prometheus: "offline" });
    setPipeline(pipelineResult.status === "fulfilled" ? pipelineResult.value : { available: false, gateway: "offline", providers: [] });
    setTick(tickResult.status === "fulfilled" ? tickResult.value : { available: false, tick_duration_ms: 0, tick_rate_effective_ms: 0, psi_cpu_avg10: 0, psi_mem_avg10: 0, psi_io_avg10: 0, prometheus: "offline" });
  };

  onMount(() => {
    void loadExtras();
    const timer = window.setInterval(() => void loadExtras(), 5_000);
    onCleanup(() => window.clearInterval(timer));
  });

  return (
    <section class="col view-panel" data-testid="view-metrics">
      <div class="col__head view-head">
        <span>Metrics</span>
        <span class={`pill ${pipeline()?.available ? "pill-ok" : "pill-warn"}`}>
          {pipeline()?.available ? "Gateway ok" : "Gateway offline"}
        </span>
      </div>
      <div class="col__body view-body">
        <Show when={consoleStore.kpi} fallback={<p class="muted">Warte auf kpi-Push.</p>}>
          {(kpi) => (
            <div class="metrics-grid" data-testid="kpi-grid">
              <MetricCard label="Aktive Agents" value={formatNumber(kpi().active_agents)} />
              <MetricCard label="Agents im Push" value={formatNumber(consoleStore.agents.length)} />
              <MetricCard label="Aktionen" value={formatNumber(kpi().total_actions)} />
              <MetricCard label="Transits" value={formatNumber(kpi().total_transits)} />
              <MetricCard label="Chaos Events" value={formatNumber(kpi().chaos_events)} tone={kpi().chaos_events > 0 ? "warn" : undefined} />
              <MetricCard label="Schichtwechsel" value={formatNumber(kpi().shift_changes)} />
              <MetricCard label="Nightrun Events" value={formatNumber(kpi().nightrun_events)} />
              <MetricCard label="Tick Count" value={formatNumber(kpi().tick_count)} />
              <MetricCard label="Bucket" value={formatBucket(kpi().bucket_start)} />
            </div>
          )}
        </Show>

        <section class="metrics-section">
          <h3>eBPF</h3>
          <div class="metrics-grid metrics-grid--compact">
            <MetricCard label="Mode" value={ebpf()?.available ? ebpf()?.mode ?? "unknown" : "N/A"} tone={ebpf()?.available ? "ok" : "warn"} />
            <MetricCard label="Stalled Agents" value={formatNumber(ebpf()?.stalled_count ?? 0)} tone={(ebpf()?.stalled_count ?? 0) > 0 ? "danger" : undefined} />
            <MetricCard label="Collection Cycle" value={`${formatNumber(ebpf()?.collection_cycle_us ?? 0)} µs`} />
            <MetricCard label="Ring Buffer Drops" value={formatNumber(ebpf()?.ring_buffer_drops ?? 0)} tone={(ebpf()?.ring_buffer_drops ?? 0) > 0 ? "warn" : undefined} />
            <MetricCard label="I/O Read" value={formatBytes(ebpf()?.io_read_bytes)} />
            <MetricCard label="I/O Write" value={formatBytes(ebpf()?.io_write_bytes)} />
            <MetricCard label="Avg PSI Stress" value={`${((ebpf()?.avg_stress ?? 0) * 100).toFixed(1)}%`} />
          </div>
          <Show when={(ebpf()?.stalled_agents ?? []).length > 0}>
            <div class="metric-detail-list">
              <strong>Stalled Agents:</strong>
              <For each={ebpf()?.stalled_agents ?? []}>{(agent) => <span class="pill">{agent.agent}{agent.seconds ? ` (${agent.seconds}s)` : ""}</span>}</For>
            </div>
          </Show>
        </section>

        <section class="metrics-section">
          <h3>Pipeline</h3>
          <Show
            when={pipeline()?.available}
            fallback={<div class="degraded-panel" data-testid="pipeline-offline">Gateway offline</div>}
          >
            <div class="provider-table">
              <div class="provider-table__head">Provider</div>
              <div class="provider-table__head">Latency</div>
              <div class="provider-table__head">OK / Fehler</div>
              <div class="provider-table__head">Tokens</div>
              <For each={pipeline()?.providers ?? []}>
                {(provider) => (
                  <>
                    <div>{provider.provider}</div>
                    <div>{formatMs(provider.latency_avg_s * 1000)} ({formatNumber(provider.latency_count)})</div>
                    <div>{formatNumber(provider.requests_ok)} / {formatNumber(provider.requests_error)}</div>
                    <div>{formatNumber(provider.tokens_input)} / {formatNumber(provider.tokens_output)}</div>
                  </>
                )}
              </For>
            </div>
          </Show>
        </section>

        <section class="metrics-section">
          <h3>Tick / PSI</h3>
          <div class="metrics-grid metrics-grid--compact">
            <MetricCard label="Tick Duration" value={formatMs(tick()?.tick_duration_ms ?? 0)} tone={tick()?.available ? undefined : "warn"} />
            <MetricCard label="Effective Rate" value={formatMs(tick()?.tick_rate_effective_ms ?? 0)} />
            <MetricCard label="PSI CPU" value={`${((tick()?.psi_cpu_avg10 ?? 0) * 100).toFixed(1)}%`} />
            <MetricCard label="PSI Mem" value={`${((tick()?.psi_mem_avg10 ?? 0) * 100).toFixed(1)}%`} />
            <MetricCard label="PSI IO" value={`${((tick()?.psi_io_avg10 ?? 0) * 100).toFixed(1)}%`} />
          </div>
        </section>

        <section class="benchmark-panel">
          <h3>Tick-Loop Benchmarks #276</h3>
          <p class="muted">sentinel-ubuntu-2404 / Intel Core i7-3930K / same-machine before-after</p>
          <div class="benchmark-table">
            <div class="benchmark-table__head">Pfad</div>
            <div class="benchmark-table__head">Delta</div>
            <div class="benchmark-table__head">System</div>
            <For each={BENCHMARK_ROWS}>
              {([label, delta, note]) => (
                <>
                  <div>{label}</div>
                  <div>{delta}</div>
                  <div>{note}</div>
                </>
              )}
            </For>
          </div>
        </section>
      </div>
    </section>
  );
}
