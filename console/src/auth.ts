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

export async function login(key: string): Promise<boolean> {
  try {
    const r = await fetch(`${base}/api/auth/login`, {
      method: "POST",
      credentials: "include",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ key }),
    });
    if (!r.ok) return false;
    return ((await r.json()) as { authenticated: boolean }).authenticated;
  } catch {
    return false;
  }
}

export async function logout(): Promise<void> {
  try {
    await fetch(`${base}/api/auth/logout`, { method: "POST", credentials: "include" });
  } catch {
    /* ignore */
  }
}
