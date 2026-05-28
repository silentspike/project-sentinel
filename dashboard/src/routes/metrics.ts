import { Hono } from "hono";
import { getLatestKpi, getProjectionLag, getActiveAgents, getTotalEventCount, getEventRatePerMinute, getRecentEvolutionAlerts, getLastNightrunStats } from "../db";
import type { BenchmarkSnapshotResponse, MetricsResponse, HealthResponse } from "../types";

export const metricRoutes = new Hono();

const startTime = Date.now();

const ISSUE_276_BENCHMARKS: BenchmarkSnapshotResponse = {
  issue: 276,
  title: "ECS tick-loop hot-path",
  measured_at: "2026-05-28T18:33:14+02:00",
  host: "sentinel-ubuntu-2404",
  cpu: "Intel Core i7-3930K @ 3.20 GHz (2011, KVM, 8 vCPU, taskset core 2)",
  comparison_scope: "Deploy-VM same-machine before/after only; TOGAF absolute baselines intentionally not used",
  notes: [
    "Measured on ubuntu@10.0.0.240 with sentinel-gateway and health-monitor timer stopped.",
    "Deploy config excludes sentinel-gateway from platform-controlplane monitored_services so gateway-off smokes stay stable.",
    "Persist benches are storage-bound on this VM; relative deltas are valid, absolute latency is not a production-baseline gate.",
    "prepare_cached/IMMEDIATE EventStore trial was rejected after mixed A/B results; gateway self-heal restart was treated as a benchmark anomaly and fixed by config.",
  ],
  results: [
    {
      id: "physics",
      label: "Physics",
      before_benchmark: "issue276.physics_system/tick_26_agents",
      after_benchmark: "issue276.physics_system/tick_26_agents",
      before_ns_per_iter: 1_172_325,
      before_stddev_ns_per_iter: 35_864,
      after_ns_per_iter: 857_440,
      after_stddev_ns_per_iter: 24_774,
      improvement_percent: 26.86,
      system_metrics_log_dir: "/tmp/issue-276-bench/logs clean-baseline-tick clean-after2-tick",
      system_metrics_summary: "CPU-pinned run; mpstat iowait 0.01%, sda util below 0.3%.",
      note: "RoomPhysicsWorkspace removes per-tick HashMap allocation and keeps room aggregation in reusable Vec storage.",
    },
    {
      id: "perception",
      label: "Perception",
      before_benchmark: "issue276.generate_perception/texts_26_agents",
      after_benchmark: "issue276.generate_perception/texts_26_agents",
      before_ns_per_iter: 149_127,
      before_stddev_ns_per_iter: 3_788,
      after_ns_per_iter: 109_845,
      after_stddev_ns_per_iter: 4_944,
      improvement_percent: 26.34,
      system_metrics_log_dir: "/tmp/issue-276-bench/logs clean-baseline-tick clean-after2-tick",
      system_metrics_summary: "CPU-pinned run; same tick benchmark pass as physics.",
      note: "generate_perception_into reuses caller-owned buffers and static text fragments where output is deterministic.",
    },
    {
      id: "persist",
      label: "Persist e2e",
      before_benchmark: "issue276.persist_26_events_individual_tx",
      after_benchmark: "issue276.persist_26_events_batch_tx",
      before_ns_per_iter: 40_377_309,
      before_stddev_ns_per_iter: 17_368_530,
      after_ns_per_iter: 19_151_838,
      after_stddev_ns_per_iter: 6_010_275,
      improvement_percent: 52.57,
      system_metrics_log_dir: "/tmp/issue-276-bench/logs clean-baseline-persist-individual clean-after2-persist-batch",
      system_metrics_summary: "Storage-bound SQLite path; mpstat iowait about 8%, sda util about 63-65%.",
      note: "append_with_outbox_batch writes all 26 events/outbox rows in one SQLite transaction.",
    },
    {
      id: "persist-prebuilt",
      label: "Persist write-only",
      before_benchmark: "issue276.persist_26_events_individual_tx_prebuilt",
      after_benchmark: "issue276.persist_26_events_batch_tx_prebuilt",
      before_ns_per_iter: 34_143_782,
      before_stddev_ns_per_iter: 12_614_457,
      after_ns_per_iter: 4_454_866,
      after_stddev_ns_per_iter: 1_560_601,
      improvement_percent: 86.95,
      system_metrics_log_dir: "/tmp/issue-276-bench/logs clean2-after2-persist-individual-prebuilt clean2-after2-persist-batch-prebuilt",
      system_metrics_summary: "Events/topics prebuilt to isolate store writes; still storage-bound on sda.",
      note: "This isolates EventStore write behavior from UUID/getrandom and event construction overhead.",
    },
    {
      id: "bio-tick",
      label: "Full tick",
      before_benchmark: "room_phase2.bio_tick_26_agents",
      after_benchmark: "room_phase2.bio_tick_26_agents",
      before_ns_per_iter: 2_357_373,
      before_stddev_ns_per_iter: 186_539,
      after_ns_per_iter: 1_951_265,
      after_stddev_ns_per_iter: 84_290,
      improvement_percent: 17.23,
      system_metrics_log_dir: "/tmp/issue-276-bench/logs clean-baseline-bio clean-after2-bio",
      system_metrics_summary: "CPU-pinned full tick run; mpstat iowait 0.01%, sda util below 0.5%.",
      note: "Regression guard: total 26-agent tick improved on the same VM.",
    },
  ],
};

