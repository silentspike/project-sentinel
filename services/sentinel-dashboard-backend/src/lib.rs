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

use std::sync::{Arc, Mutex};

pub mod auth;
pub mod cas;
pub mod cockpit;
pub mod codec;
pub mod config;
pub mod control;
pub mod event_sub;
pub mod events;
pub mod metrics_extra;
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
    /// Gateway-Proxy-Basis-URL fuer Pipeline-Metrics, Default `http://127.0.0.1:8080`.
    pub gateway_proxy_url: String,
    /// Prometheus-Basis-URL des Daemons, Default `http://127.0.0.1:9090`.
    pub prometheus_url: String,
    /// Pfad zur Projection-DB (read-only geoeffnet).
    pub projection_db: String,
    /// Pfad zur Limbo events.db (read-only geoeffnet, optional/degradierend).
    pub events_db: String,
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
    /// Verzeichnis mit der Plattform-Config (Agent-TOMLs, rooms.toml, daemon.toml) — read-only fuer #420.
    pub config_dir: String,
    /// `[daemon].max_agents` aus daemon.toml (Single-Source wie der Daemon); None = lenient (Daemon
    /// bleibt finale Validierungs-Autoritaet). Siehe `config::read_max_agents`.
    pub max_agents: Option<usize>,
}

impl Config {
    /// Liest die Konfiguration aus der Umgebung (Defaults = lokale Single-VM-Deploy).
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let config_dir =
            env("SENTINEL_CONFIG_DIR").unwrap_or_else(|| "/opt/sentinel/config".into());
        // max_agents aus derselben daemon.toml wie der Daemon (Single-Source, kein Drift).
        let max_agents = config::read_max_agents(&config_dir);
        Self {
            dashboard_api_key: env("SENTINEL_DASHBOARD_API_KEY"),
            http_bind: env("SENTINEL_DASHBOARD_HTTP_BIND").unwrap_or_else(|| "0.0.0.0:8001".into()),
            // WT teilt den Port der HTTPS-Seite (UDP/QUIC) -> same-origin: vermeidet Cross-Origin-Komplexitaet
            // und teilt dasselbe self-signed Cert + cert-hash. Die WT-Auth laeuft NICHT ueber Cookies
            // (WebTransport-Handshakes tragen keine Cookies, siehe wt.rs), sondern ueber das kurzlebige
            // Einmal-Ticket (`?t=`).
            wt_bind: env("SENTINEL_DASHBOARD_WT_BIND").unwrap_or_else(|| "0.0.0.0:8001".into()),
            operator_url: env("SENTINEL_OPERATOR_API_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8084".into()),
            operator_key: env("SENTINEL_OPERATOR_API_KEY"),
            gateway_url: env("CORTEX_GATEWAY_CONTROL_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8081".into()),
            gateway_proxy_url: env("CORTEX_GATEWAY_PROXY_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8080".into()),
            prometheus_url: env("SENTINEL_DAEMON_PROMETHEUS_URL")
                .unwrap_or_else(|| "http://127.0.0.1:9090".into()),
            projection_db: env("SENTINEL_PROJECTION_DB")
                .unwrap_or_else(|| "/opt/sentinel/data/projection.db".into()),
            events_db: env("SENTINEL_EVENTS_DB")
                .unwrap_or_else(|| "/opt/sentinel/data/events.db".into()),
            bundle_dir: env("SENTINEL_DASHBOARD_BUNDLE_DIR")
                .unwrap_or_else(|| "/opt/sentinel/console-dist".into()),
            cookie_secure: env("DASHBOARD_COOKIE_SECURE")
                .map(|v| v != "off")
                .unwrap_or(true),
            cert_hash_b64: None,
            nats_url: env("SENTINEL_NATS_URL").unwrap_or_else(|| "nats://127.0.0.1:4222".into()),
            nats_consumer: env("SENTINEL_DASHBOARD_NATS_CONSUMER")
                .unwrap_or_else(|| "dashboard-live".into()),
            config_dir,
            max_agents,
        }
    }
}

/// Geteilter Anwendungs-State (Clone-billig: Arc-Felder).
#[derive(Clone)]
pub struct AppState {
    pub sessions: auth::SessionStore,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    /// Optionaler read-only Limbo EventStore-Handle. Nicht verfuegbar => Routen degradieren.
    pub events: Option<sentinel_limbo::EventStore>,
    /// Control-Pause merkt den vorherigen Gateway rate_limit_rps-Wert fuer Resume.
    pub saved_rate_limit: Arc<tokio::sync::Mutex<Option<f64>>>,
    /// Broadcast-Kanal der Delta-Frames (#432): der Event-Subscriber sendet encodierte topic-Frames,
    /// jede WebTransport-Session abonniert via `subscribe()` und schreibt sie als uni-Streams an den Client.
    pub broadcast_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    /// Event-Log CAS-Plane (#464): append-only Block-Log fuer den WT-Bi-Stream.
    pub event_cas: Arc<Mutex<cas::EventLogCasPlane>>,
}

