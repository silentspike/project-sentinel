import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { SynthesisView, canonicalAgentId } from "../src/views/SynthesisView";

const RULE_NAMES = [
  "bio_bladder",
  "bio_hunger",
  "bio_energy",
  "bio_caffeine_low",
  "circadian_morning",
  "circadian_lunch",
  "physics_temp_high",
  "physics_noise_high",
  "routine_idle_alone",
  "routine_idle_with_presence",
];
const RULES = RULE_NAMES.map((name) => ({ name, enabled: true }));

const RESPONSES = [
  {
    request_id: "r1",
    request_class: "agent_runtime",
    provider: "synthesis",
    model: "sentinel-synth-v1",
    agent_id: "7", // numeric -> AGENT-07 (joins the drift alert below)
    agent_name: "Kai",
    content: "x",
    logged_at: "2026-06-26T10:00:00Z",
    decision: "synthesize",
    rule: "bio_hunger",
    fourth_wall: "clean",
  },
  {
    request_id: "r2",
    request_class: "agent_runtime",
    provider: "claude-code",
    model: "m",
    agent_id: "3",
    agent_name: "Tom",
    content: "y",
    logged_at: "2026-06-26T10:00:01Z",
    decision: "forward",
    fourth_wall: "regenerated",
  },
];

const ALERTS = [
  { agent_id: "AGENT-07", alert_type: "drift", severity: "warning", score: 0.4, details: "d", timestamp_ms: 1 },
];

let posted: { url: string; body: string } | null = null;

function stubFetch() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const u = String(url);
      if (init?.method === "POST" && u.includes("/api/control/synthesis-rules/")) {
        posted = { url: u, body: String(init.body) };
        return { ok: true, json: async () => ({ name: "bio_hunger", enabled: false }) };
      }
      if (u.includes("/api/control/synthesis-rules")) return { ok: true, json: async () => RULES };
      if (u.includes("/api/control/traffic-stats"))
        return { ok: true, json: async () => ({ synthesis_enabled: true }) };
      if (u.includes("/api/control/traffic-responses")) return { ok: true, json: async () => RESPONSES };
      if (u.includes("/api/control/judge-alerts")) return { ok: true, json: async () => ALERTS };
      if (u.includes("/api/control/config")) return { ok: true, json: async () => ({}) };
      return { ok: false, json: async () => ({ error: "unknown" }) };
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  posted = null;
});

describe("canonicalAgentId", () => {
  it("normalizes numeric to AGENT-NN, keeps prefixed, rejects junk", () => {
    expect(canonicalAgentId("8")).toBe("AGENT-08");
    expect(canonicalAgentId("AGENT-07")).toBe("AGENT-07");
    expect(canonicalAgentId("")).toBeNull();
    expect(canonicalAgentId(undefined)).toBeNull();
    expect(canonicalAgentId("not-numeric")).toBeNull();
  });
});

describe("SynthesisView", () => {
  it("lists 10 rule toggles and posts on toggle", async () => {
    stubFetch();
    const { getByTestId } = render(SynthesisView);
    expect(getByTestId("view-synthesis")).toBeTruthy();
    await waitFor(() => expect(getByTestId("synthesis-rule-bio_hunger")).toBeTruthy());
    expect(RULES.length).toBe(10);
    for (const r of RULES) expect(getByTestId(`synthesis-rule-${r.name}`)).toBeTruthy();

    fireEvent.click(getByTestId("synthesis-rule-bio_hunger"));
    await waitFor(() => expect(posted).not.toBeNull());
    expect(posted!.url).toContain("/api/control/synthesis-rules/bio_hunger");
    expect(posted!.body).toContain('"enabled":false');
  });

  it("renders the inspector with decision/fourth_wall + a joined judge row", async () => {
    stubFetch();
    const { getAllByTestId } = render(SynthesisView);
    await waitFor(() => expect(getAllByTestId("inspector-row").length).toBeGreaterThan(0));

    const decisions = getAllByTestId("inspector-decision").map((e) => e.textContent ?? "");
    expect(decisions.some((d) => d.includes("synthesize") && d.includes("bio_hunger"))).toBe(true);
    expect(decisions.some((d) => d.includes("forward"))).toBe(true);

    const fourthWall = getAllByTestId("inspector-fourthwall").map((e) => e.textContent ?? "");
    expect(fourthWall.some((f) => f.includes("regenerated"))).toBe(true);

    // Judge join: numeric agent_id "7" -> AGENT-07 matches the drift alert (agent-level).
    const judges = getAllByTestId("inspector-judge").map((e) => e.textContent ?? "");
    expect(judges.some((j) => j.includes("drift"))).toBe(true);
  });
});
