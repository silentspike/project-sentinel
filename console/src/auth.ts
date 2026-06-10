// httpOnly-Session-Auth-Client (#419) — gegen das #431-Backend (#402/#405-Muster).
// Der Cookie ist httpOnly (JS kann ihn nicht lesen) -> Status via /api/auth/status nach Reload.

const base = "";

export async function authStatus(): Promise<boolean> {
  try {
    const r = await fetch(`${base}/api/auth/status`, { credentials: "include" });
    if (!r.ok) return false;
    return ((await r.json()) as { authenticated: boolean }).authenticated;
  } catch {
    return false;
  }
}

/// Login outcome: success, wrong key, or rate-limited (#474 — distinct UX on `429`).
export type LoginResult = "ok" | "invalid" | "rate-limited";

export async function login(key: string): Promise<LoginResult> {
  try {
    const r = await fetch(`${base}/api/auth/login`, {
      method: "POST",
      credentials: "include",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ key }),
    });
    if (r.status === 429) return "rate-limited";
    if (!r.ok) return "invalid";
    return ((await r.json()) as { authenticated: boolean }).authenticated ? "ok" : "invalid";
  } catch {
    return "invalid";
  }
}

export async function logout(): Promise<void> {
  try {
    await fetch(`${base}/api/auth/logout`, { method: "POST", credentials: "include" });
  } catch {
    /* ignore */
  }
}
