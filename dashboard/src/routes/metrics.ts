import { Hono } from "hono";
import { getLatestKpi, getProjectionLag, getActiveAgents, getTotalEventCount, getEventRatePerMinute, getRecentEvolutionAlerts, getLastNightrunStats } from "../db";
import type { MetricsResponse, HealthResponse } from "../types";

export const metricRoutes = new Hono();

const startTime = Date.now();

metricRoutes.get("/metrics", (c) => {
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
  return c.json({
    ...response,
    evolution_count: evolution.length,
    evolution_drifts: evolution.filter((e) => e.change_type === "drift").length,
    evolution_fatigue: evolution.filter((e) => e.change_type === "fatigue_spike").length,
    evolution_quality: evolution.filter((e) => e.change_type === "quality_shift").length,
    nightrun_consolidated: nightrun?.consolidated ?? 0,
    nightrun_failed: nightrun?.failed ?? 0,
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
      signal: AbortSignal.timeout(2000),
    });
    if (!resp.ok) {
      return c.json({ available: false, mode: "unavailable" });
    }
    const text = await resp.text();

    // Parse mode
    const modeMatch = text.match(/sentinel_ebpf_monitoring_mode\{mode="(\w+)"\}\s+1/);
    const mode = modeMatch ? modeMatch[1] : "unknown";

    // Parse stalled count
    const stalledMatch = text.match(/sentinel_agent_stalled_total\s+(\d+)/);
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
    const cycleMatch = text.match(/sentinel_ebpf_collector_cycle_microseconds\s+(\d+)/);
    const cycleUs = cycleMatch ? parseInt(cycleMatch[1], 10) : 0;

    // Parse ring buffer drops
    const dropsMatch = text.match(/sentinel_ebpf_ring_buffer_drops_total\s+(\d+)/);
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
