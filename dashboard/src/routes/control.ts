// Control Plane Proxy: Dashboard → Cortex Gateway :8081
// Proxied alle Control-Requests an den Cortex Gateway Control Plane Port.
// "Pause" = rate_limit_rps auf 0 setzen, "Resume" = vorherigen Wert restaurieren.

import { Hono } from "hono";
import { requireAuth } from "../middleware/auth";
import { getRecentPlatformAnalyses, resetCaches } from "../db";
import { resetWatermarks, broadcast } from "../ws";

export const controlRoutes = new Hono();

// Auth-Middleware auf alle Write-Endpoints (POST/PATCH/DELETE)
controlRoutes.use("/control/*", async (c, next) => {
  const method = c.req.method;
  if (method === "POST" || method === "PATCH" || method === "DELETE") {
    return requireAuth(c, next);
  }
  await next();
});

const DEFAULT_CORTEX_CONTROL_URL = "http://localhost:8081";
const DEFAULT_OPERATOR_API_URL = "http://127.0.0.1:8084";

// Timeout fuer Proxy-Requests zum Cortex Gateway (ms).
const PROXY_TIMEOUT_MS = 5000;

// ── Helpers ──────────────────────────────────────────

function getCortexControlUrl(): string {
  return process.env.CORTEX_GATEWAY_URL || DEFAULT_CORTEX_CONTROL_URL;
}

function getOperatorApiUrl(): string {
  return process.env.SENTINEL_OPERATOR_API_URL || DEFAULT_OPERATOR_API_URL;
}

function getOperatorHeaders(): Record<string, string> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
  };
  const operatorKey = process.env.SENTINEL_OPERATOR_API_KEY || "";
  if (operatorKey) {
    headers["x-sentinel-operator-key"] = operatorKey;
  }
  return headers;
}

async function proxyGet(
  baseUrl: string,
  path: string,
  headers: Record<string, string> = {},
): Promise<Response> {
  const resp = await fetch(`${baseUrl}${path}`, {
    headers,
    signal: AbortSignal.timeout(PROXY_TIMEOUT_MS),
  });
  const body = await resp.text();
  return new Response(body, {
    status: resp.status,
    headers: { "Content-Type": "application/json" },
  });
}

async function proxyJson(
  baseUrl: string,
  method: string,
  path: string,
  body: unknown,
  headers: Record<string, string> = { "Content-Type": "application/json" },
): Promise<Response> {
  const resp = await fetch(`${baseUrl}${path}`, {
    method,
    headers,
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(PROXY_TIMEOUT_MS),
  });
  const text = await resp.text();
  return new Response(text, {
    status: resp.status,
    headers: { "Content-Type": "application/json" },
  });
}

// ── GET /api/control/config — Proxy zu :8081/control/config ──

