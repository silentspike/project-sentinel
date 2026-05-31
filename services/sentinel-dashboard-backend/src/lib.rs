//! SOTA dashboard backend (#431): axum HTTP(S) + WebTransport/QUIC push with
//! topic+msgpack+zstd framing. Serves the SolidJS bundle (#419), proxies the
//! Operator-API/Gateway control endpoints, and exposes the projection read-models.
//!
//! Modules:
//! - `codec`      — topic+msgpack+zstd frame encode/decode (noaide-compatible wire)
//! - `auth`       — httpOnly session auth (#402/#405 port)
//! - `projection` — read-only projection.db access + read routes
//! - `control`    — control-proxy to Operator-API/Gateway
//! - `wt`         — WebTransport/QUIC endpoint (self-signed TLS, uni-stream push)

#![forbid(unsafe_code)]

use std::sync::Arc;

pub mod auth;
pub mod codec;
pub mod control;
pub mod projection;
pub mod tls;
pub mod wt;

/// Runtime configuration (env-driven, plus the self-signed cert hash computed at startup).
#[derive(Clone, Debug)]
pub struct Config {
    /// Operator-Key fuer die Dashboard-Auth; None => Auth deaktiviert (fail-closed beim Login).
    pub dashboard_api_key: Option<String>,
    /// HTTPS-Bind (axum), z.B. `0.0.0.0:8001`.
    pub http_bind: String,
    /// WebTransport/QUIC-Bind (UDP), z.B. `0.0.0.0:4434`.
    pub wt_bind: String,
    /// Operator-API-Basis-URL (Daemon), Default `http://127.0.0.1:8084`.
    pub operator_url: String,
    /// Operator-API-Key (x-sentinel-operator-key); optional.
    pub operator_key: Option<String>,
    /// Gateway-Control-Basis-URL, Default `http://127.0.0.1:8081`.
    pub gateway_url: String,
    /// Pfad zur Projection-DB (read-only geoeffnet).
    pub projection_db: String,
    /// Verzeichnis mit der SolidJS-Bundle (ServeDir); enthaelt Platzhalter-index.html bis #419.
    pub bundle_dir: String,
    /// `Secure`-Flag fuer das Session-Cookie (true bei HTTPS-Serving).
    pub cookie_secure: bool,
    /// base64(sha-256) des self-signed Server-Certs — fuer `GET /api/cert-hash` (leer bei CA-Cert).
    pub cert_hash_b64: Option<String>,
}

impl Config {
    /// Liest die Konfiguration aus der Umgebung (Defaults = lokale Single-VM-Deploy).
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            dashboard_api_key: env("SENTINEL_DASHBOARD_API_KEY"),
            http_bind: env("SENTINEL_DASHBOARD_HTTP_BIND").unwrap_or_else(|| "0.0.0.0:8001".into()),
            wt_bind: env("SENTINEL_DASHBOARD_WT_BIND").unwrap_or_else(|| "0.0.0.0:4434".into()),
            operator_url: env("SENTINEL_OPERATOR_API_URL").unwrap_or_else(|| "http://127.0.0.1:8084".into()),
            operator_key: env("SENTINEL_OPERATOR_API_KEY"),
            gateway_url: env("CORTEX_GATEWAY_CONTROL_URL").unwrap_or_else(|| "http://127.0.0.1:8081".into()),
            projection_db: env("SENTINEL_PROJECTION_DB").unwrap_or_else(|| "/opt/sentinel/data/projection.db".into()),
            bundle_dir: env("SENTINEL_DASHBOARD_BUNDLE_DIR").unwrap_or_else(|| "/opt/sentinel/console-dist".into()),
            cookie_secure: env("DASHBOARD_COOKIE_SECURE").map(|v| v != "off").unwrap_or(true),
            cert_hash_b64: None,
        }
    }
}

/// Geteilter Anwendungs-State (Clone-billig: Arc-Felder).
#[derive(Clone)]
pub struct AppState {
    pub sessions: auth::SessionStore,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        Ok(Self {
            sessions: auth::SessionStore::new(),
            config: Arc::new(config),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
        })
    }
}
