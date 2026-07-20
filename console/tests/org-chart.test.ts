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
  tier: 1 | 2 | 3 | null,
  nano_runtime: string | null = null,
  reports_to: string | null = null,
  direct_reports: string[] = [],
): AgentConfig {
  return {
    identity: { id, name, role, department, tier, shift_set: 1, kpis: [], reports_to, direct_reports },
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
  mkAgent(1, "Thomas", "Leitung", "CEO", 1, "opus", null, ["2", "3"]),
  mkAgent(2, "Lisa", "Design", "Designer", 2, "sonnet", "Thomas"),
  mkAgent(3, "Max", "Design", "Designer", null, null, "Thomas"), // legacy omission -> "—"
  mkAgent(4, "Anna", "Dev", "Backend", 3, "haiku", "Thomas"),
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
    expect(tiers.some((t) => t.includes("hierarchy tier: 1"))).toBe(true);
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

  it("exposes agent nodes as keyboard-focusable buttons", async () => {
    stubFetch();
    const { getAllByTestId } = render(OrgChartView);
    await waitFor(() => expect(getAllByTestId("org-agent-node").length).toBe(4));
    const lisa = getAllByTestId("org-agent-node")[0] as HTMLButtonElement;
    expect(lisa.tagName).toBe("BUTTON");
    expect(lisa.type).toBe("button");
    expect(lisa.getAttribute("aria-label")).toContain("hierarchy tier 2");
    lisa.focus();
    expect(document.activeElement).toBe(lisa);
  });

  it("distinguishes loading, empty, and server error states", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: false, status: 503, statusText: "Unavailable", json: async () => ({ error: "projection offline" }) })),
    );
    const { getByTestId, findByTestId } = render(OrgChartView);
    expect(getByTestId("org-loading")).toBeTruthy();
    expect((await findByTestId("org-error")).textContent).toContain("projection offline");
  });

  it("renders a distinct empty state after a successful empty response", async () => {
    stubFetch([]);
    const { findByTestId, queryByTestId } = render(OrgChartView);
    expect(await findByTestId("org-empty")).toBeTruthy();
    expect(queryByTestId("org-error")).toBeNull();
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