controlRoutes.get("/control/config", async (c) => {
  try {
    return await proxyGet(getCortexControlUrl(), "/control/config");
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── PATCH /api/control/config — Proxy zu :8081/control/config ──

controlRoutes.patch("/control/config", async (c) => {
  try {
    const body = await c.req.json();
    return await proxyJson(getCortexControlUrl(), "PATCH", "/control/config", body);
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── POST /api/control/pause — Setzt rate_limit_rps=0 (LLM-Pause) ──
// Speichert den vorherigen Wert intern zum Restaurieren bei Resume.

let savedRateLimit: number | null = null;

controlRoutes.post("/control/pause", async (c) => {
  try {
    // Aktuellen Wert lesen und merken
    const configResp = await fetch(`${getCortexControlUrl()}/control/config`, {
      signal: AbortSignal.timeout(PROXY_TIMEOUT_MS),
    });
    if (!configResp.ok) {
      return c.json({ error: "Config lesen fehlgeschlagen" }, 502);
    }
    const config = (await configResp.json()) as { rate_limit_rps?: number };
    const currentRate = config.rate_limit_rps ?? 0;

    // Nur speichern wenn nicht bereits pausiert (rate > 0)
    if (currentRate > 0) {
      savedRateLimit = currentRate;
    }

    // Rate auf 0 setzen
    return await proxyJson(getCortexControlUrl(), "PATCH", "/control/config", {
      rate_limit_rps: 0,
    });
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── POST /api/control/resume — Restauriert rate_limit_rps ──

controlRoutes.post("/control/resume", async (c) => {
  try {
    const restoreRate = savedRateLimit ?? 10;
    savedRateLimit = null;
    return await proxyJson(getCortexControlUrl(), "PATCH", "/control/config", {
      rate_limit_rps: restoreRate,
    });
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── POST /api/control/provider — Provider wechseln ──

controlRoutes.post("/control/provider", async (c) => {
  try {
    const body = await c.req.json();
    return await proxyJson(getCortexControlUrl(), "POST", "/control/provider", body);
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── POST /api/control/agent-provider — Agent-Override setzen ──

controlRoutes.post("/control/agent-provider", async (c) => {
  try {
    const body = await c.req.json();
    return await proxyJson(getCortexControlUrl(), "POST", "/control/agent-provider", body);
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── DELETE /api/control/agent-provider — Agent-Override entfernen ──

controlRoutes.delete("/control/agent-provider", async (c) => {
  try {
    const body = await c.req.json();
    return await proxyJson(getCortexControlUrl(), "DELETE", "/control/agent-provider", body);
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

controlRoutes.post("/control/chaos", async (c) => {
  try {
    const body = await c.req.json();
    return await proxyJson(
      getOperatorApiUrl(),
      "POST",
      "/operator/chaos",
      body,
      getOperatorHeaders(),
    );
  } catch (err) {
    return c.json(
      { error: "Operator-API nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

controlRoutes.post("/control/stimulus", async (c) => {
  try {
    const body = await c.req.json();
    return await proxyJson(
      getOperatorApiUrl(),
      "POST",
      "/operator/stimulus",
      body,
      getOperatorHeaders(),
    );
  } catch (err) {
    return c.json(
      { error: "Operator-API nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

controlRoutes.post("/control/nightrun", async (c) => {
  try {
    const body = await c.req.json().catch(() => ({}));
    return await proxyJson(
      getOperatorApiUrl(),
      "POST",
      "/operator/nightrun",
      body,
      getOperatorHeaders(),
    );
  } catch (err) {
    return c.json(
      { error: "Operator-API nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── Time Machine: Snapshots + Restore ──

controlRoutes.get("/control/snapshots", async (c) => {
  try {
    const resp = await fetch(`${getOperatorApiUrl()}/operator/snapshots`, {
      signal: AbortSignal.timeout(5000),
    });
    if (!resp.ok) return c.json({ error: `Operator ${resp.status}` }, resp.status as any);
    return c.json(await resp.json());
  } catch (err) {
    return c.json({ error: "Operator-API nicht erreichbar", detail: String(err) }, 502);
  }
});

controlRoutes.post("/control/snapshot", async (c) => {
  try {
    const body = await c.req.json().catch(() => ({}));
    return await proxyJson(
      getOperatorApiUrl(),
      "POST",
      "/operator/snapshot",
      body,
      getOperatorHeaders(),
    );
  } catch (err) {
    return c.json({ error: "Operator-API nicht erreichbar", detail: String(err) }, 502);
  }
});

controlRoutes.post("/control/restore", async (c) => {
  try {
    const body = await c.req.json();
    const result = await proxyJson(
      getOperatorApiUrl(),
      "POST",
      "/operator/restore",
      body,
      getOperatorHeaders(),
    );

    // Cache-Invalidierung nach erfolgreichem Restore (#253)
    if (result.status < 400) {
      resetCaches();
      resetWatermarks();
      broadcast({ type: "snapshot_restored" });
    }

    return result;
  } catch (err) {
    return c.json({ error: "Operator-API nicht erreichbar", detail: String(err) }, 502);
  }
});

controlRoutes.post("/control/prune", async (c) => {
  try {
    return await proxyJson(
      getOperatorApiUrl(),
      "POST",
      "/operator/prune",
      {},
      getOperatorHeaders(),
    );
  } catch (err) {
    return c.json({ error: "Operator-API nicht erreichbar", detail: String(err) }, 502);
  }
});

// ── GET /api/control/traffic-stats — Traffic Control Stats (AC-19) ──
controlRoutes.get("/control/traffic-stats", async (c) => {
  try {
    return await proxyGet(getCortexControlUrl(), "/control/traffic-stats");
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

controlRoutes.get("/control/platform-state", async (c) => {
  try {
    return await proxyGet(
      getOperatorApiUrl(),
      "/operator/platform-state",
      getOperatorHeaders(),
    );
  } catch (err) {
    return c.json(
      { error: "Operator-API nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

controlRoutes.get("/control/platform-analyses", (c) => {
  try {
    const limit = Math.min(
      Math.max(parseInt(c.req.query("limit") || "50", 10) || 50, 1),
      200,
    );
    return c.json(getRecentPlatformAnalyses(limit));
  } catch (err) {
    return c.json(
      { error: "Platform-Analysen nicht verfuegbar", detail: String(err) },
      500,
    );
  }
});

controlRoutes.post("/control/platform-analyze", async (c) => {
  try {
    const body = await c.req.json().catch(() => ({}));
    return await proxyJson(
      getOperatorApiUrl(),
      "POST",
      "/operator/platform-analyze",
      body,
      getOperatorHeaders(),
    );
  } catch (err) {
    return c.json(
      { error: "Operator-API nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── GET /api/control/status — Aggregierter Status ──
// Kombiniert Config + Cortex Health + Guardrails fuer das Frontend.

controlRoutes.get("/control/status", async (c) => {
  try {
    const [configResp, healthResp] = await Promise.all([
      fetch(`${getCortexControlUrl()}/control/config`, {
        signal: AbortSignal.timeout(PROXY_TIMEOUT_MS),
      }),
      fetch(`${getCortexControlUrl()}/health`, {
        signal: AbortSignal.timeout(PROXY_TIMEOUT_MS),
      }),
    ]);

    const config = configResp.ok ? await configResp.json() : null;
    const health = healthResp.ok ? await healthResp.json() : null;

    const cfg = config as Record<string, unknown> | null;
    const rateLimitRps =
      cfg && typeof cfg.rate_limit_rps === "number" ? cfg.rate_limit_rps : -1;

    return c.json({
      connected: true,
      paused: rateLimitRps === 0,
      config,
      health,
      saved_rate_limit: savedRateLimit,
    });
  } catch (err) {
    return c.json({
      connected: false,
      paused: false,
      config: null,
      health: null,
      error: String(err),
    });
  }
});

// ── Operator-API Reverse-Proxy ──────────────────────────
// Proxied /api/operator/* an den Daemon Operator-API Port.
// Besucher-Chat, Voice of Gaia, Broadcast.

controlRoutes.post("/operator/chat", async (c) => {
  const body = await c.req.json();
  return proxyJson(getOperatorApiUrl(), "POST", "/operator/chat", body, getOperatorHeaders());
});

controlRoutes.post("/operator/gaia", async (c) => {
  const body = await c.req.json();
  return proxyJson(getOperatorApiUrl(), "POST", "/operator/gaia", body, getOperatorHeaders());
});

controlRoutes.post("/operator/broadcast", async (c) => {
  const body = await c.req.json();
  return proxyJson(getOperatorApiUrl(), "POST", "/operator/broadcast", body, getOperatorHeaders());
});

// Export for testing
export {
  savedRateLimit,
  getCortexControlUrl,
  getOperatorApiUrl,
};
