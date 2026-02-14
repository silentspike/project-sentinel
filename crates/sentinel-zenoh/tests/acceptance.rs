//! Acceptance tests for sentinel-zenoh (Issue #6).

use sentinel_zenoh::config::BusConfig;
use sentinel_zenoh::inflight::InFlightTracker;
use sentinel_zenoh::query::{QueryResponse, QueryScope, ScopedQuery};
use sentinel_zenoh::topics;
use sentinel_zenoh::{SentinelBus, TransportMode};
use uuid::Uuid;

/// AC 6.2: Topic string format is correct for all TopicType variants
#[test]
fn ac_06_02_topic_generation() {
    // Agent topics
    assert_eq!(
        topics::agent_action("thomas"),
        "sentinel/agent/thomas/action"
    );
    assert_eq!(
        topics::agent_perception("lisa"),
        "sentinel/agent/lisa/perception"
    );
    assert_eq!(
        topics::agent_state("andreas"),
        "sentinel/agent/andreas/state"
    );

    // Room topics
    assert_eq!(topics::room_audio("kueche"), "sentinel/room/kueche/audio");
    assert_eq!(topics::room_smell("lobby"), "sentinel/room/lobby/smell");
    assert_eq!(
        topics::room_presence("grossraum"),
        "sentinel/room/grossraum/presence"
    );

    // Physics tick
    assert_eq!(topics::physics_tick(0), "sentinel/physics/tick/0");
    assert_eq!(topics::physics_tick(42), "sentinel/physics/tick/42");

    // Cortex inject
    assert_eq!(
        topics::cortex_inject("thomas"),
        "sentinel/cortex/inject/thomas"
    );

    // Constants
    assert_eq!(topics::CHAOS_EVENT, "sentinel/chaos/event");
    assert_eq!(topics::JUDGE_ALERT, "sentinel/judge/alert");
    assert_eq!(topics::MODEL_SWAP, "sentinel/meta/model-swap");
    assert_eq!(topics::PREFIX, "sentinel");

    // Query topics (neu)
    assert_eq!(
        topics::query_request_agent("thomas"),
        "sentinel/query/agent/thomas/request"
    );
    assert_eq!(
        topics::query_request_room("kueche"),
        "sentinel/query/room/kueche/request"
    );
    assert_eq!(
        topics::QUERY_REQUEST_GLOBAL,
        "sentinel/query/global/request"
    );
    assert_eq!(
        topics::query_response_agent("thomas"),
        "sentinel/query/response/thomas"
    );
}

/// AC 6.4: SentinelBus API komplett - new(), publish(), subscribe(), scoped_query() existieren
#[test]
fn ac_06_04_sentinelbus_api_exists() {
    // SentinelBus ist ein oeffentlicher, klonbarer Typ
    fn _check_type_is_public_and_clone<T: Clone>() {}
    _check_type_is_public_and_clone::<SentinelBus>();

    // BusSubscriber ist ein oeffentlicher Typ
    fn _check_subscriber_public(_: &sentinel_zenoh::BusSubscriber) {}

    // TransportMode ist oeffentlich und vergleichbar
    fn _check_transport_mode() {
        let _: TransportMode = TransportMode::Shm;
        let _: TransportMode = TransportMode::Network;
        assert_ne!(TransportMode::Shm, TransportMode::Network);
    }
    _check_transport_mode();

    // Signaturen kompilieren korrekt
    async fn _sig_new() -> anyhow::Result<SentinelBus> {
        SentinelBus::new().await
    }
    async fn _sig_with_config() -> anyhow::Result<SentinelBus> {
        SentinelBus::with_config(BusConfig::default()).await
    }
    async fn _sig_publish(bus: &SentinelBus) -> anyhow::Result<()> {
        bus.publish("t", b"p").await
    }
    async fn _sig_subscribe(bus: &SentinelBus) -> anyhow::Result<sentinel_zenoh::BusSubscriber> {
        bus.subscribe("t").await
    }
    async fn _sig_scoped_query(bus: &SentinelBus) -> anyhow::Result<Option<QueryResponse>> {
        let q = ScopedQuery::new(
            sentinel_common::AgentId(1),
            QueryScope::Global,
            vec![],
            sentinel_common::Tick(0),
            100,
            0,
        );
        bus.scoped_query(q).await
    }

    // Suppress unused warnings
    let _ = _sig_new;
    let _ = _sig_with_config;
    let _ = _sig_publish;
    let _ = _sig_subscribe;
    let _ = _sig_scoped_query;
}

