// Auth-Routes (#402): Login/Logout/Status fuer die server-side Session-Auth.
//
// Login validiert den eingegebenen Key constant-time gegen SENTINEL_DASHBOARD_API_KEY,
// mintet ein Session-Token und setzt es als httpOnly + SameSite=Strict Cookie. Der Key
// wird NICHT an den Client zurueckgegeben und nirgends client-seitig gespeichert.

import { Hono } from "hono";
import type { Context } from "hono";
import { setCookie, getCookie, deleteCookie } from "hono/cookie";
import { timingSafeEqual } from "node:crypto";
import { getDashboardApiKey } from "../middleware/auth";
import {
  createSession,
  revokeSession,
  validateSession,
  SESSION_COOKIE,
  SESSION_TTL_SECONDS,
} from "../auth-session";

export const authRoutes = new Hono();

// Constant-time-Vergleich (laengen-sicher) gegen Timing-Angriffe auf den Key.
function constantTimeEqual(a: string, b: string): boolean {
  const ab = Buffer.from(a, "utf8");
  const bb = Buffer.from(b, "utf8");
  if (ab.length !== bb.length) return false;
  return timingSafeEqual(ab, bb);
}

// Secure-Flag konfigurationsgetrieben: DASHBOARD_COOKIE_SECURE=auto|on|off (default auto).
// auto = Secure wenn Request ueber HTTPS laeuft (X-Forwarded-Proto hinter Proxy, sonst URL-Protokoll).
export function cookieSecure(c: Context): boolean {
  const mode = (process.env.DASHBOARD_COOKIE_SECURE || "auto").toLowerCase();
  if (mode === "on") return true;
  if (mode === "off") return false;
  const xfproto = c.req.header("X-Forwarded-Proto");
  if (xfproto) return xfproto.split(",")[0].trim().toLowerCase() === "https";
  try {
    return new URL(c.req.url).protocol === "https:";
  } catch {
    return false;
  }
}

// POST /api/auth/login — Key validieren, Session minten, Cookie setzen.
authRoutes.post("/auth/login", async (c) => {
  const apiKey = getDashboardApiKey();
  if (!apiKey) {
    return c.json(
      { error: "Auth deaktiviert: SENTINEL_DASHBOARD_API_KEY nicht konfiguriert", authenticated: false },
      403,
    );
  }
  let key = "";
  try {
    const body = (await c.req.json()) as { key?: unknown };
    if (typeof body.key === "string") key = body.key;
  } catch {
    key = "";
  }
  if (!constantTimeEqual(key, apiKey)) {
    return c.json({ error: "Ungueltiger API-Key", authenticated: false }, 401);
  }
  const token = createSession();
  setCookie(c, SESSION_COOKIE, token, {
    httpOnly: true,
    sameSite: "Strict",
    secure: cookieSecure(c),
    path: "/",
    maxAge: SESSION_TTL_SECONDS,
  });
  return c.json({ authenticated: true });
});

// POST /api/auth/logout — Session invalidieren, Cookie loeschen.
authRoutes.post("/auth/logout", (c) => {
  revokeSession(getCookie(c, SESSION_COOKIE));
  deleteCookie(c, SESSION_COOKIE, { path: "/" });
  return c.json({ authenticated: false });
});

// GET /api/auth/status — fuer UI-Restore nach Reload (Cookie ist httpOnly, JS kann es nicht lesen).
authRoutes.get("/auth/status", (c) => {
  return c.json({ authenticated: validateSession(getCookie(c, SESSION_COOKIE)) });
});
