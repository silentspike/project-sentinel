import { describe, it, expect, vi, afterEach } from "vitest";
import { render, waitFor } from "@solidjs/testing-library";
import { CostView } from "../src/views/CostView";

const ROW = (key: string, cr: number, cc: number, cost: number, calls: number) => ({
  key,
  input_tokens: 1300,
  output_tokens: 600,
  cache_read: cr,
  cache_creation: cc,
  cost_usd: cost,
  call_count: calls,
});

const COST = {
  by_agent: [ROW("AGENT-08", 200, 100, 0.025, 2), ROW("AGENT-09", 0, 0, 0.0001, 1)],
  by_tier: [ROW("high", 200, 100, 0.025, 2), ROW("low", 0, 0, 0.0001, 1)],
  time_series: [ROW("0", 200, 100, 0.025, 2), ROW("60000", 0, 0, 0.0001, 1)],
};

function stubFetch() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      const u = String(url);
      if (u.includes("/api/cost")) return { ok: true, json: async () => COST };
      return { ok: false, json: async () => ({ error: "unknown" }) };
    }),
  );
}

afterEach(() => vi.unstubAllGlobals());

describe("CostView", () => {
  it("renders per-agent + per-tier rows with cache-aware columns and a sparkline", async () => {
    stubFetch();
    const { getByTestId, getAllByTestId } = render(CostView);
    expect(getByTestId("view-cost")).toBeTruthy();
    await waitFor(() => expect(getAllByTestId("cost-agent-row").length).toBe(2));

    expect(getByTestId("cost-agent-table")).toBeTruthy();
    expect(getByTestId("cost-tier-table")).toBeTruthy();
    expect(getAllByTestId("cost-tier-row").length).toBe(2);

    // Cache-aware columns: AGENT-08 has cache_read 200 / cache_creation 100.
    const cacheReads = getAllByTestId("cost-cache-read").map((e) => e.textContent ?? "");
    expect(cacheReads.some((t) => t.includes("200"))).toBe(true);
    const cacheCreations = getAllByTestId("cost-cache-creation").map((e) => e.textContent ?? "");
    expect(cacheCreations.some((t) => t.includes("100"))).toBe(true);

    // Sparkline rendered with a non-empty polyline.
    const spark = getByTestId("cost-sparkline");
    expect(spark).toBeTruthy();
    const poly = spark.querySelector("polyline");
    expect((poly?.getAttribute("points") ?? "").length).toBeGreaterThan(0);
  });
});
