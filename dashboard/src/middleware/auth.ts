// Auth Middleware: Bearer Token Check fuer Write-Endpoints (POST/PATCH/DELETE).
// Wenn SENTINEL_DASHBOARD_API_KEY gesetzt → Bearer Token wird geprueft.
// Wenn nicht gesetzt → Write-Endpoints sind disabled (403).

import type { Context, Next } from "hono";

export function getDashboardApiKey(): string {
  return process.env.SENTINEL_DASHBOARD_API_KEY || "";
}

export async function requireAuth(c: Context, next: Next): Promise<Response | void> {
  const apiKey = getDashboardApiKey();
  // Keine API-Key konfiguriert → Write-Endpoints gesperrt
  if (!apiKey) {
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
  if (token !== apiKey) {
    return c.json({ error: "Ungueltiger API-Key" }, 403);
  }

  await next();
}
