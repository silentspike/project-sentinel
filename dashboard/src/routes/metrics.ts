import { Hono } from "hono";
import { getLatestKpi, getProjectionLag, getActiveAgents, getTotalEventCount, getEventRatePerMinute, getRecentEvolutionAlerts, getLastNightrunStats } from "../db";
import type { MetricsResponse, HealthResponse } from "../types";

export const metricRoutes = new Hono();

const startTime = Date.now();

metricRoutes.get("/metrics", async (c) => {
  const kpi = getLatestKpi();
  const agents = getActiveAgents();
  const response: MetricsResponse = {
    active_agents: agents.length,
    total_actions: kpi?.total_actions ?? 0,
    total_transits: kpi?.total_transits ?? 0,
    chaos_events: kpi?.chaos_events ?? 0,
    tick_count: kpi?.tick_count ?? 0,
    shift_changes: kpi?.shift_changes ?? 0,
    nightrun_events: kpi?.nightrun_events ?? 0,
    bucket_start: kpi?.bucket_start ?? null,
    uptime: Math.floor((Date.now() - startTime) / 1000),
    total_events: getTotalEventCount(),
    event_rate_per_min: getEventRatePerMinute(),
  };
  // Evolution/MARBLE Daten anhaengen
  const evolution = getRecentEvolutionAlerts(24);
  const nightrun = getLastNightrunStats();

  // Tick-Dauer + PSI aus Daemon Prometheus (:9090)
  let tick_duration_ms = 0;
  let tick_rate_effective_ms = 0;
  let psi_cpu = 0;
  let psi_mem = 0;
  let psi_io = 0;
  try {
    const promResp = await fetch("http://localhost:9090/metrics", {
      signal: AbortSignal.timeout(1000),
    });
    if (promResp.ok) {
      const text = await promResp.text();
      const p = (name: string): number => {
        const m = text.match(new RegExp(`${name}\\s+(\\d+)`));
        return m ? parseInt(m[1], 10) : 0;
      };
      tick_duration_ms = p("sentinel_tick_duration_ms");
      tick_rate_effective_ms = p("sentinel_tick_rate_effective_ms");
      psi_cpu = p("sentinel_psi_cpu_avg10");
      psi_mem = p("sentinel_psi_mem_avg10");
      psi_io = p("sentinel_psi_io_avg10");
    }
  } catch {
    // Daemon Prometheus nicht erreichbar — Defaults (0)
  }

  return c.json({
    ...response,
    evolution_count: evolution.length,
    evolution_drifts: evolution.filter((e) => e.change_type === "drift").length,
    evolution_fatigue: evolution.filter((e) => e.change_type === "fatigue_spike").length,
    evolution_quality: evolution.filter((e) => e.change_type === "quality_shift").length,
    nightrun_consolidated: nightrun?.consolidated ?? 0,
    nightrun_failed: nightrun?.failed ?? 0,
    tick_duration_ms,
    tick_rate_effective_ms,
    psi_cpu: psi_cpu / 1000,
    psi_mem: psi_mem / 1000,
    psi_io: psi_io / 1000,
  });
});

metricRoutes.get("/ebpf/status", async (c) => {
  try {
    const resp = await fetch("http://localhost:9090/metrics", {
      signal: AbortSignal.timeout(2000),
    });
    if (!resp.ok) {
      return c.json({ mode: "unavailable" });
    }
    const text = await resp.text();
    // Parse sentinel_ebpf_monitoring_mode{mode="kernel"} 1
    const match = text.match(/sentinel_ebpf_monitoring_mode\{mode="(\w+)"\}\s+1/);
    return c.json({ mode: match ? match[1] : "unknown" });
  } catch {
    return c.json({ mode: "unavailable" });
  }
});

