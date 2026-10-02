// httpOnly-Session-Auth-Client (#419) — gegen das #431-Backend (#402/#405-Muster).
// Der Cookie ist httpOnly (JS kann ihn nicht lesen) -> Status via /api/auth/status nach Reload.

const base = "";

export interface AuthOutcomeBinding {
  generation: number;
  authenticated: boolean;
}

export async function authStatus(binding?: AuthOutcomeBinding): Promise<boolean> {
  const generation = binding?.generation;
  let authenticated = false;
  try {
    const r = await fetch(`${base}/api/auth/status`, { credentials: "include" });
    if (r.ok) authenticated = ((await r.json()) as { authenticated: boolean }).authenticated === true;
  } catch {
    /* fail closed */
  }
  if (binding) {
    if (binding.generation !== generation) return binding.authenticated;
    binding.authenticated = authenticated;
  }
  return authenticated;
}

/// Login outcome: success, wrong key, or rate-limited (#474 — distinct UX on `429`).
export type LoginResult = "ok" | "invalid" | "rate-limited";

export async function login(key: string, binding?: AuthOutcomeBinding): Promise<LoginResult> {
  const generation = binding ? ++binding.generation : undefined;
  if (binding) binding.authenticated = false;
  let outcome: LoginResult = "invalid";
  try {
    const r = await fetch(`${base}/api/auth/login`, {
      method: "POST",
      credentials: "include",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ key }),
    });
    if (r.status === 429) outcome = "rate-limited";
    else if (r.ok && ((await r.json()) as { authenticated: boolean }).authenticated === true) outcome = "ok";
  } catch {
    /* retain the invalid outcome */
  }
  if (binding) {
    if (binding.generation !== generation) return "invalid";
    binding.authenticated = outcome === "ok";
  }
  return outcome;
}

export async function logout(binding?: AuthOutcomeBinding): Promise<void> {
  if (binding) {
    ++binding.generation;
    binding.authenticated = false;
  }
  try {
    await fetch(`${base}/api/auth/logout`, { method: "POST", credentials: "include" });
  } catch {
    /* ignore */
  }
}
