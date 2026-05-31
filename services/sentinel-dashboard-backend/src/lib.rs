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
//! - `event_sub`  — NATS-Event-Subscriber (#432): pusht Delta-Frames in den Broadcast-Kanal

#![forbid(unsafe_code)]

use std::sync::Arc;

pub mod auth;
pub mod codec;
pub mod control;
pub mod event_sub;
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
    /// NATS-Server-URL fuer den Event-Stream-Subscriber (#432), Default `nats://127.0.0.1:4222`.
    pub nats_url: String,
    /// Log-Label fuer den Event-Subscriber (#432). NICHT der NATS-durable-Name — der Consumer ist
    /// ephemeral (serverseitig generierter Name). Default `dashboard-live`.
    pub nats_consumer: String,
}

impl Config {
    /// Liest die Konfiguration aus der Umgebung (Defaults = lokale Single-VM-Deploy).
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            dashboard_api_key: env("SENTINEL_DASHBOARD_API_KEY"),
            http_bind: env("SENTINEL_DASHBOARD_HTTP_BIND").unwrap_or_else(|| "0.0.0.0:8001".into()),
            // WT teilt den Port der HTTPS-Seite (UDP/QUIC) -> same-origin: vermeidet Cross-Origin-Komplexitaet
            // und teilt dasselbe self-signed Cert + cert-hash. Die WT-Auth laeuft NICHT ueber Cookies
            // (WebTransport-Handshakes tragen keine Cookies, siehe wt.rs), sondern ueber das kurzlebige
            // Einmal-Ticket (`?t=`).
            wt_bind: env("SENTINEL_DASHBOARD_WT_BIND").unwrap_or_else(|| "0.0.0.0:8001".into()),
            operator_url: env("SENTINEL_OPERATOR_API_URL").unwrap_or_else(|| "http://127.0.0.1:8084".into()),
            operator_key: env("SENTINEL_OPERATOR_API_KEY"),
            gateway_url: env("CORTEX_GATEWAY_CONTROL_URL").unwrap_or_else(|| "http://127.0.0.1:8081".into()),
            projection_db: env("SENTINEL_PROJECTION_DB").unwrap_or_else(|| "/opt/sentinel/data/projection.db".into()),
            bundle_dir: env("SENTINEL_DASHBOARD_BUNDLE_DIR").unwrap_or_else(|| "/opt/sentinel/console-dist".into()),
            cookie_secure: env("DASHBOARD_COOKIE_SECURE").map(|v| v != "off").unwrap_or(true),
            cert_hash_b64: None,
            nats_url: env("SENTINEL_NATS_URL").unwrap_or_else(|| "nats://127.0.0.1:4222".into()),
            nats_consumer: env("SENTINEL_DASHBOARD_NATS_CONSUMER").unwrap_or_else(|| "dashboard-live".into()),
        }
    }
}

/// Geteilter Anwendungs-State (Clone-billig: Arc-Felder).
#[derive(Clone)]
pub struct AppState {
    pub sessions: auth::SessionStore,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    /// Broadcast-Kanal der Delta-Frames (#432): der Event-Subscriber sendet encodierte topic-Frames,
    /// jede WebTransport-Session abonniert via `subscribe()` und schreibt sie als uni-Streams an den Client.
    pub broadcast_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
}

impl AppState {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        // Kapazitaet 256: ueberlaeuft ein langsamer Client, liefert `recv()` `Lagged` — der naechste
        // Voll-Snapshot ist ohnehin autoritativ (reconcile), also tolerierbar.
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(256);
        Ok(Self {
            sessions: auth::SessionStore::new(),
            config: Arc::new(config),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
            broadcast_tx,
        })
    }
}

/// Baut die komplette axum-App (HTTP/Control-Routen + Projection-Reads + ServeDir + CORS).
/// Aus `main` ausgelagert, damit Integrationstests die Auth-Gates (#463) ohne Live-Server pruefen.
pub fn build_app(state: AppState) -> axum::Router {
    use axum::middleware;
    use axum::routing::{get, post};
    use tower_http::{cors::CorsLayer, services::ServeDir};

    // #463: Projection-Read-Routen hinter `require_auth` (konsistent zur Control-Plane) — die echten
    // Sim-Projektionsdaten (Agenten/Raeume/KPIs/Tasks) sind sonst unauthentifiziert per HTTPS-GET lesbar.
    let read_routes = axum::Router::new()
        .route("/agents", get(projection::agents))
        .route("/rooms", get(projection::rooms))
        .route("/metrics", get(projection::metrics))
        .route("/tasks", get(projection::tasks))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    // Control-Proxy hinter require_auth.
    let control_routes = axum::Router::new()
        .route("/chaos", post(control::chaos))
        .route("/stimulus", post(control::stimulus))
        .route("/nightrun", post(control::nightrun))
        .route("/config", get(control::get_config).patch(control::patch_config))
        .route("/provider", post(control::provider))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    let api = axum::Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/status", get(auth::status))
        .route(
            "/wt-ticket",
            get(auth::wt_ticket).route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth)),
        )
        // `/cert-hash` bleibt BEWUSST public: der Browser braucht den Cert-Hash VOR dem Login, um die
        // WebTransport-Verbindung via `serverCertificateHashes` zu pinnen (self-signed) — der Connect
        // (und damit das Holen des Auth-Tickets) laeuft sonst nie an. Liefert nur den oeffentlichen
        // Zertifikats-Hash, keine sensiblen Daten.
        .route("/cert-hash", get(cert_hash))
        .merge(read_routes)
        .nest("/control", control_routes);

    axum::Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(&state.config.bundle_dir).append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// GET /api/cert-hash — base64(sha-256(cert DER)) fuer WebTransport `serverCertificateHashes` (leer bei CA-Cert).
async fn cert_hash(
    axum::extract::State(st): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "hash": st.config.cert_hash_b64, "algorithm": "sha-256" }))
}
