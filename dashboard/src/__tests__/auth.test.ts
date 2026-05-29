import { describe, it, expect, afterEach, beforeEach } from "bun:test";
import { app } from "../index";
import {
  _clearAllSessions,
  _forceExpire,
  createSession,
  SESSION_COOKIE,
} from "../auth-session";

const ORIG_KEY = process.env.SENTINEL_DASHBOARD_API_KEY;
const ORIG_SECURE = process.env.DASHBOARD_COOKIE_SECURE;

function setCookieHeader(res: Response): string {
  return res.headers.get("set-cookie") || "";
}

describe("Auth Routes (#402)", () => {
  beforeEach(() => {
    _clearAllSessions();
    process.env.SENTINEL_DASHBOARD_API_KEY = "dash-key";
    process.env.DASHBOARD_COOKIE_SECURE = "off";
  });

  afterEach(() => {
    _clearAllSessions();
    process.env.SENTINEL_DASHBOARD_API_KEY = ORIG_KEY ?? "";
    if (ORIG_SECURE === undefined) delete process.env.DASHBOARD_COOKIE_SECURE;
    else process.env.DASHBOARD_COOKIE_SECURE = ORIG_SECURE;
  });

  it("login with correct key sets httpOnly + SameSite=Strict cookie (CSRF mitigation)", async () => {
    const res = await app.request("/api/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ key: "dash-key" }),
    });
    expect(res.status).toBe(200);
    expect((await res.json()).authenticated).toBe(true);
    const sc = setCookieHeader(res);
    expect(sc).toContain(`${SESSION_COOKIE}=`);
    expect(sc.toLowerCase()).toContain("httponly");
    expect(sc).toMatch(/samesite=strict/i);
    // DASHBOARD_COOKIE_SECURE=off → kein Secure-Flag
    expect(sc.toLowerCase()).not.toContain("secure");
  });

  it("login with wrong key → 401, kein Session-Cookie", async () => {
    const res = await app.request("/api/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ key: "falsch" }),
    });
    expect(res.status).toBe(401);
    expect(setCookieHeader(res)).not.toContain(`${SESSION_COOKIE}=`);
  });

  it("login → 403 wenn kein Env-Key konfiguriert", async () => {
    process.env.SENTINEL_DASHBOARD_API_KEY = "";
    const res = await app.request("/api/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ key: "x" }),
    });
    expect(res.status).toBe(403);
  });

  it("DASHBOARD_COOKIE_SECURE=on erzwingt Secure-Flag", async () => {
    process.env.DASHBOARD_COOKIE_SECURE = "on";
    const res = await app.request("/api/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ key: "dash-key" }),
    });
    expect(setCookieHeader(res).toLowerCase()).toContain("secure");
  });

  it("status spiegelt gueltiges/fehlendes Cookie", async () => {
    const token = createSession();
    const ok = await app.request("/api/auth/status", {
      headers: { Cookie: `${SESSION_COOKIE}=${token}` },
    });
    expect((await ok.json()).authenticated).toBe(true);
    const none = await app.request("/api/auth/status");
    expect((await none.json()).authenticated).toBe(false);
  });

  it("logout invalidiert die Session", async () => {
    const token = createSession();
    const lo = await app.request("/api/auth/logout", {
      method: "POST",
      headers: { Cookie: `${SESSION_COOKIE}=${token}` },
    });
    expect(lo.status).toBe(200);
    const st = await app.request("/api/auth/status", {
      headers: { Cookie: `${SESSION_COOKIE}=${token}` },
    });
    expect((await st.json()).authenticated).toBe(false);
  });

  it("abgelaufene Session → status false", async () => {
    const token = createSession();
    _forceExpire(token);
    const st = await app.request("/api/auth/status", {
      headers: { Cookie: `${SESSION_COOKIE}=${token}` },
    });
    expect((await st.json()).authenticated).toBe(false);
  });

  it("requireAuth: ohne Cookie → 401, mit gueltigem Cookie → erreicht Proxy", async () => {
    // ohne Cookie → 401
    const r1 = await app.request("/api/control/pause", { method: "POST" });
    expect(r1.status).toBe(401);

    // mit gueltigem Cookie → passiert die Auth (Gateway gemockt)
    const token = createSession();
    const origFetch = globalThis.fetch;
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ rate_limit_rps: 10 }), { status: 200 })) as unknown as typeof fetch;
    try {
      const r2 = await app.request("/api/control/pause", {
        method: "POST",
        headers: { Cookie: `${SESSION_COOKIE}=${token}` },
      });
      expect(r2.status).not.toBe(401);
      expect(r2.status).not.toBe(403);
    } finally {
      globalThis.fetch = origFetch;
    }
  });
});
