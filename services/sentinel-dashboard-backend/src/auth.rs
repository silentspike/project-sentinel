//! Server-side Session-Auth (#431/#402/#405) fuer die SolidJS-Konsole.
//!
//! In-Memory `Map<token, expiry_epoch_ms>`. Der Operator-Key (`SENTINEL_DASHBOARD_API_KEY`) verlaesst
//! den Server nie: Login validiert constant-time, mintet ein opakes UUID-Token, setzt es als
//! httpOnly + SameSite=Strict Cookie. Token sind revozierbar (Logout) + laufen nach 12h ab.
//! Nicht persistent (Server-Restart => Re-Login, fail-closed).

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{header, request::Parts, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::AppState;

/// Name des httpOnly-Session-Cookies (identisch zum Bun-Dashboard, gemeinsamer Login).
pub const SESSION_COOKIE: &str = "sentinel_session";
/// Session-TTL (12h), identisch zur Referenz.
pub const SESSION_TTL_SECS: u64 = 12 * 60 * 60;

/// Thread-sicherer In-Memory Session-Store: token -> expiry (epoch ms).
#[derive(Clone, Default)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<String, u64>>>,
    /// Kurzlebige Einmal-Tickets fuer den WebTransport-Handshake (Browser senden bei WT KEINE Cookies,
    /// daher kann der WT-Pfad nicht das Session-Cookie nutzen). token -> expiry (epoch ms).
    tickets: Arc<Mutex<HashMap<String, u64>>>,
}

/// Gueltigkeit eines WT-Tickets (kurz: nur fuer den unmittelbaren Connect).
pub const WT_TICKET_TTL_SECS: u64 = 30;

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Mintet ein kurzlebiges Einmal-WT-Ticket (nur fuer einen authentifizierten Caller via require_auth).
    pub fn issue_ticket(&self) -> String {
        let t = uuid::Uuid::new_v4().to_string();
        let mut m = self.tickets.lock().expect("ticket lock");
        m.insert(t.clone(), Self::now_ms() + WT_TICKET_TTL_SECS * 1000);
        t
    }

    /// Validiert + verbraucht (single-use) ein WT-Ticket am Handshake.
    pub fn consume_ticket(&self, ticket: Option<&str>) -> bool {
        let Some(ticket) = ticket else { return false };
        let mut m = self.tickets.lock().expect("ticket lock");
        match m.remove(ticket) {
            Some(expiry) => Self::now_ms() < expiry,
            None => false,
        }
    }

    /// Mintet ein neues opakes Token (12h gueltig).
    pub fn create(&self) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        let mut m = self.inner.lock().expect("session lock");
        m.insert(token.clone(), Self::now_ms() + SESSION_TTL_SECS * 1000);
        token
    }

    /// True, wenn Token existiert + nicht abgelaufen (lazy-cleanup bei Ablauf).
    pub fn validate(&self, token: Option<&str>) -> bool {
        let Some(token) = token else { return false };
        let mut m = self.inner.lock().expect("session lock");
        match m.get(token).copied() {
            None => false,
            Some(expiry) => {
                if Self::now_ms() >= expiry {
                    m.remove(token);
                    false
                } else {
                    true
                }
            }
        }
    }

    /// Invalidiert ein Token (Logout).
    pub fn revoke(&self, token: Option<&str>) {
        if let Some(token) = token {
            self.inner.lock().expect("session lock").remove(token);
        }
    }
}

/// #474: Per-client-IP fixed-window brute-force limiter for `POST /api/auth/login` ONLY.
///
/// Deliberately a tiny own implementation (no `governor` crate — supply-chain / `deny.toml`).
/// In-memory like the [`SessionStore`]: a restart resets the counters (fail-open on the
/// counter, fail-closed on sessions). Behind a loopback bind every client is `127.0.0.1`, so
/// the per-IP limiter acts effectively globally — accepted (anyone on the VM is already more
/// privileged); the ENV knobs are the tuning valve. `wt`/`require_auth`/`cert-hash` are untouched.
#[derive(Clone)]
pub struct LoginRateLimiter {
    inner: Arc<Mutex<HashMap<IpAddr, FailWindow>>>,
    max_fails: u32,
    window: Duration,
    block: Duration,
}

