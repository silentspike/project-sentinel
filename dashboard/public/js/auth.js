// Dashboard Operator-Auth (#402): server-side Session + httpOnly-Cookie.
//
// Der Operator-Key wird NUR einmal beim Login ge-POSTet und nie in JS gehalten.
// Das Session-Cookie ist httpOnly (fuer JS via document.cookie unlesbar) und wird vom
// Browser automatisch same-origin mitgeschickt. Ersetzt das fruehere In-Memory-Key-Modul
// (In-Memory-Key + Authorization-Header).
//
// `authenticated` ist nur ein UI-Hinweis-Flag (per /api/auth/status nach Reload restauriert) —
// die echte Autorisierung macht ausschliesslich der Server anhand des Cookies.

let authenticated = false;

export function isAuthenticated() {
  return authenticated;
}

export async function login(key) {
  try {
    const res = await fetch('/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'same-origin',
      body: JSON.stringify({ key }),
    });
    const data = res.ok ? await res.json().catch(() => ({})) : {};
    authenticated = data.authenticated === true;
  } catch (_) {
    authenticated = false;
  }
  return authenticated;
}

export async function logout() {
  try {
    await fetch('/api/auth/logout', { method: 'POST', credentials: 'same-origin' });
  } catch (_) { /* ignore */ }
  authenticated = false;
}

export async function refreshAuthStatus() {
  try {
    const res = await fetch('/api/auth/status', { credentials: 'same-origin' });
    const data = res.ok ? await res.json().catch(() => ({})) : {};
    authenticated = data.authenticated === true;
  } catch (_) {
    authenticated = false;
  }
  return authenticated;
}

export function markUnauthenticated() {
  authenticated = false;
}

// Test-Helper: setzt das UI-Flag direkt (umgeht den Login-Fetch).
export function _setAuthenticated(value) {
  authenticated = value === true;
}
