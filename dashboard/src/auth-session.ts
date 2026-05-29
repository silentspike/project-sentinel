// Server-side Session-Store fuer die Dashboard-Operator-Auth (#402).
//
// In-Memory Map<token, expiryEpochMs>. Der Operator-Key (SENTINEL_DASHBOARD_API_KEY)
// verlaesst den Server nach dem Login nie wieder: stattdessen wird ein zufaelliges,
// opakes Token gemintet und nur dieses (als httpOnly-Cookie) an den Client gegeben.
// Token sind revozierbar (Logout) und laufen nach SESSION_TTL_MS ab. Bewusst NICHT
// persistent (kein Redis/DB) — Server-Restart => Re-Login (fail-closed).

import { randomUUID } from "node:crypto";

const SESSION_TTL_MS = 12 * 60 * 60 * 1000; // 12 Stunden
/// TTL in Sekunden fuer das Cookie-maxAge.
export const SESSION_TTL_SECONDS = SESSION_TTL_MS / 1000;
/// Name des httpOnly-Session-Cookies.
export const SESSION_COOKIE = "sentinel_session";

const sessions = new Map<string, number>();

/// Mintet ein neues opakes Session-Token und registriert es mit Ablauf in 12h.
export function createSession(): string {
  const token = randomUUID();
  sessions.set(token, Date.now() + SESSION_TTL_MS);
  return token;
}

/// True, wenn das Token existiert und nicht abgelaufen ist (lazy-cleanup bei Ablauf).
export function validateSession(token: string | undefined | null): boolean {
  if (!token) return false;
  const expiry = sessions.get(token);
  if (expiry === undefined) return false;
  if (Date.now() >= expiry) {
    sessions.delete(token);
    return false;
  }
  return true;
}

/// Invalidiert ein Token (Logout).
export function revokeSession(token: string | undefined | null): void {
  if (token) sessions.delete(token);
}

// ── Test-Helper ──
/// Loescht alle Sessions (Test-Isolation).
export function _clearAllSessions(): void {
  sessions.clear();
}
/// Setzt ein bestehendes Token auf abgelaufen (Test fuer Expiry).
export function _forceExpire(token: string): void {
  if (sessions.has(token)) sessions.set(token, 0);
}