struct FailWindow {
    window_start: Instant,
    fails: u32,
    blocked_until: Option<Instant>,
}

/// Lazy-sweep bound: keep the map small even under an IP-spoofed flood.
const LIMITER_MAX_ENTRIES: usize = 1024;

impl LoginRateLimiter {
    /// `max_fails` failures within `window_secs` engage a block of `block_secs`.
    pub fn new(max_fails: u32, window_secs: u64, block_secs: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_fails: max_fails.max(1),
            window: Duration::from_secs(window_secs.max(1)),
            block: Duration::from_secs(block_secs.max(1)),
        }
    }

    /// `None` = free; `Some(secs)` = blocked, `Retry-After` seconds (rounded up, never 0).
    pub fn blocked_secs(&self, ip: IpAddr) -> Option<u64> {
        self.blocked_secs_at(ip, Instant::now())
    }

    fn blocked_secs_at(&self, ip: IpAddr, now: Instant) -> Option<u64> {
        let mut m = self.inner.lock().expect("limiter lock");
        // Copy the field out so the immutable borrow ends before the conditional `remove`.
        let blocked_until = m.get(&ip)?.blocked_until;
        match blocked_until {
            Some(until) if now < until => Some((until - now).as_secs() + 1),
            Some(_) => {
                // Block expired -> drop the entry (lazy cleanup), free again.
                m.remove(&ip);
                None
            }
            None => None,
        }
    }

    /// Record a failed login. Returns `(fails_in_window, newly_blocked)`.
    pub fn record_failure(&self, ip: IpAddr) -> (u32, bool) {
        self.record_failure_at(ip, Instant::now())
    }

    fn record_failure_at(&self, ip: IpAddr, now: Instant) -> (u32, bool) {
        let window = self.window;
        let block = self.block;
        let max_fails = self.max_fails;
        let mut m = self.inner.lock().expect("limiter lock");

        // Memory bound: drop entries whose window rolled over and that are not blocked.
        if m.len() > LIMITER_MAX_ENTRIES {
            m.retain(|_, w| {
                let blocked = w.blocked_until.is_some_and(|u| now < u);
                blocked || now.saturating_duration_since(w.window_start) < window
            });
        }

        let entry = m.entry(ip).or_insert_with(|| FailWindow {
            window_start: now,
            fails: 0,
            blocked_until: None,
        });

        // Fresh window if not currently blocked and the old window expired.
        let block_over = entry.blocked_until.is_none_or(|u| now >= u);
        if block_over && now.saturating_duration_since(entry.window_start) >= window {
            entry.window_start = now;
            entry.fails = 0;
            entry.blocked_until = None;
        }

        entry.fails += 1;
        let mut newly_blocked = false;
        if entry.fails >= max_fails && entry.blocked_until.is_none() {
            entry.blocked_until = Some(now + block);
            newly_blocked = true;
        }
        (entry.fails, newly_blocked)
    }

    /// Clear an IP's counter after a successful login (legitimate operator after typos).
    /// Does NOT unblock an active block — that only expires by time.
    pub fn reset(&self, ip: IpAddr) {
        self.inner.lock().expect("limiter lock").remove(&ip);
    }
}

/// Client IP extracted from `ConnectInfo<SocketAddr>` (installed via
/// `into_make_service_with_connect_info` in `main.rs`). Falls back to `0.0.0.0` when no
/// `ConnectInfo` is present (e.g. `oneshot` tests) so handlers never 500 over a missing extension.
pub struct ClientIp(pub IpAddr);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        Ok(ClientIp(ip))
    }
}

/// Laengen-sicherer constant-time-Vergleich (gegen Timing-Angriffe auf den Key).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Liest einen Cookie-Wert aus dem `Cookie`-Header.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

