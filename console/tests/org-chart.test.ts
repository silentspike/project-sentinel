import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import type { AgentConfig } from "../src/api";
import { OrgChartView, buildOrgTree } from "../src/views/OrgChartView";
import { AgentEditorView } from "../src/views/AgentEditorView";
import { selectedAgentId, setSelectedAgentId } from "../src/state/selection";

// Mock openPanel so the click-navigation is observable without driving the real tiling layout.
vi.mock("../src/tiling/engine", async (importActual) => {
  const actual = await importActual<typeof import("../src/tiling/engine")>();
  return { ...actual, openPanel: vi.fn() };
});
import { openPanel } from "../src/tiling/engine";

function mkAgent(
  id: number,
  name: string,
  department: string,
  role: string,
  nano_runtime: string | null = null,
  reports_to: string | null = null,
  direct_reports: string[] = [],
): AgentConfig {
  return {
    identity: { id, name, role, department, shift_set: 1, kpis: [], reports_to, direct_reports },
    personality: {
      openness: 0.5,
      conscientiousness: 0.5,
      extraversion: 0.5,
      agreeableness: 0.5,
      neuroticism: 0.3,
      caffeine_tolerance: 0.5,
      morning_person: true,
    },
    preferences: { favorite_room: "x", coffee_preference: "x", lunch_time: "12:00" },
    background: { bio: "", quirks: [] },
    runtime: { nano_runtime },
    capabilities: { tools: [], sandbox_allowed_paths: [] },
  };
}

const AGENTS: AgentConfig[] = [
  mkAgent(1, "Thomas", "Leitung", "CEO", "opus", null, ["2", "3"]),
  mkAgent(2, "Lisa", "Design", "Designer", "sonnet", "Thomas"),
  mkAgent(3, "Max", "Design", "Designer", null, "Thomas"), // null tier -> "—"
  mkAgent(4, "Anna", "Dev", "Backend", "haiku", "Thomas"),
];

function stubFetch(agents: AgentConfig[] = AGENTS) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      if (String(url).includes("/api/config/agents")) return { ok: true, json: async () => agents };
      return { ok: false, json: async () => [] };
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.mocked(openPanel).mockClear();
  setSelectedAgentId(null);
});

describe("buildOrgTree", () => {
  it("groups department -> role -> agent, stably sorted", () => {
    const tree = buildOrgTree(AGENTS);
    expect(tree.map((d) => d.department)).toEqual(["Design", "Dev", "Leitung"]);
    const design = tree.find((d) => d.department === "Design")!;
    expect(design.count).toBe(2);
    expect(design.roles[0].role).toBe("Designer");
    expect(design.roles[0].agents.map((a) => a.identity.name)).toEqual(["Lisa", "Max"]);
  });
});

describe("OrgChartView (#424)", () => {
  it("renders the hierarchy with tier per node (null -> '—')", async () => {
    stubFetch();
    const { getByTestId, getAllByTestId } = render(OrgChartView);
    expect(getByTestId("view-org-chart")).toBeTruthy();
    await waitFor(() => expect(getAllByTestId("org-agent-node").length).toBe(4));
    expect(getAllByTestId("org-dept").length).toBe(3);
    const tiers = getAllByTestId("org-tier").map((e) => e.textContent ?? "");
    expect(tiers.some((t) => t.includes("opus"))).toBe(true);
    expect(tiers.some((t) => t.includes("—"))).toBe(true); // Max's null tier
  });

  it("click on an agent sets selectedAgentId + opens the agent-editor (AC-3)", async () => {
    stubFetch();
    const { getAllByTestId } = render(OrgChartView);
    await waitFor(() => expect(getAllByTestId("org-agent-node").length).toBe(4));
    // DOM order (dept Design->Dev->Leitung, agents by name): Lisa(2), Max(3), Anna(4), Thomas(1).
    fireEvent.click(getAllByTestId("org-agent-node")[0]); // Lisa, id 2
    expect(selectedAgentId()).toBe(2);
    expect(openPanel).toHaveBeenCalledWith("agent-editor");
  });
});

describe("AgentEditorView consume-and-clear (#424 AC-3)", () => {
  it("pre-selects the org-chart-requested agent then clears the shared signal", async () => {
    stubFetch();
    setSelectedAgentId(4); // org-chart requested agent 4 (Anna)
    const { getByTestId } = render(AgentEditorView);
    const select = getByTestId("ae-select") as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe("4")); // editor pre-selected agent 4
    expect(selectedAgentId()).toBe(null); // consumed-and-cleared: no stale re-apply on re-open
  });
});
