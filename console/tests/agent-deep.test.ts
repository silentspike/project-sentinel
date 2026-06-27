import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import type {
  FsListing,
  FsFileRead,
  EventsResponse,
  AgentLifecycleResult,
} from "../src/api";
import { AgentDeepView } from "../src/views/AgentDeepView";
import { AgentsView } from "../src/views/AgentsView";
import { selectedAgentId, setSelectedAgentId } from "../src/state/selection";
import { ingestFrame } from "../src/stores/console";

// Mock openPanel so the AgentsView click-navigation is observable without driving the real layout.
vi.mock("../src/tiling/engine", async (importActual) => {
  const actual = await importActual<typeof import("../src/tiling/engine")>();
  return { ...actual, openPanel: vi.fn() };
});
import { openPanel } from "../src/tiling/engine";

const LISTING: FsListing = {
  accepted: true,
  agent_id: 7,
  aggregate_id: "AGENT-07",
  inode: 1,
  entries: [
    { name: "src", inode: 2, kind: "dir", size: 0, mode: 0o755, mtime: 0, hash: "", refcount: 0 },
    {
      name: "hello.txt",
      inode: 3,
      kind: "file",
      size: 11,
      mode: 0o644,
      mtime: 0,
      hash: "ab".repeat(32),
      refcount: 2,
    },
  ],
  dedup_ratio_percent: 42.5,
  cas_blob_count: 1,
  dedup_savings_bytes: 11,
};

const FILE: FsFileRead = {
  accepted: true,
  agent_id: 7,
  aggregate_id: "AGENT-07",
  inode: 3,
  size: 11,
  returned_bytes: 11,
  truncated: false,
  hash: "ab".repeat(32),
  refcount: 2,
  encoding: "utf8",
  content: "hello world",
};

const EVENTS: EventsResponse = {
  events: [
    { id: 3, event_id: "e3", event_type: "agent_action_received", aggregate_id: "AGENT-07", payload: JSON.stringify({ action_type: "write_file" }), tick: 30, timestamp_ms: 3000 },
    { id: 2, event_id: "e2", event_type: "agent_action_received", aggregate_id: "AGENT-07", payload: JSON.stringify({ action_type: "read_file" }), tick: 20, timestamp_ms: 2000 },
    { id: 1, event_id: "e1", event_type: "transit_started", aggregate_id: "AGENT-07", payload: "{}", tick: 10, timestamp_ms: 1000 },
  ],
  total: 3,
  limit: 200,
  offset: 0,
  events_db: "ok",
};

const LIFECYCLE: AgentLifecycleResult = {
  accepted: true,
  agent_id: 7,
  aggregate_id: "AGENT-07",
  action: "pause",
  new_status: "suspended",
  affected_pids: 2,
  outcome: "ok",
  note: "paused (SIGSTOP; ECS-Entity + Memory bleiben)",
};

function jsonOk(body: unknown) {
  return { ok: true, json: async () => body };
}