// eBPF Metrics Endpoint — parses Prometheus text from daemon :9090
metricRoutes.get("/ebpf/metrics", async (c) => {
  try {
    const resp = await fetch("http://localhost:9090/metrics", {
      signal: AbortSignal.timeout(5000),
    });
    if (!resp.ok) {
      return c.json({ available: false, mode: "unavailable" });
    }
    const text = await resp.text();

    // Parse mode
    const modeMatch = text.match(/sentinel_ebpf_monitoring_mode\{mode="(\w+)"\}\s+1/);
    const mode = modeMatch ? modeMatch[1] : "unknown";

    // Parse stalled count (handle optional labels)
    const stalledMatch = text.match(/sentinel_agent_stalled_total(?:\{[^}]*\})?\s+(\d+)/);
    const stalledCount = stalledMatch ? parseInt(stalledMatch[1], 10) : 0;

    // Parse stalled agent names
    const stalledAgents: { agent: string; seconds: number }[] = [];
    const stalledRe = /sentinel_agent_stalled\{cgroup_id="[^"]*",agent="([^"]*)"\}\s+1/g;
    const secsRe = /sentinel_agent_last_write_seconds\{cgroup_id="[^"]*",agent="([^"]*)"\}\s+(\d+)/g;
    const secsMap = new Map<string, number>();
    let m;
    while ((m = secsRe.exec(text)) !== null) {
      secsMap.set(m[1], parseInt(m[2], 10));
    }
    while ((m = stalledRe.exec(text)) !== null) {
      stalledAgents.push({ agent: m[1], seconds: secsMap.get(m[1]) ?? 0 });
    }

    // Parse collection cycle
    const cycleMatch = text.match(/sentinel_ebpf_collector_cycle_microseconds(?:\{[^}]*\})?\s+(\d+)/);
    const cycleUs = cycleMatch ? parseInt(cycleMatch[1], 10) : 0;

    // Parse ring buffer drops
    const dropsMatch = text.match(/sentinel_ebpf_ring_buffer_drops_total(?:\{[^}]*\})?\s+(\d+)/);
    const drops = dropsMatch ? parseInt(dropsMatch[1], 10) : 0;

    // Parse I/O totals (sum across all cgroups)
    let ioReadBytes = 0;
    let ioWriteBytes = 0;
    const ioReadRe = /sentinel_io_bytes_total\{[^}]*direction="read"\}\s+(\d+)/g;
    const ioWriteRe = /sentinel_io_bytes_total\{[^}]*direction="write"\}\s+(\d+)/g;
    while ((m = ioReadRe.exec(text)) !== null) {
      ioReadBytes += parseInt(m[1], 10);
    }
    while ((m = ioWriteRe.exec(text)) !== null) {
      ioWriteBytes += parseInt(m[1], 10);
    }

    // Parse PSI stress
    let totalStress = 0;
    let stressCount = 0;
    const psiRe = /sentinel_agent_cpu_pressure_stress\{agent="[^"]*"\}\s+([\d.]+)/g;
    while ((m = psiRe.exec(text)) !== null) {
      totalStress += parseFloat(m[1]);
      stressCount++;
    }

    return c.json({
      available: true,
      mode,
      stalled_count: stalledCount,
      stalled_agents: stalledAgents,
      collection_cycle_us: cycleUs,
      ring_buffer_drops: drops,
      io_read_bytes: ioReadBytes,
      io_write_bytes: ioWriteBytes,
      avg_stress: stressCount > 0 ? totalStress / stressCount : 0,
    });
  } catch {
    return c.json({ available: false, mode: "unavailable" });
  }
});

// ── Pipeline Metrics — Latenz + Tokens + Requests aus Cortex :8080 ──

const CORTEX_PROXY_URL =
  process.env.CORTEX_PROXY_URL || "http://localhost:8080";

interface PipelineProvider {
  provider: string;
  latency_avg_s: number;
  latency_count: number;
  requests_ok: number;
  requests_error: number;
  tokens_input: number;
  tokens_output: number;
}

