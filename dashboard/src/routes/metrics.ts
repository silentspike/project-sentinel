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