/// POST /api/auth/login — Key validieren, Session minten, httpOnly-Cookie setzen.
/// Body wird lenient geparst (fehlend/ungueltig => key="", => 401), wie die Referenz.
///
/// #474: per-IP brute-force protection. The block is checked **before** the key comparison so a
/// blocked IP gets `429` without the key ever being evaluated (no oracle: even a correct key
/// returns `429` while blocked). Failed attempts are audit-logged (never the attempted key).
pub async fn login(State(st): State<AppState>, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    // (1) Block-check BEFORE the key comparison (no key oracle during a block).
    if let Some(retry) = st.login_limiter.blocked_secs(ip) {
        tracing::warn!(client_ip = %ip, retry_after_secs = retry, "audit: login blocked (rate limit)");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry.to_string())],
            Json(json!({
                "error":"Zu viele fehlgeschlagene Login-Versuche, bitte spaeter erneut versuchen",
                "retry_after_secs": retry,
                "authenticated": false
            })),
        )
            .into_response();
    }
    // A missing key = auth disabled (config error), not a brute-force attempt -> no failure recorded.
    let Some(api_key) = st.config.dashboard_api_key.as_deref() else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Auth deaktiviert: SENTINEL_DASHBOARD_API_KEY nicht konfiguriert","authenticated":false})),
        )
            .into_response();
    };
    let key = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("key").and_then(|k| k.as_str().map(String::from)))
        .unwrap_or_default();
    if !constant_time_eq(key.as_bytes(), api_key.as_bytes()) {
        let (fails, newly_blocked) = st.login_limiter.record_failure(ip);
        // Audit: log the attempt + count, NEVER the attempted key.
        tracing::warn!(client_ip = %ip, fails, "audit: login failed (invalid operator key)");
        if newly_blocked {
            tracing::warn!(client_ip = %ip, "audit: login rate-limit block engaged");
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Ungueltiger API-Key","authenticated":false})),
        )
            .into_response();
    }
    st.login_limiter.reset(ip);
    tracing::info!(client_ip = %ip, "audit: login success");
    let token = st.sessions.create();
    let secure = if st.config.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict{secure}; Path=/; Max-Age={SESSION_TTL_SECS}"
    );
    (
        [(header::SET_COOKIE, cookie)],
        Json(json!({"authenticated":true})),
    )
        .into_response()
}

/// POST /api/auth/logout — Session invalidieren + Cookie loeschen.
pub async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Response {
    st.sessions
        .revoke(cookie_value(&headers, SESSION_COOKIE).as_deref());
    let clear = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    (
        [(header::SET_COOKIE, clear)],
        Json(json!({"authenticated":false})),
    )
        .into_response()
}

/// GET /api/auth/status — fuer UI-Restore nach Reload (Cookie ist httpOnly).
pub async fn status(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let ok = st
        .sessions
        .validate(cookie_value(&headers, SESSION_COOKIE).as_deref());
    Json(json!({ "authenticated": ok })).into_response()
}

/// GET /api/wt-ticket — mintet ein kurzlebiges Einmal-Ticket fuer den WebTransport-Connect.
/// Hinter `require_auth` (gueltige Session noetig). Der Client haengt es als `?t=<ticket>` an die WT-URL.
pub async fn wt_ticket(State(st): State<AppState>) -> Response {
    Json(json!({ "ticket": st.sessions.issue_ticket() })).into_response()
}

