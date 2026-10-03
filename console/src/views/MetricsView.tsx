import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { apiJson, type EbpfMetrics, type PipelineMetrics, type TickMetrics } from "../api";
import { activeAgentCount, consoleStore } from "../stores/console";
import { formatBucket, formatBytes, formatMs, formatNumber } from "./format";

const METRICS_REFRESH_MS = 5_000;
const METRICS_REQUEST_TIMEOUT_MS = 4_000;

const BENCHMARK_ROWS = [
  ["Physics", "26.86% schneller", "RoomPhysicsWorkspace ohne per-tick HashMap-Allokation"],
  ["Perception", "26.34% schneller", "generate_perception_into mit wiederverwendeten Puffern"],
  ["Persist e2e", "52.57% schneller", "26 Events in einer SQLite-Transaktion"],
  ["Persist write-only", "86.95% schneller", "prebuilt Event-Pfad isoliert Store-Writes"],
  ["Full tick", "17.23% schneller", "26-Agenten-Tick Regression-Guard"],
  ["Phase-Timing #381", "<0.1% Tick-Budget", "10 Histogramm-Records ~0.3 µs/Tick; full-tick Delta im Messrauschen (129.78 vs 129.54 µs)"],
] as const;

function MetricCard(props: { label: string; value: string; tone?: "warn" | "danger" | "ok" }): JSX.Element {
  return (
    <div class={`metric-card ${props.tone ? `metric-card--${props.tone}` : ""}`}>
      <div class="metric-card__value">{props.value}</div>
      <div class="metric-card__label">{props.label}</div>
    </div>
  );
}

function measuredValue(value: number | null | undefined, kind: "count" | "duration" | "ratio"): number | null {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return null;
  if (kind === "count" && !Number.isSafeInteger(value)) return null;
  if (kind === "ratio" && value > 1) return null;
  return value;
}

function measuredText(value: number | null, format: (value: number) => string): string {
  return value == null ? "N/A" : format(value);
}