metricRoutes.get("/metrics/pipeline", async (c) => {
  try {
    const resp = await fetch(`${CORTEX_PROXY_URL}/metrics`, {
      signal: AbortSignal.timeout(3000),
    });
    if (!resp.ok) {
      return c.json({ available: false });
    }
    const text = await resp.text();

    // Collect providers from latency histogram
    const providers = new Map<string, PipelineProvider>();

    const ensureProvider = (name: string): PipelineProvider => {
      if (!providers.has(name)) {
        providers.set(name, {
          provider: name,
          latency_avg_s: 0,
          latency_count: 0,
          requests_ok: 0,
          requests_error: 0,
          tokens_input: 0,
          tokens_output: 0,
        });
      }
      return providers.get(name)!;
    };

    // Parse latency: sentinel_pipeline_latency_seconds_sum{provider="X"} N
    let m: RegExpExecArray | null;
    const sumRe =
      /sentinel_pipeline_latency_seconds_sum\{provider="([^"]+)"\}\s+([\d.e+-]+)/g;
    const countRe =
      /sentinel_pipeline_latency_seconds_count\{provider="([^"]+)"\}\s+(\d+)/g;

    while ((m = sumRe.exec(text)) !== null) {
      const p = ensureProvider(m[1]);
      p.latency_avg_s = parseFloat(m[2]);
    }
    while ((m = countRe.exec(text)) !== null) {
      const p = ensureProvider(m[1]);
      const count = parseInt(m[2], 10);
      if (count > 0) {
        p.latency_avg_s = p.latency_avg_s / count;
      }
      p.latency_count = count;
    }

    // Parse requests: sentinel_pipeline_requests_total{provider="X",status="ok|error"} N
    const reqRe =
      /sentinel_pipeline_requests_total\{provider="([^"]+)",status="([^"]+)"\}\s+(\d+)/g;
    while ((m = reqRe.exec(text)) !== null) {
      const p = ensureProvider(m[1]);
      if (m[2] === "ok") {
        p.requests_ok = parseInt(m[3], 10);
      } else {
        p.requests_error = parseInt(m[3], 10);
      }
    }

    // Parse tokens: sentinel_pipeline_tokens_total{direction="input|output",provider="X"} N
    const tokRe =
      /sentinel_pipeline_tokens_total\{direction="([^"]+)",provider="([^"]+)"\}\s+(\d+)/g;
    while ((m = tokRe.exec(text)) !== null) {
      const p = ensureProvider(m[2]);
      if (m[1] === "input") {
        p.tokens_input = parseInt(m[3], 10);
      } else {
        p.tokens_output = parseInt(m[3], 10);
      }
    }

    return c.json({
      available: true,
      providers: Array.from(providers.values()),
    });
  } catch {
    return c.json({ available: false, providers: [] });
  }
});

// ── Tick Duration + PSI Metrics aus Daemon :9090 Prometheus ──

metricRoutes.get("/metrics/tick", async (c) => {
  try {
    const resp = await fetch("http://localhost:9090/metrics", {
      signal: AbortSignal.timeout(2000),
    });
    if (!resp.ok) {
      return c.json({ available: false });
    }
    const text = await resp.text();

    const parse = (name: string): number => {
      const re = new RegExp(`${name}\\s+(\\d+)`);
      const m = text.match(re);
      return m ? parseInt(m[1], 10) : 0;
    };

    return c.json({
      available: true,
      tick_duration_ms: parse("sentinel_tick_duration_ms"),
      tick_rate_effective_ms: parse("sentinel_tick_rate_effective_ms"),
      psi_cpu_avg10: parse("sentinel_psi_cpu_avg10") / 1000,
      psi_mem_avg10: parse("sentinel_psi_mem_avg10") / 1000,
      psi_io_avg10: parse("sentinel_psi_io_avg10") / 1000,
    });
  } catch {
    return c.json({ available: false });
  }
});

metricRoutes.get("/health", (c) => {
  let lag = 0;
  try {
    lag = getProjectionLag();
  } catch {
    // EventStore DB nicht verfuegbar — Lag = 0
  }
  const response: HealthResponse = {
    status: "ok",
    uptime: Math.floor((Date.now() - startTime) / 1000),
    projection_lag: lag,
  };
  return c.json(response);
});