/// Middleware: blockt unauthentifizierte Requests (kein/ungueltiges Session-Cookie) mit 401.
pub async fn require_auth(
    State(st): State<AppState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    if st
        .sessions
        .validate(cookie_value(&headers, SESSION_COOKIE).as_deref())
    {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"nicht authentifiziert","authenticated":false})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifecycle_create_validate_revoke() {
        let s = SessionStore::new();
        let t = s.create();
        assert!(s.validate(Some(&t)), "frisch gemintet -> gueltig");
        assert!(
            !s.validate(Some("nonexistent")),
            "unbekanntes Token -> ungueltig"
        );
        assert!(!s.validate(None), "kein Token -> ungueltig");
        s.revoke(Some(&t));
        assert!(!s.validate(Some(&t)), "revoked -> ungueltig");
    }

    #[test]
    fn constant_time_eq_correct() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreX"));
        assert!(!constant_time_eq(b"secret", b"sec"), "Laengen-Mismatch");
    }

    #[test]
    fn cookie_value_parses_session() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            "foo=bar; sentinel_session=abc123; baz=qux".parse().unwrap(),
        );
        assert_eq!(cookie_value(&h, SESSION_COOKIE).as_deref(), Some("abc123"));
        assert_eq!(cookie_value(&h, "missing"), None);
    }

    // ── #474 LoginRateLimiter ──────────────────────────────────────────────
    // All tests inject `now` (the `_at` variants) — deterministic, no sleeps.

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, n))
    }

    #[test]
    fn limiter_allows_below_threshold() {
        let rl = LoginRateLimiter::new(5, 60, 300);
        let t0 = Instant::now();
        for _ in 0..4 {
            let (_, blocked) = rl.record_failure_at(ip(1), t0);
            assert!(!blocked, "below threshold must not block");
        }
        assert_eq!(rl.blocked_secs_at(ip(1), t0), None, "4 < 5 -> free");
    }

    #[test]
    fn limiter_blocks_at_threshold() {
        let rl = LoginRateLimiter::new(5, 60, 300);
        let t0 = Instant::now();
        for i in 1..=5 {
            let (fails, blocked) = rl.record_failure_at(ip(1), t0);
            assert_eq!(fails, i);
            assert_eq!(blocked, i == 5, "the 5th failure engages the block");
        }
        let retry = rl.blocked_secs_at(ip(1), t0).expect("blocked");
        assert!(
            retry > 0 && retry <= 301,
            "retry-after within block window: {retry}"
        );
    }

    #[test]
    fn limiter_block_expires() {
        let rl = LoginRateLimiter::new(3, 60, 300);
        let t0 = Instant::now();
        for _ in 0..3 {
            rl.record_failure_at(ip(1), t0);
        }
        assert!(
            rl.blocked_secs_at(ip(1), t0).is_some(),
            "blocked right after"
        );
        let after = t0 + Duration::from_secs(301);
        assert_eq!(
            rl.blocked_secs_at(ip(1), after),
            None,
            "block expired -> free"
        );
        // Expired entry is dropped: a fresh failure starts a new window at fails=1.
        let (fails, blocked) = rl.record_failure_at(ip(1), after);
        assert_eq!((fails, blocked), (1, false));
    }

    #[test]
    fn limiter_window_rollover() {
        let rl = LoginRateLimiter::new(5, 60, 300);
        let t0 = Instant::now();
        for _ in 0..4 {
            rl.record_failure_at(ip(1), t0);
        }
        // A failure after the window has elapsed starts a fresh count, not the 5th -> no block.
        let later = t0 + Duration::from_secs(61);
        let (fails, blocked) = rl.record_failure_at(ip(1), later);
        assert_eq!((fails, blocked), (1, false), "window rolled over");
        assert_eq!(rl.blocked_secs_at(ip(1), later), None);
    }

    #[test]
    fn limiter_reset_on_success() {
        let rl = LoginRateLimiter::new(5, 60, 300);
        let t0 = Instant::now();
        for _ in 0..4 {
            rl.record_failure_at(ip(1), t0);
        }
        rl.reset(ip(1));
        // After reset the next failure starts from 1 again.
        let (fails, _) = rl.record_failure_at(ip(1), t0);
        assert_eq!(fails, 1, "reset cleared the counter");
    }

    #[test]
    fn limiter_multi_ip_independent() {
        let rl = LoginRateLimiter::new(3, 60, 300);
        let t0 = Instant::now();
        for _ in 0..3 {
            rl.record_failure_at(ip(1), t0);
        }
        assert!(rl.blocked_secs_at(ip(1), t0).is_some(), "ip 1 blocked");
        assert_eq!(rl.blocked_secs_at(ip(2), t0), None, "ip 2 unaffected");
    }

    #[test]
    fn limiter_sweep_bounds_memory() {
        let rl = LoginRateLimiter::new(5, 60, 300);
        let t0 = Instant::now();
        // Fill > LIMITER_MAX_ENTRIES with rolled-over (expired-window) single failures.
        for n in 0..(LIMITER_MAX_ENTRIES + 50) {
            let addr = IpAddr::V4(Ipv4Addr::new(10, (n >> 8) as u8, (n & 0xff) as u8, 7));
            rl.record_failure_at(addr, t0);
        }
        // A later failure triggers the retain-sweep; entries older than the window are dropped.
        let later = t0 + Duration::from_secs(120);
        rl.record_failure_at(ip(9), later);
        let len = rl.inner.lock().unwrap().len();
        assert!(len <= LIMITER_MAX_ENTRIES + 1, "map stays bounded: {len}");
    }
}
