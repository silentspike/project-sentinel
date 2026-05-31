//! Server-side Session-Auth (#431) — Port von `dashboard/src/auth-session.ts` + `routes/auth.ts` (#402/#405).
//!
//! In-Memory `Map<token, expiry_epoch_ms>`. Der Operator-Key (`SENTINEL_DASHBOARD_API_KEY`) verlaesst
//! den Server nie: Login validiert constant-time, mintet ein opakes UUID-Token, setzt es als
//! httpOnly + SameSite=Strict Cookie. Token sind revozierbar (Logout) + laufen nach 12h ab.
//! Nicht persistent (Server-Restart => Re-Login, fail-closed).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
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
}

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
pub async fn login(State(st): State<AppState>, body: Bytes) -> Response {
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Ungueltiger API-Key","authenticated":false})),
        )
            .into_response();
    }
    let token = st.sessions.create();
    let secure = if st.config.cookie_secure { "; Secure" } else { "" };
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict{secure}; Path=/; Max-Age={SESSION_TTL_SECS}"
    );
    ([(header::SET_COOKIE, cookie)], Json(json!({"authenticated":true}))).into_response()
}

/// POST /api/auth/logout — Session invalidieren + Cookie loeschen.
pub async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Response {
    st.sessions.revoke(cookie_value(&headers, SESSION_COOKIE).as_deref());
    let clear = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    ([(header::SET_COOKIE, clear)], Json(json!({"authenticated":false}))).into_response()
}

/// GET /api/auth/status — fuer UI-Restore nach Reload (Cookie ist httpOnly).
pub async fn status(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let ok = st.sessions.validate(cookie_value(&headers, SESSION_COOKIE).as_deref());
    Json(json!({ "authenticated": ok })).into_response()
}

/// Middleware: blockt unauthentifizierte Requests (kein/ungueltiges Session-Cookie) mit 401.
pub async fn require_auth(
    State(st): State<AppState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    if st.sessions.validate(cookie_value(&headers, SESSION_COOKIE).as_deref()) {
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
        assert!(!s.validate(Some("nonexistent")), "unbekanntes Token -> ungueltig");
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
        h.insert(header::COOKIE, "foo=bar; sentinel_session=abc123; baz=qux".parse().unwrap());
        assert_eq!(cookie_value(&h, SESSION_COOKIE).as_deref(), Some("abc123"));
        assert_eq!(cookie_value(&h, "missing"), None);
    }
}
