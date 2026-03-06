// Control Plane Proxy: Dashboard → Cortex Gateway :8081
// Proxied alle Control-Requests an den Cortex Gateway Control Plane Port.
// "Pause" = rate_limit_rps auf 0 setzen, "Resume" = vorherigen Wert restaurieren.

import { Hono } from "hono";
import { requireAuth } from "../middleware/auth";

export const controlRoutes = new Hono();

// Auth-Middleware auf alle Write-Endpoints (POST/PATCH/DELETE)
controlRoutes.use("/control/*", async (c, next) => {
  const method = c.req.method;
  if (method === "POST" || method === "PATCH" || method === "DELETE") {
    return requireAuth(c, next);
  }
  await next();
});

const CORTEX_CONTROL_URL =
  process.env.CORTEX_GATEWAY_URL || "http://localhost:8081";

// Timeout fuer Proxy-Requests zum Cortex Gateway (ms).
const PROXY_TIMEOUT_MS = 5000;

// ── Helpers ──────────────────────────────────────────

async function proxyGet(path: string): Promise<Response> {
  const resp = await fetch(`${CORTEX_CONTROL_URL}${path}`, {
    signal: AbortSignal.timeout(PROXY_TIMEOUT_MS),
  });
  const body = await resp.text();
  return new Response(body, {
    status: resp.status,
    headers: { "Content-Type": "application/json" },
  });
}

async function proxyJson(
  method: string,
  path: string,
  body: unknown,
): Promise<Response> {
  const resp = await fetch(`${CORTEX_CONTROL_URL}${path}`, {
    method,
    headers: { "Content-Type": "application/json" },
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
    return await proxyGet("/control/config");
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
    return await proxyJson("PATCH", "/control/config", body);
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
    const configResp = await fetch(`${CORTEX_CONTROL_URL}/control/config`, {
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
    return await proxyJson("PATCH", "/control/config", {
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
    return await proxyJson("PATCH", "/control/config", {
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
    return await proxyJson("POST", "/control/provider", body);
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
    return await proxyJson("POST", "/control/agent-provider", body);
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
    return await proxyJson("DELETE", "/control/agent-provider", body);
  } catch (err) {
    return c.json(
      { error: "Cortex Gateway nicht erreichbar", detail: String(err) },
      502,
    );
  }
});

// ── GET /api/control/status — Aggregierter Status ──
// Kombiniert Config + Cortex Health + Guardrails fuer das Frontend.

controlRoutes.get("/control/status", async (c) => {
  try {
    const [configResp, healthResp] = await Promise.all([
      fetch(`${CORTEX_CONTROL_URL}/control/config`, {
        signal: AbortSignal.timeout(PROXY_TIMEOUT_MS),
      }),
      fetch(`${CORTEX_CONTROL_URL}/health`, {
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

// Export for testing
export { savedRateLimit, CORTEX_CONTROL_URL };
