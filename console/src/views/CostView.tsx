import { createSignal, For, onMount, onCleanup, type JSX } from "solid-js";
import { apiJson, type CostStats, type CostRow } from "../api";
import { formatNumber } from "./format";

// #427: cache-aware cost/token view per agent + tier, plus a minute time-series
// sparkline. Reads the CostHandler projection via GET /api/cost (1:n — the cost
// info lives once as the AgentLlmUsage event sequence; this view only reads it).

const EMPTY: CostStats = { by_agent: [], by_tier: [], time_series: [] };
const GRID = "110px 1fr 1fr 1fr 1fr 64px 96px";

function formatUsd(v: number): string {
  return Number.isFinite(v) ? `$${v.toFixed(4)}` : "$0.0000";
}

function Sparkline(props: { rows: CostRow[] }): JSX.Element {
  const w = 240;
  const h = 40;
  const points = (): string => {
    const vals = props.rows.map((r) => r.cost_usd);
    if (vals.length === 0) return "";
    const max = Math.max(...vals, 1e-9);
    return vals
      .map((v, i) => {
        const x = vals.length <= 1 ? w : (i / (vals.length - 1)) * w;
        const y = h - (v / max) * h;
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(" ");
  };
  return (
    <svg
      data-testid="cost-sparkline"
      width={w}
      height={h}
      viewBox={`0 0 ${w} ${h}`}
      style={{ overflow: "visible" }}
    >
      <polyline points={points()} fill="none" stroke="var(--accent, #6cf)" stroke-width="2" />
    </svg>
  );
}

function CostTable(props: {
  testid: string;
  rowTestid: string;
  keyLabel: string;
  rows: CostRow[];
}): JSX.Element {
  const header = {
    display: "grid",
    "grid-template-columns": GRID,
    gap: "8px",
    "font-size": "11px",
  } as const;
  return (
    <div data-testid={props.testid}>
      <div class="muted" style={header}>
        <span>{props.keyLabel}</span>
        <span>Input</span>
        <span>Output</span>
        <span>Cache R</span>
        <span>Cache W</span>
        <span>Calls</span>
        <span>Cost</span>
      </div>
      <For each={props.rows}>
        {(r) => (
          <div
            data-testid={props.rowTestid}
            style={{
              display: "grid",
              "grid-template-columns": GRID,
              gap: "8px",
              "font-size": "12px",
            }}
          >
            <span style={{ "font-family": "monospace" }}>{r.key}</span>
            <span>{formatNumber(r.input_tokens)}</span>
            <span>{formatNumber(r.output_tokens)}</span>
            <span data-testid="cost-cache-read">{formatNumber(r.cache_read)}</span>
            <span data-testid="cost-cache-creation">{formatNumber(r.cache_creation)}</span>
            <span>{r.call_count}</span>
            <span>{formatUsd(r.cost_usd)}</span>
          </div>
        )}
      </For>
    </div>
  );
}

export function CostView(): JSX.Element {
  const [stats, setStats] = createSignal<CostStats>(EMPTY);

  async function load(): Promise<void> {
    try {
      const v = await apiJson<CostStats>("/api/cost");
      if (v) {
        setStats({
          by_agent: v.by_agent ?? [],
          by_tier: v.by_tier ?? [],
          time_series: v.time_series ?? [],
          projection: v.projection,
        });
      }
    } catch {
      /* keep last good data on a transient read error */
    }
  }

  onMount(() => {
    void load();
    const timer = window.setInterval(() => void load(), 10_000);
    onCleanup(() => window.clearInterval(timer));
  });

  return (
    <div
      data-testid="view-cost"
      class="col"
      style={{ gap: "12px", padding: "12px", overflow: "auto", height: "100%" }}
    >
      <section class="control-card">
        <h3 style={{ margin: "0 0 4px" }}>Kosten-Zeitreihe (USD pro Minute)</h3>
        <p class="muted" style={{ "margin-top": 0, "font-size": "12px" }}>
          {stats().time_series.length} Minuten-Buckets aus der AgentLlmUsage-Event-Sequenz.
        </p>
        <Sparkline rows={stats().time_series} />
      </section>

      <section class="control-card">
        <h3 style={{ margin: "0 0 8px" }}>Kosten/Tokens pro Agent</h3>
        <CostTable
          testid="cost-agent-table"
          rowTestid="cost-agent-row"
          keyLabel="Agent"
          rows={stats().by_agent}
        />
      </section>

      <section class="control-card">
        <h3 style={{ margin: "0 0 8px" }}>Kosten/Tokens pro Tier</h3>
        <CostTable
          testid="cost-tier-table"
          rowTestid="cost-tier-row"
          keyLabel="Tier"
          rows={stats().by_tier}
        />
      </section>
    </div>
  );
}