/// AC 6.2 (SHM Fallback): Default transport ist Network wenn SHM nicht konfiguriert
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac_06_02_shm_fallback_default_network() {
    let config = BusConfig {
        shm_enabled: false,
        ..BusConfig::default()
    };
    let bus = SentinelBus::with_config(config)
        .await
        .expect("Bus should open in network mode");
    assert_eq!(bus.transport_mode(), TransportMode::Network);
}

/// AC 6.4 (InFlight): Globales Limit von 128 wird erzwungen
#[tokio::test]
async fn ac_06_04_inflight_global_cap() {
    let tracker = InFlightTracker::new(128, 100);
    let mut guards = Vec::new();

    // 128 Slots belegen (jeder Agent mit eigenem Slot)
    for i in 0..128u16 {
        let guard = tracker
            .try_acquire(Uuid::now_v7(), i, 0)
            .await
            .expect("should acquire within global cap");
        guards.push(guard);
    }

    // 129. muss fehlschlagen
    let result = tracker.try_acquire(Uuid::now_v7(), 200, 0).await;
    assert!(result.is_err(), "Should fail when global cap exceeded");
}

/// AC 6.4 (InFlight): Per-Agent Limit von 8 wird erzwungen
#[tokio::test]
async fn ac_06_04_inflight_per_agent_cap() {
    let tracker = InFlightTracker::new(1000, 8);
    let mut guards = Vec::new();

    // 8 Slots fuer Agent 1
    for _ in 0..8 {
        let guard = tracker
            .try_acquire(Uuid::now_v7(), 1, 0)
            .await
            .expect("should acquire within per-agent cap");
        guards.push(guard);
    }

    // 9. fuer Agent 1 muss fehlschlagen
    let result = tracker.try_acquire(Uuid::now_v7(), 1, 0).await;
    assert!(result.is_err(), "Should fail when per-agent cap exceeded");

    // Agent 2 hat eigene Limits, sollte funktionieren
    let guard = tracker
        .try_acquire(Uuid::now_v7(), 2, 0)
        .await
        .expect("other agent should have separate limit");
    guards.push(guard);
}

/// AC 6.3 (Stale Response): QueryResponse mit altem Tick wird als stale erkannt
#[test]
fn ac_06_03_stale_response_detection() {
    let response = QueryResponse {
        query_id: Uuid::now_v7(),
        response_tick: 10,
        payload: vec![],
    };

    // response_tick(10) < min_tick(20) = stale
    assert!(response.is_stale(20));

    // response_tick(10) >= min_tick(10) = nicht stale
    assert!(!response.is_stale(10));

    // response_tick(10) >= min_tick(5) = nicht stale
    assert!(!response.is_stale(5));
}

/// AC 6.5 (FlatBuffer roundtrip): Query-Payload bleibt durch Serialisierung intakt
#[test]
fn ac_06_05_payload_roundtrip() {
    // Simuliere FlatBuffer-Payload (beliebige Bytes)
    let original_payload: Vec<u8> = (0..255).collect();

    let query = ScopedQuery::new(
        sentinel_common::AgentId(1),
        QueryScope::Agent(sentinel_common::AgentId(2)),
        original_payload.clone(),
        sentinel_common::Tick(42),
        100,
        0,
    );

    // JSON Roundtrip (wie in der scoped_query Methode)
    let json = serde_json::to_vec(&query).unwrap();
    let deserialized: ScopedQuery = serde_json::from_slice(&json).unwrap();

    assert_eq!(deserialized.payload, original_payload);
    assert_eq!(deserialized.origin_agent, sentinel_common::AgentId(1));
    assert_eq!(deserialized.tick, sentinel_common::Tick(42));
}

/// BusConfig Defaults stimmen mit Issue-Anforderungen ueberein
#[test]
fn config_defaults_match_issue_requirements() {
    let config = BusConfig::default();
    assert!(!config.shm_enabled);
    assert_eq!(config.shm_p99_target_us, 200);
    assert_eq!(config.query_deadline_ms, 100);
    assert!(config.query_cancel_enabled);
    assert_eq!(config.max_inflight_global, 128);
    assert_eq!(config.max_inflight_per_agent, 8);
}
