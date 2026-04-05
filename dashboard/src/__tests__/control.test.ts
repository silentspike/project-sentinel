import { describe, it, expect, afterAll, beforeEach } from "bun:test";
import { Database } from "bun:sqlite";
import { app } from "../index";
import { setDatabases } from "../db";

// Mock fetch fuer Cortex Gateway Proxy-Tests
const originalFetch = globalThis.fetch;
const originalDashboardApiKey = process.env.SENTINEL_DASHBOARD_API_KEY;
const originalOperatorApiKey = process.env.SENTINEL_OPERATOR_API_KEY;
const originalOperatorApiUrl = process.env.SENTINEL_OPERATOR_API_URL;

function mockFetch(handler: (url: string, init?: RequestInit) => Response | Promise<Response>) {
  globalThis.fetch = handler as typeof fetch;
}

function restoreFetch() {
  globalThis.fetch = originalFetch;
}

describe("Control Routes", () => {
  beforeEach(() => {
    restoreFetch();
    process.env.SENTINEL_DASHBOARD_API_KEY = originalDashboardApiKey || "";
    process.env.SENTINEL_OPERATOR_API_KEY = originalOperatorApiKey || "";
    process.env.SENTINEL_OPERATOR_API_URL = originalOperatorApiUrl || "";
  });

  afterAll(() => {
    restoreFetch();
    process.env.SENTINEL_DASHBOARD_API_KEY = originalDashboardApiKey || "";
    process.env.SENTINEL_OPERATOR_API_KEY = originalOperatorApiKey || "";
    process.env.SENTINEL_OPERATOR_API_URL = originalOperatorApiUrl || "";
  });

  describe("GET /api/control/config", () => {
    it("proxies to Cortex Gateway and returns config", async () => {
      const fakeConfig = {
        primary_provider: "claude-code",
        temperature: 0.7,
        max_tokens: 4096,
        rate_limit_rps: 0,
        agent_overrides: {},
      };

      mockFetch(async (url: string) => {
        if (url.includes("/control/config")) {
          return new Response(JSON.stringify(fakeConfig), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          });
        }
        return new Response("not found", { status: 404 });
      });

      const res = await app.request("/api/control/config");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.primary_provider).toBe("claude-code");
      expect(body.temperature).toBe(0.7);
    });

    it("returns 502 when Cortex Gateway unreachable", async () => {
      mockFetch(async () => {
        throw new Error("Connection refused");
      });

      const res = await app.request("/api/control/config");
      expect(res.status).toBe(502);
      const body = await res.json();
      expect(body.error).toContain("nicht erreichbar");
    });
  });

  describe("GET /api/control/status", () => {
    it("returns aggregated status", async () => {
      const fakeConfig = {
        primary_provider: "claude-code",
        temperature: 0.7,
        rate_limit_rps: 10,
      };
      const fakeHealth = { status: "ok" };

      mockFetch(async (url: string) => {
        if (url.includes("/control/config")) {
          return new Response(JSON.stringify(fakeConfig), { status: 200 });
        }
        if (url.includes("/health")) {
          return new Response(JSON.stringify(fakeHealth), { status: 200 });
        }
        return new Response("not found", { status: 404 });
      });

      const res = await app.request("/api/control/status");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.connected).toBe(true);
      expect(body.paused).toBe(false);
      expect(body.config).toBeTruthy();
      expect(body.health).toBeTruthy();
    });

    it("detects paused state (rate_limit_rps=0)", async () => {
      const fakeConfig = {
        primary_provider: "claude-code",
        rate_limit_rps: 0,
      };

      mockFetch(async (url: string) => {
        if (url.includes("/control/config")) {
          return new Response(JSON.stringify(fakeConfig), { status: 200 });
        }
        if (url.includes("/health")) {
          return new Response(JSON.stringify({ status: "ok" }), { status: 200 });
        }
        return new Response("not found", { status: 404 });
      });

      const res = await app.request("/api/control/status");
      const body = await res.json();
      expect(body.paused).toBe(true);
    });
  });

  describe("POST /api/control/pause (auth required)", () => {
    it("returns 403 when no API key configured", async () => {
      // SENTINEL_DASHBOARD_API_KEY not set → 403
      process.env.SENTINEL_DASHBOARD_API_KEY = "";
      const res = await app.request("/api/control/pause", {
        method: "POST",
      });
      expect(res.status).toBe(403);
    });

    it("returns 401 when no Authorization header", async () => {
      process.env.SENTINEL_DASHBOARD_API_KEY = "test-key-123";
      const res = await app.request("/api/control/pause", {
        method: "POST",
      });
      expect(res.status).toBe(401);
    });
  });

  describe("PATCH /api/control/config (auth required)", () => {
    it("returns 403 without auth", async () => {
      const res = await app.request("/api/control/config", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ temperature: 1.0 }),
      });
      expect(res.status).toBe(403);
    });
  });

  describe("POST /api/control/provider (auth required)", () => {
    it("returns 403 without auth", async () => {
      const res = await app.request("/api/control/provider", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ provider: "ollama" }),
      });
      expect(res.status).toBe(403);
    });
  });

  describe("POST /api/control/chaos", () => {
    it("proxies chaos trigger to the local operator API", async () => {
      process.env.SENTINEL_DASHBOARD_API_KEY = "dash-key";
      process.env.SENTINEL_OPERATOR_API_KEY = "operator-key";

      mockFetch(async (url: string, init?: RequestInit) => {
        expect(url).toBe("http://127.0.0.1:8084/operator/chaos");
        expect(init?.method).toBe("POST");
        const headers = new Headers(init?.headers);
        expect(headers.get("x-sentinel-operator-key")).toBe("operator-key");
        expect(headers.get("content-type")).toContain("application/json");
        expect(JSON.parse(String(init?.body))).toEqual({
          room_id: "kueche",
          chaos_type: "AirConBroken",
          duration_ticks: 45,
        });
        return new Response(
          JSON.stringify({
            accepted: true,
            event_id: "evt-1",
            room_id: "kueche",
            chaos_type: "AirConBroken",
          }),
          {
            status: 202,
            headers: { "Content-Type": "application/json" },
          },
        );
      });

      const res = await app.request("/api/control/chaos", {
        method: "POST",
        headers: {
          "Authorization": "Bearer dash-key",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          room_id: "kueche",
          chaos_type: "AirConBroken",
          duration_ticks: 45,
        }),
      });

      expect(res.status).toBe(202);
      const body = await res.json();
      expect(body.accepted).toBe(true);
      expect(body.room_id).toBe("kueche");
    });

    it("returns 502 when the operator API is unreachable", async () => {
      process.env.SENTINEL_DASHBOARD_API_KEY = "dash-key";

      mockFetch(async () => {
        throw new Error("connect ECONNREFUSED 127.0.0.1:8084");
      });

      const res = await app.request("/api/control/chaos", {
        method: "POST",
        headers: {
          "Authorization": "Bearer dash-key",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          room_id: "kueche",
          chaos_type: "PrinterBroken",
        }),
      });

      expect(res.status).toBe(502);
      const body = await res.json();
      expect(body.error).toContain("Operator-API");
    });
  });

  describe("POST /api/control/stimulus", () => {
    it("proxies room stimulus trigger to the local operator API", async () => {
      process.env.SENTINEL_DASHBOARD_API_KEY = "dash-key";
      process.env.SENTINEL_OPERATOR_API_KEY = "operator-key";

      mockFetch(async (url: string, init?: RequestInit) => {
        expect(url).toBe("http://127.0.0.1:8084/operator/stimulus");
        expect(init?.method).toBe("POST");
        const headers = new Headers(init?.headers);
        expect(headers.get("x-sentinel-operator-key")).toBe("operator-key");
        expect(headers.get("content-type")).toContain("application/json");
        expect(JSON.parse(String(init?.body))).toEqual({
          room_id: "kueche",
          stimulus_type: "co2",
          delta: 900,
          duration_ticks: 90,
        });
        return new Response(
          JSON.stringify({
            accepted: true,
            event_id: "evt-stim-1",
            room_id: "kueche",
            stimulus_type: "co2",
            delta: 900,
          }),
          {
            status: 202,
            headers: { "Content-Type": "application/json" },
          },
        );
      });

      const res = await app.request("/api/control/stimulus", {
        method: "POST",
        headers: {
          "Authorization": "Bearer dash-key",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          room_id: "kueche",
          stimulus_type: "co2",
          delta: 900,
          duration_ticks: 90,
        }),
      });

      expect(res.status).toBe(202);
      const body = await res.json();
      expect(body.accepted).toBe(true);
      expect(body.stimulus_type).toBe("co2");
    });
  });

  describe("Platform control endpoints", () => {
    it("proxies GET /api/control/platform-state to the local operator API", async () => {
      process.env.SENTINEL_OPERATOR_API_KEY = "operator-key";

      mockFetch(async (url: string, init?: RequestInit) => {
        expect(url).toBe("http://127.0.0.1:8084/operator/platform-state");
        const headers = new Headers(init?.headers);
        expect(headers.get("x-sentinel-operator-key")).toBe("operator-key");
        return new Response(
          JSON.stringify({
            current_tick: 123,
            llm_enabled: true,
            llm_analysis_interval_secs: 30,
            llm_retry_delay_secs: 15,
            stall_recent_activity_grace_ticks: 120,
            last_analysis_tick: 120,
            last_analysis_trigger: "manual",
            last_scheduled_analysis_tick: 90,
            unresolved_counts: { projection_lag: 2 },
            threshold_overrides: { memory_pressure_threshold: 0.8 },
            resource_profiles: { "AGENT-01": "normal" },
            agents: [],
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      });

      const res = await app.request("/api/control/platform-state");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.current_tick).toBe(123);
      expect(body.llm_enabled).toBe(true);
    });

    it("returns recent platform analyses from the event store", async () => {
      const projDb = new Database(":memory:");
      const esDb = new Database(":memory:");
      esDb.exec(`
        CREATE TABLE events (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          event_id TEXT NOT NULL UNIQUE,
          event_type TEXT NOT NULL,
          aggregate_id TEXT NOT NULL,
          payload TEXT NOT NULL,
          correlation_id TEXT NOT NULL,
          causation_id TEXT,
          tick INTEGER NOT NULL,
          timestamp_ms INTEGER NOT NULL
        );
      `);
      esDb
        .prepare(`
          INSERT INTO events (event_id, event_type, aggregate_id, payload, correlation_id, causation_id, tick, timestamp_ms)
          VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        `)
        .run(
          "evt-platform-1",
          "platform_analysis",
          "AGENT-03",
          JSON.stringify({
            trigger: "manual",
            severity: "warning",
            summary: "Projection lag analysis",
            recommendation: "Watch projection",
            suggested_action: "adjust_threshold",
            target: "AGENT-03",
            provider: "claude-code",
            model: "gpt-test",
            unresolved_keys: ["projection_lag"],
            parameters: { key: "max_projection_lag", value: 120 },
          }),
          "corr-platform",
          null,
          345,
          1_700_000_000_000,
        );
      setDatabases(projDb, esDb);

      try {
        const res = await app.request("/api/control/platform-analyses");
        expect(res.status).toBe(200);
        const body = await res.json();
        expect(body).toHaveLength(1);
        expect(body[0].event_id).toBe("evt-platform-1");
        expect(body[0].suggested_action).toBe("adjust_threshold");
      } finally {
        projDb.close();
        esDb.close();
      }
    });

    it("proxies POST /api/control/platform-analyze with dashboard auth", async () => {
      process.env.SENTINEL_DASHBOARD_API_KEY = "dash-key";
      process.env.SENTINEL_OPERATOR_API_KEY = "operator-key";

      mockFetch(async (url: string, init?: RequestInit) => {
        expect(url).toBe("http://127.0.0.1:8084/operator/platform-analyze");
        expect(init?.method).toBe("POST");
        const headers = new Headers(init?.headers);
        expect(headers.get("x-sentinel-operator-key")).toBe("operator-key");
        expect(headers.get("content-type")).toContain("application/json");
        expect(String(init?.body)).toBe("{}");
        return new Response(
          JSON.stringify({ accepted: true, message: "Platform-Analyse eingeplant" }),
          { status: 202, headers: { "Content-Type": "application/json" } },
        );
      });

      const res = await app.request("/api/control/platform-analyze", {
        method: "POST",
        headers: {
          "Authorization": "Bearer dash-key",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({}),
      });

      expect(res.status).toBe(202);
      const body = await res.json();
      expect(body.accepted).toBe(true);
    });
  });
});