const ISSUE_277_BENCHMARKS = {
  issue: 277,
  title: "Dashboard WebSocket polling",
  measured_at: "2026-05-28T21:18:00+02:00",
  host: "sentinel-ubuntu-2404",
  cpu: "Intel Core i7-3930K @ 3.20 GHz (2011, KVM, 8 vCPU)",
  comparison_scope: "Deploy-VM same-machine before/after only; syscall count is a relative proxy, AC-1 is proven by one global Projection watermark SQL query in the poll path",
  query_count_per_poll: 1,
  query_plan: "SEARCH projection_watermarks USING INDEX sqlite_autoindex_projection_watermarks_1 (projection_name=?)",
  notes: [
    "Before build: d44f89e (#276 head). After build: feat/issue-277-dashboard-polling deployed to /opt/sentinel/dashboard and /opt/sentinel/bin/sentinel-projection.",
    "Bun on the Deploy-VM must use the linux-x64-baseline binary because i7-3930K lacks AVX2; /usr/local/bin/bun was updated to baseline v1.3.14 before measurement.",
    "System metrics captured with vmstat 1, mpstat 1, and iostat -x 1 in parallel with strace.",
    "Rejected optimization trial: direct kpi_1m MAX union scanned 36k KPI buckets without indexes and measured 615 pread64 calls/10s; indexed variant still measured 18/10s.",
    "Rejected optimization trial: EventStore projection_offsets lookup mixed Dashboard polling with EventStore/health traffic and measured >100k pread64 calls/10s under tab load.",
    "Final browser verification used 3 simultaneous Playwright tabs for 30 seconds with no ERR_INSUFFICIENT_RESOURCES; only expected 502 traffic-stats console entries appeared because cortex-gateway stayed stopped.",
  ],
  results: [
    {
      id: "ws-poll-pread64-idle",
      label: "Idle Dashboard pread64 calls",
      before_reference: "d44f89e dashboard",
      after_reference: "feat/issue-277-dashboard-polling",
      duration_seconds: 10,
      before_pread64_calls: 13,
      after_pread64_calls: 11,
      reduction_percent: 15.38,
      system_metrics_log_dir: "/tmp/issue277-before-ws-only and /tmp/issue277-after-idle-skip-ws-only on Deploy-VM",
      system_metrics_summary: "Before mpstat idle 99.61%, iowait 0.01%; after idle 99.64%, iowait 0.00%. strace -yy showed remaining idle pread64 calls on /proc/<pid>/statm, not projection.db.",
      note: "When no WebSocket clients are connected, the dashboard now skips change-detection and health DB reads entirely.",
    },
    {
      id: "ws-poll-pread64-3tabs-after",
      label: "Active 3-tab Dashboard pread64 calls",
      after_reference: "feat/issue-277-dashboard-polling",
      duration_seconds: 10,
      after_pread64_calls: 18,
      system_metrics_log_dir: "/tmp/issue277-after-3tabs on Deploy-VM",
      system_metrics_summary: "mpstat average: user 0.17%, system 0.21%, iowait 0.01%, idle 99.57%; gateway inactive.",
      note: "Active-client steady state performs one projection_watermarks lookup per poll. Health updates still read EventStore lag every 5s as designed.",
    },
  ],
};

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

metricRoutes.get("/metrics/benchmarks", (c) => c.json(ISSUE_276_BENCHMARKS));
metricRoutes.get("/metrics/benchmarks/277", (c) => c.json(ISSUE_277_BENCHMARKS));

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