impl AppState {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        // Kapazitaet 256: ueberlaeuft ein langsamer Client, liefert `recv()` `Lagged` — der naechste
        // Voll-Snapshot ist ohnehin autoritativ (reconcile), also tolerierbar.
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(256);
        let events = match sentinel_limbo::EventStore::open_readonly(&config.events_db) {
            Ok(store) => Some(store),
            Err(e) => {
                tracing::warn!(error = %e, path = %config.events_db, "events.db read-only handle unavailable; event routes degrade");
                None
            }
        };
        Ok(Self {
            sessions: auth::SessionStore::new(),
            config: Arc::new(config),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
            events,
            saved_rate_limit: Arc::new(tokio::sync::Mutex::new(None)),
            broadcast_tx,
            event_cas: Arc::new(Mutex::new(cas::EventLogCasPlane::new())),
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
        .route("/rooms/{id}/detail", get(projection::room_detail))
        .route("/metrics", get(projection::metrics))
        .route("/metrics/ebpf", get(metrics_extra::ebpf))
        .route("/metrics/pipeline", get(metrics_extra::pipeline))
        .route("/metrics/tick", get(metrics_extra::tick))
        .route("/tasks", get(projection::tasks))
        .route("/cockpit", get(cockpit::cockpit))
        .route("/cockpit/incident/{id}", get(cockpit::incident))
        .route("/events", get(events::events))
        .route("/events/types", get(events::event_types))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // Control-Proxy hinter require_auth.
    let control_routes = axum::Router::new()
        .route("/chaos", post(control::chaos))
        .route("/stimulus", post(control::stimulus))
        .route("/nightrun", post(control::nightrun))
        .route(
            "/config",
            get(control::get_config).patch(control::patch_config),
        )
        .route("/provider", post(control::provider))
        .route("/pause", post(control::pause))
        .route("/resume", post(control::resume))
        .route(
            "/agent-provider",
            post(control::agent_provider).delete(control::delete_agent_provider),
        )
        .route("/traffic-stats", get(control::traffic_stats))
        .route("/platform-state", get(control::platform_state))
        .route("/platform-analyses", get(control::platform_analyses))
        .route("/platform-analyze", post(control::platform_analyze))
        .route("/status", get(control::status))
        .route("/snapshots", get(control::snapshots))
        .route("/snapshot", post(control::snapshot))
        .route("/snapshot-state", get(control::snapshot_state))
        .route("/snapshot-restore", post(control::snapshot_restore))
        .route("/restore", post(control::snapshot_restore))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let operator_routes = axum::Router::new()
        .route("/chat", post(control::operator_chat))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // #420: Config Read+Write — EIGENE Gruppe (NICHT `/control/config`, das proxyt die Gateway-LLM-Config).
    // READ geparst (agents/rooms) + daemon.toml-Rohtext; WRITE validiert + proxyt an #425 (Daemon=Schreiber).
    let config_routes = axum::Router::new()
        .route("/agents", get(config::get_agents))
        .route("/rooms", get(config::get_rooms))
        .route("/daemon", get(config::get_daemon))
        .route("/apply", post(config::apply))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let api = axum::Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/status", get(auth::status))
        .route(
            "/wt-ticket",
            get(auth::wt_ticket).route_layer(middleware::from_fn_with_state(
                state.clone(),
                auth::require_auth,
            )),
        )
        // `/cert-hash` bleibt BEWUSST public: der Browser braucht den Cert-Hash VOR dem Login, um die
        // WebTransport-Verbindung via `serverCertificateHashes` zu pinnen (self-signed) — der Connect
        // (und damit das Holen des Auth-Tickets) laeuft sonst nie an. Liefert nur den oeffentlichen
        // Zertifikats-Hash, keine sensiblen Daten.
        .route("/cert-hash", get(cert_hash))
        .route("/health", get(health))
        .merge(read_routes)
        .nest("/control", control_routes)
        .nest("/operator", operator_routes)
        .nest("/config", config_routes);

    axum::Router::new()
        .nest("/api", api)
        .fallback_service(
            ServeDir::new(&state.config.bundle_dir).append_index_html_on_directories(true),
        )
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// GET /api/cert-hash — base64(sha-256(cert DER)) fuer WebTransport `serverCertificateHashes` (leer bei CA-Cert).
async fn cert_hash(
    axum::extract::State(st): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "hash": st.config.cert_hash_b64, "algorithm": "sha-256" }))
}

/// GET /api/health — public service liveness probe for deploy smoke/monitoring.
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "status": "ok", "service": "sentinel-dashboard-backend" }))
}
