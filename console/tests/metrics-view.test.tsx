import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, within } from "@solidjs/testing-library";
import { MetricsView } from "../src/views/MetricsView";

type View = ReturnType<typeof render>;
type Failure = "http" | "network" | null;

const readTotal = "Agent-wide Block I/O Read Total";
const writeTotal = "Agent-wide Block I/O Write Total";

function measured(overrides: Record<string, unknown> = {}) {
  return {
    available: true,
    mode: "measured",
    stalled_count: 2,
    stalled_agents: [{ agent: "AGENT-17", seconds: 12 }, { agent: "AGENT-42" }],
    collection_cycle_us: 250,
    ring_buffer_drops: 7,
    io_source: "agent_cgroup_io_stat",
    io_read_bytes: 1_536,
    io_write_bytes: 3_145_728,
    avg_stress: 0.375,
    ...overrides,
  };
}

function measuredTick(overrides: Record<string, unknown> = {}) {
  return { available: true, tick_duration_ms: 4, tick_rate_effective_ms: 1_000,
    psi_cpu_avg10: 0.125, psi_mem_avg10: 0.25, psi_io_avg10: 0.5, ...overrides };
}

function measuredPipeline(provider: string) {
  return { available: true, providers: [{ provider, latency_avg_s: 0, latency_count: 0,
    requests_ok: 0, requests_error: 0, tokens_input: 0, tokens_output: 0 }] };
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function mockMetrics(initial: unknown) {
  let payload = initial;
  let failure: Failure = null;
  let pendingEbpf: Promise<Response> | null = null;
  let pendingPipeline: Promise<Response> | null = null;
  let pipelinePayload: unknown = { available: true, providers: [] };
  let pipelineFailure: Failure = null;
  let pendingTick: Promise<Response> | null = null;
  let tickPayload: unknown = measuredTick();
  let tickFailure: Failure = null;
  // Mock only the HTTP boundary: the production API helper and view still run.
  const fetcher = vi.fn(async (path: string, _init?: RequestInit) => {
    if (path === "/api/metrics/ebpf") {
      if (pendingEbpf) {
        const pending = pendingEbpf;
        pendingEbpf = null;
        return pending;
      }
      if (failure === "network") throw new TypeError("metrics unavailable");
      if (failure === "http") return jsonResponse({ error: "metrics unavailable" }, 503);
      return jsonResponse(payload);
    }
    if (path === "/api/metrics/pipeline") {
      if (pendingPipeline) {
        const pending = pendingPipeline;
        pendingPipeline = null;
        return pending;
      }
      if (pipelineFailure === "network") throw new TypeError("pipeline metrics unavailable");
      if (pipelineFailure === "http") return jsonResponse({ error: "pipeline metrics unavailable" }, 503);
      return jsonResponse(pipelinePayload);
    }
    if (path === "/api/metrics/tick") {
      if (pendingTick) {
        const pending = pendingTick;
        pendingTick = null;
        return pending;
      }
      if (tickFailure === "network") throw new TypeError("tick metrics unavailable");
      if (tickFailure === "http") return jsonResponse({ error: "tick metrics unavailable" }, 503);
      return jsonResponse(tickPayload);
    }
    throw new Error(`unexpected metrics request: ${path}`);
  });
  vi.stubGlobal("fetch", fetcher);
  return {
    fetcher,
    replace(next: unknown) { payload = next; failure = null; },
    reject(next: Exclude<Failure, null>) { failure = next; },
    deferNextEbpf(response: Promise<Response>) { pendingEbpf = response; },
    replacePipeline(next: unknown) { pipelinePayload = next; pipelineFailure = null; },
    rejectPipeline(next: Exclude<Failure, null>) { pipelineFailure = next; },
    deferNextPipeline(response: Promise<Response>) { pendingPipeline = response; },
    deferNextTick(response: Promise<Response>) { pendingTick = response; },
    replaceTick(next: unknown) { tickPayload = next; tickFailure = null; },
    rejectTick(next: Exclude<Failure, null>) { tickFailure = next; },
  };
}

function expectMetric(view: View, label: string | RegExp, value: string) {
  const labelElement = view.getByText(label, { exact: true });
  const card = labelElement.parentElement;
  expect(card, `card for ${String(label)}`).not.toBeNull();
  expect(within(card!).getByText(value, { exact: true })).toBeDefined();
}

function expectResourcesUnavailable(view: View) {
  expectMetric(view, "Stalled Agents", "N/A");
  expectMetric(view, "Collection Cycle", "N/A");
  expectMetric(view, "Ring Buffer Drops", "N/A");
  expectMetric(view, readTotal, "N/A");
  expectMetric(view, writeTotal, "N/A");
  expectMetric(view, "Avg PSI Stress", "N/A");
}

function ebpfStatus(view: View) {
  return within(view.getByRole("heading", { name: "eBPF" }).parentElement!).getByRole("status").textContent;
}

function tickStatus(view: View) {
  return within(view.getByRole("heading", { name: "Tick / PSI" }).parentElement!).getByRole("status").textContent;
}

function expectTickUnavailable(view: View) {
  expectMetric(view, "Tick Duration", "N/A");
  expectMetric(view, "Effective Rate", "N/A");
  expectMetric(view, "PSI CPU", "N/A");
  expectMetric(view, "PSI Mem", "N/A");
  expectMetric(view, "PSI IO", "N/A");
}

async function mounted() {
  const view = render(() => <MetricsView />);
  // Drain the mount's fetch/JSON promises without advancing the refresh interval.
  await vi.advanceTimersByTimeAsync(0);
  return view;
}

beforeEach(() => { vi.useFakeTimers(); });

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("MetricsView measured resources", () => {
  it("shows numeric byte totals, a finite stress fraction and stalled identities with optional ages", async () => {
    mockMetrics(measured({ stalled_count: 4, stalled_agents: [
      { agent: "AGENT-17", seconds: 12 },
      { agent: "AGENT-42" },
      { agent: "AGENT-63", seconds: null },
      { agent: "AGENT-84", seconds: 0 },
    ] }));
    const view = await mounted();
    expectMetric(view, "Mode", "measured");
    expectMetric(view, "Stalled Agents", "4");
    expectMetric(view, "Collection Cycle", "250 \u00b5s");
    expectMetric(view, "Ring Buffer Drops", "7");
    expectMetric(view, readTotal, "1.5 KB");
    expectMetric(view, writeTotal, "3.0 MB");
    expectMetric(view, "Avg PSI Stress", "37.5%");
    const section = view.getByRole("heading", { name: "eBPF" }).parentElement!;
    expect(within(section).getByText("AGENT-17 (12s)", { exact: true })).toBeDefined();
    expect(within(section).getByText("AGENT-42 (N/A)", { exact: true })).toBeDefined();
    expect(within(section).getByText("AGENT-63 (N/A)", { exact: true })).toBeDefined();
    expect(within(section).getByText("AGENT-84 (0s)", { exact: true })).toBeDefined();
  });

  it("preserves valid measured zeros instead of treating them as missing", async () => {
    mockMetrics(measured({ stalled_count: 0, stalled_agents: [], collection_cycle_us: 0,
      ring_buffer_drops: 0, io_read_bytes: 0, io_write_bytes: 0, avg_stress: 0 }));
    const view = await mounted();
    expectMetric(view, "Mode", "measured");
    expectMetric(view, "Stalled Agents", "0");
    expectMetric(view, "Collection Cycle", "0 \u00b5s");
    expectMetric(view, "Ring Buffer Drops", "0");
    expectMetric(view, readTotal, "0 B");
    expectMetric(view, writeTotal, "0 B");
    expectMetric(view, "Avg PSI Stress", "0.0%");
  });

  it.each(["omitted", "null"] as const)("renders %s resources as N/A even after a successful scrape", async (kind) => {
    const resources = kind === "null" ? { stalled_count: null, collection_cycle_us: null, ring_buffer_drops: null,
      io_read_bytes: null, io_write_bytes: null, avg_stress: null } : {};
    mockMetrics({ available: true, mode: "measured", stalled_agents: [], ...resources });
    const view = await mounted();
    // Mode proves the response loaded; N/A must not merely come from the initial state.
    expectMetric(view, "Mode", "measured");
    expectResourcesUnavailable(view);
  });

  it.each([
    { kind: "malformed", fields: { stalled_count: "2", collection_cycle_us: "250", ring_buffer_drops: true,
      io_read_bytes: "1536", io_write_bytes: { bytes: 3_145_728 }, avg_stress: "0.375" }, age: "12" },
    { kind: "negative", fields: { stalled_count: -1, collection_cycle_us: -1, ring_buffer_drops: -1,
      io_read_bytes: -1, io_write_bytes: -1, avg_stress: -0.1 }, age: -1 },
  ])("renders $kind resource values and stalled age as N/A", async ({ fields, age }) => {
    mockMetrics(measured({ ...fields, stalled_agents: [{ agent: "AGENT-invalid", seconds: age }] }));
    const view = await mounted();
    expectMetric(view, "Mode", "measured");
    expectResourcesUnavailable(view);
    const section = view.getByRole("heading", { name: "eBPF" }).parentElement!;
    expect(within(section).getByText("AGENT-invalid (N/A)", { exact: true })).toBeDefined();
  });

  it("rejects fractional counts without rejecting valid fractional durations and stress", async () => {
    mockMetrics(measured({ stalled_count: 0.5, ring_buffer_drops: 0.5,
      io_read_bytes: 1_536.5, io_write_bytes: 3_145_728.5, collection_cycle_us: 0.5, avg_stress: 0.5 }));
    const view = await mounted();
    expectMetric(view, "Mode", "measured");
    expectMetric(view, "Stalled Agents", "N/A");
    expectMetric(view, "Ring Buffer Drops", "N/A");
    expectMetric(view, readTotal, "N/A");
    expectMetric(view, writeTotal, "N/A");
    expectMetric(view, "Collection Cycle", "0.5 \u00b5s");
    expectMetric(view, "Avg PSI Stress", "50.0%");
  });

  it.each([-0.001, 1.001])("renders stress ratio %s outside [0,1] as N/A without hiding valid totals", async (avg_stress) => {
    mockMetrics(measured({ avg_stress }));
    const view = await mounted();
    expectMetric(view, "Mode", "measured");
    expectMetric(view, "Avg PSI Stress", "N/A");
    expectMetric(view, readTotal, "1.5 KB");
    expectMetric(view, writeTotal, "3.0 MB");
  });

  it("determines each resource's availability independently of the scrape and neighboring fields", async () => {
    mockMetrics(measured({ collection_cycle_us: null, io_read_bytes: null, avg_stress: null }));
    const view = await mounted();
    expectMetric(view, "Mode", "measured");
    expectMetric(view, "Collection Cycle", "N/A");
    expectMetric(view, "Ring Buffer Drops", "7");
    expectMetric(view, readTotal, "N/A");
    expectMetric(view, writeTotal, "3.0 MB");
    expectMetric(view, "Avg PSI Stress", "N/A");
  });

  it("replaces old numeric success with N/A when a refresh reports unavailable despite populated fields", async () => {
    const mock = mockMetrics(measured());
    const view = await mounted();
    expectMetric(view, readTotal, "1.5 KB");
    expectMetric(view, "Avg PSI Stress", "37.5%");
    mock.replace(measured({ available: false }));
    await vi.advanceTimersByTimeAsync(5_000);
    expect(mock.fetcher.mock.calls.filter(([path]) => path === "/api/metrics/ebpf")).toHaveLength(2);
    expectMetric(view, "Mode", "N/A");
    expectResourcesUnavailable(view);
  });

  it("replaces old values on partial refreshes and accepts newly measured totals and stress", async () => {
    const mock = mockMetrics(measured());
    const view = await mounted();
    expectMetric(view, readTotal, "1.5 KB");
    expectMetric(view, writeTotal, "3.0 MB");
    expectMetric(view, "Avg PSI Stress", "37.5%");
    mock.replace({ available: true, mode: "measured", stalled_count: 0, stalled_agents: [],
      io_source: "agent_cgroup_io_stat", io_write_bytes: 0, avg_stress: null });
    await vi.advanceTimersByTimeAsync(5_000);
    expectMetric(view, readTotal, "N/A");
    expectMetric(view, writeTotal, "0 B");
    expectMetric(view, "Avg PSI Stress", "N/A");
    expect(view.queryByText("AGENT-17 (12s)", { exact: true })).toBeNull();
    mock.replace(measured({ io_read_bytes: 512, io_write_bytes: 2_048, avg_stress: 1 }));
    await vi.advanceTimersByTimeAsync(5_000);
    expectMetric(view, readTotal, "512 B");
    expectMetric(view, writeTotal, "2.0 KB");
    expectMetric(view, "Avg PSI Stress", "100.0%");
  });

  it.each(["http", "network"] as const)("clears prior resources on %s rejection and recovers on the next refresh", async (failure) => {
    const mock = mockMetrics(measured());
    const view = await mounted();
    expectMetric(view, readTotal, "1.5 KB");
    expectMetric(view, writeTotal, "3.0 MB");
    expectMetric(view, "Avg PSI Stress", "37.5%");
    mock.reject(failure);
    await vi.advanceTimersByTimeAsync(5_000);
    expectMetric(view, "Mode", "N/A");
    expectResourcesUnavailable(view);
    expect(view.queryByText("AGENT-17 (12s)", { exact: true })).toBeNull();
    mock.replace(measured({ io_read_bytes: 256, io_write_bytes: 0, avg_stress: 0.5 }));
    await vi.advanceTimersByTimeAsync(5_000);
    expectMetric(view, "Mode", "measured");
    expectMetric(view, readTotal, "256 B");
    expectMetric(view, writeTotal, "0 B");
    expectMetric(view, "Avg PSI Stress", "50.0%");
    expect(mock.fetcher.mock.calls.filter(([path]) => path === "/api/metrics/ebpf")).toHaveLength(3);
  });

  it("does not restore an older success after a newer unavailable refresh completes", async () => {
    const oldSuccess = deferred<Response>();
    const mock = mockMetrics(measured({ available: false }));
    // Deliberately ignore abort in the mock so a late result exercises the generation guard.
    mock.deferNextEbpf(oldSuccess.promise);
    const view = await mounted();
    expect(ebpfStatus(view)).toBe("Loading");
    expect(mock.fetcher.mock.calls.filter(([path]) => path === "/api/metrics/ebpf")).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(5_000);
    expect(mock.fetcher.mock.calls.filter(([path]) => path === "/api/metrics/ebpf")).toHaveLength(2);
    expect(ebpfStatus(view)).toBe("Offline");
    expectMetric(view, "Mode", "N/A");
    expectResourcesUnavailable(view);

    oldSuccess.resolve(jsonResponse(measured()));
    await vi.advanceTimersByTimeAsync(0);
    expect(ebpfStatus(view)).toBe("Offline");
    expectMetric(view, "Mode", "N/A");
    expectResourcesUnavailable(view);
    expect(view.queryByText("AGENT-17 (12s)", { exact: true })).toBeNull();
  });

  it("aborts the signals passed to fetch and stops refresh requests on unmount", async () => {
    const pending = deferred<Response>();
    const pendingPipeline = deferred<Response>();
    const pendingTick = deferred<Response>();
    const mock = mockMetrics(measured());
    mock.deferNextEbpf(pending.promise);
    mock.deferNextPipeline(pendingPipeline.promise);
    mock.deferNextTick(pendingTick.promise);
    const view = await mounted();
    const detached = view.getByTestId("view-metrics");
    const beforeUnmount = detached.textContent;
    expect(ebpfStatus(view)).toBe("Loading");
    expect(tickStatus(view)).toBe("Loading");
    expect(mock.fetcher).toHaveBeenCalledTimes(3);
    const signals = mock.fetcher.mock.calls.map(([, init]) => init?.signal);
    for (const signal of signals) {
      expect(signal).toBeTruthy();
      expect(signal?.aborted).toBe(false);
    }
    view.unmount();
    for (const signal of signals) expect(signal?.aborted).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
    pending.resolve(jsonResponse(measured()));
    pendingPipeline.resolve(jsonResponse(measuredPipeline("late-provider")));
    pendingTick.resolve(jsonResponse(measuredTick()));
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(mock.fetcher).toHaveBeenCalledTimes(3);
    expect(detached.textContent).toBe(beforeUnmount);
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each([undefined, null, "block_rq_complete", "proc_vfs"])("does not claim agent-owned bytes without recognized provenance: %s", async (io_source) => {
    mockMetrics(measured({ io_source, io_read_bytes: 0, io_write_bytes: 0 }));
    const view = await mounted();
    expectMetric(view, "Mode", "measured");
    expectMetric(view, readTotal, "N/A");
    expectMetric(view, writeTotal, "N/A");
    expectMetric(view, "Avg PSI Stress", "37.5%");
  });

  it("drops previous byte totals when source coverage becomes unavailable and recovers real zeros", async () => {
    const mock = mockMetrics(measured());
    const view = await mounted();
    expectMetric(view, readTotal, "1.5 KB");
    mock.replace(measured({ io_source: null }));
    await vi.advanceTimersByTimeAsync(5_000);
    expectMetric(view, readTotal, "N/A");
    expectMetric(view, writeTotal, "N/A");
    mock.replace(measured({ io_read_bytes: 0, io_write_bytes: 0 }));
    await vi.advanceTimersByTimeAsync(5_000);
    expectMetric(view, readTotal, "0 B");
    expectMetric(view, writeTotal, "0 B");
  });
});

describe("MetricsView independent endpoint publication", () => {
  it("publishes fast resources across refreshes while six-second pipeline responses cannot overwrite newer data", async () => {
    const oldPipeline = deferred<Response>();
    const nextPipeline = deferred<Response>();
    const mock = mockMetrics(measured());
    mock.deferNextPipeline(oldPipeline.promise);
    window.setTimeout(() => oldPipeline.resolve(jsonResponse(measuredPipeline("old-provider"))), 6_000);
    const view = await mounted();

    expect(ebpfStatus(view)).toBe("Scrape available");
    expect(tickStatus(view)).toBe("Scrape available");
    expectMetric(view, readTotal, "1.5 KB");
    expectMetric(view, "Tick Duration", "4 ms");
    expect(view.getByTestId("pipeline-offline").textContent).toBe("Gateway loading");
    const firstSignals = mock.fetcher.mock.calls.map(([, init]) => init?.signal);

    mock.replace(measured({ io_read_bytes: 0, io_write_bytes: 0 }));
    mock.replaceTick(measuredTick({ tick_duration_ms: 0, psi_cpu_avg10: 0 }));
    mock.deferNextPipeline(nextPipeline.promise);
    await vi.advanceTimersByTimeAsync(5_000);
    window.setTimeout(() => nextPipeline.resolve(jsonResponse(measuredPipeline("superseded-provider"))), 6_000);

    for (const signal of firstSignals) expect(signal?.aborted).toBe(true);
    expectMetric(view, readTotal, "0 B");
    expectMetric(view, writeTotal, "0 B");
    expectMetric(view, "Tick Duration", "0 ms");
    expectMetric(view, "PSI CPU", "0.0%");
    expect(ebpfStatus(view)).toBe("Scrape available");
    expect(tickStatus(view)).toBe("Scrape available");
    expect(view.getByTestId("pipeline-offline").textContent).toBe("Gateway offline");

    await vi.advanceTimersByTimeAsync(1_000);
    expect(view.queryByText("old-provider")).toBeNull();
    expect(view.getByTestId("pipeline-offline").textContent).toBe("Gateway offline");
    mock.replacePipeline(measuredPipeline("current-provider"));
    await vi.advanceTimersByTimeAsync(4_000);
    expect(view.getByText("current-provider", { exact: true })).toBeDefined();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(view.getByText("current-provider", { exact: true })).toBeDefined();
    expect(view.queryByText("superseded-provider")).toBeNull();
    expectMetric(view, readTotal, "0 B");
    expectMetric(view, "Tick Duration", "0 ms");
  });

  it.each(["ebpf", "tick", "pipeline"] as const)("clears stale %s values at its deadline without clearing other endpoints", async (endpoint) => {
    const pending = deferred<Response>();
    const mock = mockMetrics(measured());
    mock.replacePipeline(measuredPipeline("current-provider"));
    const view = await mounted();
    expectMetric(view, readTotal, "1.5 KB");
    expectMetric(view, "Tick Duration", "4 ms");
    expect(view.getByText("current-provider", { exact: true })).toBeDefined();

    if (endpoint === "ebpf") mock.deferNextEbpf(pending.promise);
    if (endpoint === "tick") mock.deferNextTick(pending.promise);
    if (endpoint === "pipeline") mock.deferNextPipeline(pending.promise);
    await vi.advanceTimersByTimeAsync(5_000);
    const calls = mock.fetcher.mock.calls.filter(([path]) => path === `/api/metrics/${endpoint}`);
    const signal = calls[calls.length - 1][1]?.signal;
    expect(signal?.aborted).toBe(false);
    await vi.advanceTimersByTimeAsync(3_999);
    expect(signal?.aborted).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(signal?.aborted).toBe(true);

    const expectDeadlineState = () => {
      if (endpoint === "ebpf") {
        expectResourcesUnavailable(view);
        expect(ebpfStatus(view)).toBe("Offline");
      } else {
        expectMetric(view, readTotal, "1.5 KB");
        expect(ebpfStatus(view)).toBe("Scrape available");
      }
      if (endpoint === "tick") {
        expectTickUnavailable(view);
        expect(tickStatus(view)).toBe("Offline");
      } else {
        expectMetric(view, "Tick Duration", "4 ms");
        expect(tickStatus(view)).toBe("Scrape available");
      }
      if (endpoint === "pipeline") {
        expect(view.queryByText("current-provider")).toBeNull();
        expect(view.getByTestId("pipeline-offline").textContent).toBe("Gateway offline");
      } else {
        expect(view.getByText("current-provider", { exact: true })).toBeDefined();
      }
    };
    expectDeadlineState();
    const late = endpoint === "ebpf" ? measured() : endpoint === "tick" ? measuredTick() : measuredPipeline("late-provider");
    pending.resolve(jsonResponse(late));
    await vi.advanceTimersByTimeAsync(0);
    expectDeadlineState();
    expect(view.queryByText("late-provider")).toBeNull();

    await vi.advanceTimersByTimeAsync(1_000);
    expectMetric(view, readTotal, "1.5 KB");
    expectMetric(view, "Tick Duration", "4 ms");
    expect(view.getByText("current-provider", { exact: true })).toBeDefined();
  });

  it.each(["tick", "pipeline"] as const)("does not restore an older %s success after a newer failure", async (endpoint) => {
    const oldSuccess = deferred<Response>();
    const mock = mockMetrics(measured());
    if (endpoint === "tick") mock.deferNextTick(oldSuccess.promise);
    else mock.deferNextPipeline(oldSuccess.promise);
    const view = await mounted();
    expectMetric(view, readTotal, "1.5 KB");

    if (endpoint === "tick") mock.rejectTick("network");
    else mock.rejectPipeline("network");
    await vi.advanceTimersByTimeAsync(5_000);
    oldSuccess.resolve(jsonResponse(endpoint === "tick" ? measuredTick() : measuredPipeline("old-provider")));
    await vi.advanceTimersByTimeAsync(0);

    if (endpoint === "tick") {
      expectTickUnavailable(view);
      expect(tickStatus(view)).toBe("Offline");
    } else {
      expect(view.queryByText("old-provider")).toBeNull();
      expect(view.getByTestId("pipeline-offline").textContent).toBe("Gateway offline");
    }
    expectMetric(view, readTotal, "1.5 KB");
  });
});

describe("MetricsView nullable Tick / PSI", () => {
  it("renders durations and normalized PSI fractions with the correct scale", async () => {
    const mock = mockMetrics(measured());
    mock.replaceTick(measuredTick({ tick_duration_ms: 0.25 }));
    const view = await mounted();
    expect(tickStatus(view)).toBe("Scrape available");
    expectMetric(view, "Tick Duration", "0.25 ms");
    expectMetric(view, "Effective Rate", "1.00 s");
    expectMetric(view, "PSI CPU", "12.5%");
    expectMetric(view, "PSI Mem", "25.0%");
    expectMetric(view, "PSI IO", "50.0%");
  });

  it("preserves measured zero durations and pressures", async () => {
    const mock = mockMetrics(measured());
    mock.replaceTick(measuredTick({ tick_duration_ms: 0, tick_rate_effective_ms: 0,
      psi_cpu_avg10: 0, psi_mem_avg10: 0, psi_io_avg10: 0 }));
    const view = await mounted();
    expect(tickStatus(view)).toBe("Scrape available");
    expectMetric(view, "Tick Duration", "0 ms");
    expectMetric(view, "Effective Rate", "0 ms");
    expectMetric(view, "PSI CPU", "0.0%");
    expectMetric(view, "PSI Mem", "0.0%");
    expectMetric(view, "PSI IO", "0.0%");
  });

  it.each(["omitted", "null"] as const)("renders %s Tick / PSI fields as N/A despite scrape success", async (kind) => {
    const mock = mockMetrics(measured());
    const fields = kind === "null" ? { tick_duration_ms: null, tick_rate_effective_ms: null,
      psi_cpu_avg10: null, psi_mem_avg10: null, psi_io_avg10: null } : {};
    mock.replaceTick({ available: true, ...fields });
    const view = await mounted();
    expect(tickStatus(view)).toBe("Scrape available");
    expectTickUnavailable(view);
  });

  it("keeps individual Tick / PSI availability independent", async () => {
    const mock = mockMetrics(measured());
    mock.replaceTick(measuredTick({ tick_rate_effective_ms: null, psi_mem_avg10: null, psi_io_avg10: 1 }));
    const view = await mounted();
    expectMetric(view, "Tick Duration", "4 ms");
    expectMetric(view, "Effective Rate", "N/A");
    expectMetric(view, "PSI CPU", "12.5%");
    expectMetric(view, "PSI Mem", "N/A");
    expectMetric(view, "PSI IO", "100.0%");
  });

  it.each([
    { kind: "malformed", fields: { tick_duration_ms: "4", tick_rate_effective_ms: false,
      psi_cpu_avg10: "0", psi_mem_avg10: {}, psi_io_avg10: [] } },
    { kind: "negative or out of range", fields: { tick_duration_ms: -1, tick_rate_effective_ms: -100,
      psi_cpu_avg10: -0.001, psi_mem_avg10: 1.001, psi_io_avg10: 25 } },
  ])("renders $kind Tick / PSI fields as N/A", async ({ fields }) => {
    const mock = mockMetrics(measured());
    mock.replaceTick(measuredTick(fields));
    const view = await mounted();
    expect(tickStatus(view)).toBe("Scrape available");
    expectTickUnavailable(view);
  });

  it.each([
    { kind: "offline", payload: measuredTick({ available: false }) },
    { kind: "missing availability", payload: measuredTick({ available: undefined }) },
    { kind: "null availability", payload: measuredTick({ available: null }) },
    { kind: "null response", payload: null },
  ])("replaces previous Tick / PSI success after $kind refresh", async ({ payload }) => {
    const mock = mockMetrics(measured());
    const view = await mounted();
    expectMetric(view, "Tick Duration", "4 ms");
    expectMetric(view, "PSI CPU", "12.5%");
    mock.replaceTick(payload);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(tickStatus(view)).toBe("Offline");
    expectTickUnavailable(view);
    expectMetric(view, readTotal, "1.5 KB");
  });

  it.each(["http", "network"] as const)("clears stale Tick / PSI on %s rejection and recovers", async (failure) => {
    const mock = mockMetrics(measured());
    const view = await mounted();
    expectMetric(view, "PSI CPU", "12.5%");
    mock.rejectTick(failure);
    await vi.advanceTimersByTimeAsync(5_000);
    expectTickUnavailable(view);
    expect(tickStatus(view)).toBe("Offline");
    mock.replaceTick(measuredTick());
    await vi.advanceTimersByTimeAsync(5_000);
    expectMetric(view, "Tick Duration", "4 ms");
    expectMetric(view, "PSI CPU", "12.5%");
    expectMetric(view, "PSI Mem", "25.0%");
    expectMetric(view, "PSI IO", "50.0%");
  });
});