function stubFetch(over: Partial<{ listing: FsListing; file: FsFileRead; events: EventsResponse; lifecycle: AgentLifecycleResult }> = {}) {
  const fn = vi.fn(async (url: string) => {
    const u = String(url);
    if (u.includes("/fs/read")) return jsonOk(over.file ?? FILE);
    if (u.includes("/fs")) return jsonOk(over.listing ?? LISTING);
    if (u.includes("/api/events")) return jsonOk(over.events ?? EVENTS);
    if (u.includes("/stop") || u.includes("/start") || u.includes("/remove")) return jsonOk(over.lifecycle ?? LIFECYCLE);
    if (u.includes("/api/metrics/ebpf")) return jsonOk({ available: true, mode: "live", stalled_count: 0, stalled_agents: [] });
    return { ok: false, json: async () => ({ error: "unmatched" }) };
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

function seedAgent(status: string) {
  ingestFrame("agent_live", {
    agents: [
      { agent_id: 7, name: "Test Agent", role: "Backend", current_room: "buero", status, mood: "ok", hunger: 0.2, energy: 0.8 },
    ],
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.mocked(openPanel).mockClear();
  setSelectedAgentId(null);
  ingestFrame("agent_live", { agents: [] });
});

describe("AgentDeepView (#428)", () => {
  it("consumes selectedAgentId on mount (consume-and-clear)", async () => {
    stubFetch();
    setSelectedAgentId(7);
    const { getByTestId } = render(AgentDeepView);
    expect(getByTestId("view-agent-deep")).toBeTruthy();
    expect(selectedAgentId()).toBe(null); // consumed
    await waitFor(() => expect(getByTestId("deep-agent-name")).toBeTruthy());
  });

  it("renders the FS listing read-only with dedup/size and opens a file", async () => {
    stubFetch();
    setSelectedAgentId(7);
    const { getAllByTestId, getByTestId, queryByTestId } = render(AgentDeepView);
    await waitFor(() => expect(getAllByTestId("fs-entry").length).toBe(2));
    expect(getByTestId("fs-dedup").textContent).toContain("42.5%");
    expect(getAllByTestId("fs-entry-refcount").map((e) => e.textContent)).toContain("×2");
    expect(queryByTestId("fs-file-content")).toBeNull();
    // entry order: [src (dir), hello.txt (file)] -> click the file
    fireEvent.click(getAllByTestId("fs-entry")[1]);
    await waitFor(() => expect(getByTestId("fs-file-content").textContent).toContain("hello world"));
  });

  it("navigates into a directory (breadcrumb grows)", async () => {
    stubFetch();
    setSelectedAgentId(7);
    const { getAllByTestId } = render(AgentDeepView);
    await waitFor(() => expect(getAllByTestId("fs-entry").length).toBe(2));
    expect(getAllByTestId("fs-crumb").length).toBe(1);
    fireEvent.click(getAllByTestId("fs-entry")[0]); // src (dir)
    await waitFor(() => expect(getAllByTestId("fs-crumb").length).toBe(2));
  });

  it("renders sparkline + tool donut from real event data", async () => {
    stubFetch();
    setSelectedAgentId(7);
    const { getByTestId, getAllByTestId } = render(AgentDeepView);
    await waitFor(() => expect(getAllByTestId("deep-donut-legend").length).toBeGreaterThan(0));
    // sparkline has points (events present -> non-empty polyline)
    const spark = getByTestId("deep-sparkline");
    expect(spark.querySelector("polyline")?.getAttribute("points")?.length ?? 0).toBeGreaterThan(0);
    // donut groups by action_type (write_file, read_file) + event_type (transit_started)
    const labels = getAllByTestId("deep-donut-legend").map((e) => e.textContent ?? "");
    expect(labels.some((l) => l.includes("write_file"))).toBe(true);
    expect(labels.some((l) => l.includes("transit_started"))).toBe(true);
  });

  it("Stop calls the stop endpoint and shows the result", async () => {
    const fetchFn = stubFetch();
    setSelectedAgentId(7);
    const { getByTestId } = render(AgentDeepView);
    await waitFor(() => expect(getByTestId("deep-stop")).toBeTruthy());
    fireEvent.click(getByTestId("deep-stop"));
    await waitFor(() => expect(getByTestId("deep-lifecycle-msg").textContent).toContain("suspended"));
    expect(fetchFn.mock.calls.some((c) => String(c[0]).endsWith("/api/control/agent/7/stop"))).toBe(true);
  });

  it("destructive remove requires a confirmation before calling /remove", async () => {
    const fetchFn = stubFetch();
    setSelectedAgentId(7);
    const { getByTestId, queryByTestId } = render(AgentDeepView);
    await waitFor(() => expect(getByTestId("deep-remove")).toBeTruthy());
    fireEvent.click(getByTestId("deep-remove"));
    // first click only reveals the confirm button — no /remove call yet
    expect(queryByTestId("deep-remove-confirm")).toBeTruthy();
    expect(fetchFn.mock.calls.some((c) => String(c[0]).includes("/remove"))).toBe(false);
    fireEvent.click(getByTestId("deep-remove-confirm"));
    await waitFor(() =>
      expect(fetchFn.mock.calls.some((c) => String(c[0]).endsWith("/api/control/agent/7/remove"))).toBe(true),
    );
  });

  it("falls back to the #deep=<id> URL hash when no agent was selected", async () => {
    stubFetch();
    setSelectedAgentId(null);
    window.location.hash = "#deep=8";
    const { getByTestId } = render(AgentDeepView);
    await waitFor(() => expect(getByTestId("deep-agent-name").textContent).toContain("AGENT-08"));
    window.location.hash = "";
  });

  it("renders the canonical 'suspended' status as 'Pausiert'", async () => {
    stubFetch();
    seedAgent("suspended");
    setSelectedAgentId(7);
    const { getByTestId } = render(AgentDeepView);
    await waitFor(() => expect(getByTestId("deep-status").textContent).toBe("Pausiert"));
  });
});

describe("AgentsView -> Deep View entry (#428 AC-2)", () => {
  it("clicking an agent card sets selectedAgentId + opens the agent-deep panel", async () => {
    stubFetch();
    seedAgent("active");
    const { getAllByTestId } = render(AgentsView);
    await waitFor(() => expect(getAllByTestId("agent-card").length).toBe(1));
    fireEvent.click(getAllByTestId("agent-card")[0]);
    expect(selectedAgentId()).toBe(7);
    expect(openPanel).toHaveBeenCalledWith("agent-deep");
  });
});
