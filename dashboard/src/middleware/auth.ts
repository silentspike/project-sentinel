// Auth Middleware: Bearer Token Check fuer Write-Endpoints (POST/PATCH/DELETE).
// Wenn SENTINEL_DASHBOARD_API_KEY gesetzt → Bearer Token wird geprueft.
// Wenn nicht gesetzt → Write-Endpoints sind disabled (403).

import type { Context, Next } from "hono";

const API_KEY = process.env.SENTINEL_DASHBOARD_API_KEY || "";

export async function requireAuth(c: Context, next: Next): Promise<Response | void> {
  // Keine API-Key konfiguriert → Write-Endpoints gesperrt
  if (!API_KEY) {
    return c.json(
      { error: "Write-Endpoints deaktiviert: SENTINEL_DASHBOARD_API_KEY nicht konfiguriert" },
      403,
    );
  }

  const authHeader = c.req.header("Authorization") || "";
  if (!authHeader.startsWith("Bearer ")) {
    return c.json({ error: "Authorization header fehlt oder ungueltig" }, 401);
  }

  const token = authHeader.slice(7);
  if (token !== API_KEY) {
    return c.json({ error: "Ungueltiger API-Key" }, 403);
  }

  await next();
}

// Export fuer Tests
export { API_KEY };
