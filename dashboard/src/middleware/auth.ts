// Auth Middleware: Session-Cookie-Check fuer Write-Endpoints (POST/PATCH/DELETE) (#402).
// Wenn SENTINEL_DASHBOARD_API_KEY nicht gesetzt → Write-Endpoints disabled (403).
// Sonst: gueltiges httpOnly-Session-Cookie erforderlich (Login via /api/auth/login).
// Der Key wird nie mehr per Header transportiert (kein JS-zugaenglicher Token).

import type { Context, Next } from "hono";
import { getCookie } from "hono/cookie";
import { validateSession, SESSION_COOKIE } from "../auth-session";

export function getDashboardApiKey(): string {
  return process.env.SENTINEL_DASHBOARD_API_KEY || "";
}

export async function requireAuth(c: Context, next: Next): Promise<Response | void> {
  const apiKey = getDashboardApiKey();
  // Kein API-Key konfiguriert → Write-Endpoints gesperrt
  if (!apiKey) {
    return c.json(
      { error: "Write-Endpoints deaktiviert: SENTINEL_DASHBOARD_API_KEY nicht konfiguriert" },
      403,
    );
  }

  // Gueltiges Session-Cookie erforderlich (httpOnly, via Login gesetzt)
  if (!validateSession(getCookie(c, SESSION_COOKIE))) {
    return c.json({ error: "Nicht authentifiziert: Login erforderlich" }, 401);
  }

  await next();
}