export function MetricsView(): JSX.Element {
  const [ebpf, setEbpf] = createSignal<EbpfMetrics | null>(null);
  const [ebpfLoaded, setEbpfLoaded] = createSignal(false);
  const [pipeline, setPipeline] = createSignal<PipelineMetrics | null>(null);
  const [pipelineLoaded, setPipelineLoaded] = createSignal(false);
  const [tick, setTick] = createSignal<TickMetrics | null>(null);
  const [tickLoaded, setTickLoaded] = createSignal(false);
  let generation = 0;
  let disposed = false;
  let controllers: AbortController[] = [];
  const timeouts = new Set<number>();

  const cancelRequests = () => {
    for (const timeout of timeouts) window.clearTimeout(timeout);
    timeouts.clear();
    for (const controller of controllers) controller.abort();
    controllers = [];
  };

  const loadExtras = () => {
    if (disposed) return;
    const currentGeneration = ++generation;
    cancelRequests();

    const load = <T,>(path: string, publish: (value: T | null) => void) => {
      const controller = new AbortController();
      controllers.push(controller);
      let settled = false;
      const finish = (value: T | null) => {
        if (settled || disposed || currentGeneration !== generation || controller.signal.aborted) return;
        settled = true;
        window.clearTimeout(timeout);
        timeouts.delete(timeout);
        publish(value);
      };
      const timeout = window.setTimeout(() => {
        finish(null);
        controller.abort();
      }, METRICS_REQUEST_TIMEOUT_MS);
      timeouts.add(timeout);
      void apiJson<T>(path, { signal: controller.signal }).then(finish, () => finish(null));
    };

    load<EbpfMetrics>("/api/metrics/ebpf", (value) => {
      setEbpf(value);
      setEbpfLoaded(true);
    });
    load<PipelineMetrics>("/api/metrics/pipeline", (value) => {
      setPipeline(value);
      setPipelineLoaded(true);
    });
    load<TickMetrics>("/api/metrics/tick", (value) => {
      setTick(value);
      setTickLoaded(true);
    });
  };

  const resource = () => ebpf()?.available === true ? ebpf() : null;
  const agentBlockIo = () => resource()?.io_source === "agent_cgroup_io_stat" ? resource() : null;
  const tickResource = () => tick()?.available === true ? tick() : null;
  const stalledCount = () => measuredValue(resource()?.stalled_count, "count");
  const ringBufferDrops = () => measuredValue(resource()?.ring_buffer_drops, "count");
  const mode = () => {
    const value = resource()?.mode;
    return typeof value === "string" && value.trim() && !["unknown", "unavailable", "offline"].includes(value.trim().toLowerCase())
      ? value.trim()
      : "N/A";
  };

  onMount(() => {
    void loadExtras();
    const timer = window.setInterval(() => void loadExtras(), METRICS_REFRESH_MS);
    onCleanup(() => {
      disposed = true;
      ++generation;
      window.clearInterval(timer);
      cancelRequests();
    });
  });

  return (
    <section class="col view-panel" data-testid="view-metrics">
      <div class="col__head view-head">
        <span>Metrics</span>
        <span class={`pill ${pipeline()?.available ? "pill-ok" : pipelineLoaded() ? "pill-warn" : ""}`}>
          {!pipelineLoaded() ? "Gateway loading" : pipeline()?.available ? "Gateway ok" : "Gateway offline"}
        </span>
      </div>
      <div class="col__body view-body">
        <Show when={consoleStore.kpi} fallback={<p class="muted">Warte auf kpi-Push.</p>}>
          {(kpi) => (
            <div class="metrics-grid" data-testid="kpi-grid">
              <MetricCard label="Aktive Agents" value={activeAgentCount() == null ? "N/A" : formatNumber(activeAgentCount()!)} />
              <MetricCard label="Agenten-Delta / Minute" value={formatNumber(kpi().active_agents)} />
              <MetricCard label="Agents im Push" value={formatNumber(consoleStore.agents.length)} />
              <MetricCard label="Aktionen" value={formatNumber(kpi().total_actions)} />
              <MetricCard label="Transits" value={formatNumber(kpi().total_transits)} />
              <MetricCard label="Chaos Events" value={formatNumber(kpi().chaos_events)} tone={kpi().chaos_events > 0 ? "warn" : undefined} />
              <MetricCard label="Schichtwechsel" value={formatNumber(kpi().shift_changes)} />
              <MetricCard label="Nightrun Events" value={formatNumber(kpi().nightrun_events)} />
              <MetricCard label="Tick Count" value={measuredText(measuredValue(kpi().tick_count, "count"), formatNumber)} />
              <MetricCard label="Bucket" value={formatBucket(kpi().bucket_start)} />
            </div>
          )}
        </Show>

        <section class="metrics-section">
          <h3>eBPF</h3>
          <span class="muted" role="status" data-testid="ebpf-status">
            {!ebpfLoaded() ? "Loading" : resource() ? "Scrape available" : "Offline"}
          </span>
          <div class="metrics-grid metrics-grid--compact">
            <MetricCard label="Mode" value={mode()} />
            <MetricCard label="Stalled Agents" value={measuredText(stalledCount(), formatNumber)} tone={(stalledCount() ?? 0) > 0 ? "danger" : undefined} />
            <MetricCard label="Collection Cycle" value={measuredText(measuredValue(resource()?.collection_cycle_us, "duration"), (value) => `${value} µs`)} />
            <MetricCard label="Ring Buffer Drops" value={measuredText(ringBufferDrops(), formatNumber)} tone={(ringBufferDrops() ?? 0) > 0 ? "warn" : undefined} />
            <MetricCard label="I/O Source" value={agentBlockIo() ? "Agent cgroup block I/O" : "N/A"} />
            <MetricCard label="Agent-wide Block I/O Read Total" value={measuredText(measuredValue(agentBlockIo()?.io_read_bytes, "count"), formatBytes)} />
            <MetricCard label="Agent-wide Block I/O Write Total" value={measuredText(measuredValue(agentBlockIo()?.io_write_bytes, "count"), formatBytes)} />
            <MetricCard label="Avg PSI Stress" value={measuredText(measuredValue(resource()?.avg_stress, "ratio"), (value) => `${(value * 100).toFixed(1)}%`)} />
          </div>
          <Show when={(resource()?.stalled_agents ?? []).length > 0}>
            <div class="metric-detail-list">
              <strong>Stalled Agents:</strong>
              <For each={resource()?.stalled_agents ?? []}>
                {(agent) => <span class="pill">{agent.agent} ({measuredText(measuredValue(agent.seconds, "duration"), (value) => `${value}s`)})</span>}
              </For>
            </div>
          </Show>
        </section>

        <section class="metrics-section">
          <h3>Pipeline</h3>
          <Show
            when={pipeline()?.available}
            fallback={<div class="degraded-panel" data-testid="pipeline-offline">{pipelineLoaded() ? "Gateway offline" : "Gateway loading"}</div>}
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
          <span class="muted" role="status" data-testid="tick-status">
            {!tickLoaded() ? "Loading" : tickResource() ? "Scrape available" : "Offline"}
          </span>
          <div class="metrics-grid metrics-grid--compact">
            <MetricCard label="Tick Duration" value={measuredText(measuredValue(tickResource()?.tick_duration_ms, "duration"), formatMs)} />
            <MetricCard label="Effective Rate" value={measuredText(measuredValue(tickResource()?.tick_rate_effective_ms, "duration"), formatMs)} />
            <MetricCard label="PSI CPU" value={measuredText(measuredValue(tickResource()?.psi_cpu_avg10, "ratio"), (value) => `${(value * 100).toFixed(1)}%`)} />
            <MetricCard label="PSI Mem" value={measuredText(measuredValue(tickResource()?.psi_mem_avg10, "ratio"), (value) => `${(value * 100).toFixed(1)}%`)} />
            <MetricCard label="PSI IO" value={measuredText(measuredValue(tickResource()?.psi_io_avg10, "ratio"), (value) => `${(value * 100).toFixed(1)}%`)} />
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
