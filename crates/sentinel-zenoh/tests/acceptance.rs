//! Acceptance tests for sentinel-zenoh (Issue #6).

use sentinel_zenoh::topics;

/// AC 6.2: Topic string format is correct for all TopicType variants
#[test]
fn ac_06_02_topic_generation() {
    // AC 6.2: Verify topic string format for all topic functions

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
}

/// AC 6.4: SentinelBus API komplett - new(), publish(), subscribe() existieren
///
/// Compile-time Signatur-Check: Verifiziert dass SentinelBus, BusSubscriber
/// oeffentliche Typen sind und new/publish/subscribe die korrekten Signaturen haben.
/// Runtime-Aufruf benoetigt Zenoh-Router und ist separat via #[ignore] getestet.
#[test]
fn ac_06_04_sentinelbus_api_exists() {
    // AC 6.4: SentinelBus ist ein oeffentlicher, klonbarer Typ
    fn _check_type_is_public_and_clone<T: Clone>() {}
    _check_type_is_public_and_clone::<sentinel_zenoh::SentinelBus>();

    // BusSubscriber ist ein oeffentlicher Typ
    fn _check_subscriber_public(_: &sentinel_zenoh::BusSubscriber) {}

    // Signaturen kompilieren korrekt (async fn → Future<Output = Result<_>>)
    // new() → Result<SentinelBus>
    async fn _sig_new() -> anyhow::Result<sentinel_zenoh::SentinelBus> {
        sentinel_zenoh::SentinelBus::new().await
    }
    // publish() → Result<()>
    async fn _sig_publish(bus: &sentinel_zenoh::SentinelBus) -> anyhow::Result<()> {
        bus.publish("t", b"p").await
    }
    // subscribe() → Result<BusSubscriber>
    async fn _sig_subscribe(
        bus: &sentinel_zenoh::SentinelBus,
    ) -> anyhow::Result<sentinel_zenoh::BusSubscriber> {
        bus.subscribe("t").await
    }
}
