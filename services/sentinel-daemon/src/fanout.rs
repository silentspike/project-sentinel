//! Zenoh Event Fan-Out Bridge.
//!
//! Empfaengt DomainEvents aus dem ECS-Thread via tokio::sync::mpsc Channel
//! und publiziert sie auf Zenoh Topics fuer 1:n Real-Time-Verteilung.
//!
//! Limbo bleibt SSOT — Zenoh ist best-effort Fan-Out. Wenn Zenoh ausfaellt
//! oder der Channel voll ist, gehen keine Events verloren.

use sentinel_common::events::DomainEvent;
use sentinel_zenoh::SentinelBus;
use tracing::warn;

/// Async Task: Empfaengt Events via Channel und publiziert auf Zenoh Topics.
///
/// Laeuft im tokio Runtime. Beendet sich automatisch wenn der Sender
/// (im ECS-Thread) gedroppt wird.
pub async fn zenoh_fanout_task(
    bus: SentinelBus,
    mut rx: tokio::sync::mpsc::Receiver<DomainEvent>,
) {
    let mut publish_count: u64 = 0;
    let mut error_count: u64 = 0;

    while let Some(event) = rx.recv().await {
        let topic = match fanout_topic(&event) {
            Some(t) => t,
            None => continue, // Event-Typ nicht fuer Fan-Out vorgesehen
        };

        match serde_json::to_vec(&event) {
            Ok(payload) => {
                if let Err(e) = bus.publish(&topic, &payload).await {
                    error_count += 1;
                    if error_count <= 10 || error_count.is_power_of_two() {
                        warn!(
                            error = %e,
                            topic,
                            error_count,
                            "Zenoh fanout publish failed"
                        );
                    }
                } else {
                    publish_count += 1;
                    if publish_count.is_power_of_two() && publish_count <= 1024 {
                        tracing::debug!(publish_count, topic, "Zenoh fanout milestone");
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, event_type = %event.event_type, "Zenoh fanout serialization failed");
            }
        }
    }

    tracing::info!(
        publish_count,
        error_count,
        "Zenoh fanout task beendet (Sender dropped)"
    );
}

/// Bestimmt das Zenoh-Topic fuer ein Event. Gibt `None` zurueck wenn das
/// Event nicht auf Zenoh publiziert werden soll.
///
/// Nutzt die bestehenden Topic-Funktionen aus sentinel_zenoh::topics.
fn fanout_topic(event: &DomainEvent) -> Option<String> {
    match event.event_type.as_str() {
        // Room Events
        "RoomPhysicsUpdated" => Some(sentinel_zenoh::topics::room_audio(&event.aggregate_id)),
        "SmellEventTriggered" => Some(sentinel_zenoh::topics::room_smell(&event.aggregate_id)),
        "TransitCompleted" => Some(sentinel_zenoh::topics::room_presence(&event.aggregate_id)),

        // Agent State Events
        "AgentSpawned" | "AgentDespawned" | "BioStateUpdated" | "AgentStatusChanged" => {
            Some(sentinel_zenoh::topics::agent_state(&event.aggregate_id))
        }

        // Agent Actions
        "AgentActionReceived" => Some(sentinel_zenoh::topics::agent_action(&event.aggregate_id)),

        // Chaos Events
        "ChaosTriggered" => Some(sentinel_zenoh::topics::CHAOS_EVENT.to_string()),

        // Tick Snapshots
        "TickSnapshot" => {
            // tick Feld aus dem Event extrahieren (aggregate_id ist "simulation")
            // Verwende eine stabile Topic-ID statt den Tick-Wert
            Some(sentinel_zenoh::topics::physics_tick(0))
        }

        // Transit Events
        "TransitStarted" => Some(sentinel_zenoh::topics::room_presence(&event.aggregate_id)),

        // Shift Events
        "ShiftTransitionCompleted" => {
            Some(format!("{}/shift/transition", sentinel_zenoh::topics::PREFIX))
        }

        // Nightrun/Judge/Consolidation — NICHT auf Zenoh (Limbo-only, Determinismus)
        "NightRunStarted" | "NightRunCompleted" | "AgentConsolidated"
        | "AgentConsolidationFailed" => None,

        // Judge Alerts — bereits ueber NATS verteilt
        "JudgeAlertReceived" => None,

        // Bio Actions, Hallway Encounters
        "BioActionPerformed" => Some(sentinel_zenoh::topics::agent_state(&event.aggregate_id)),
        "HallwayEncounterDetected" => {
            Some(sentinel_zenoh::topics::room_presence(&event.aggregate_id))
        }

        // Unbekannte Events — nicht publishen
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::events::DomainEvent;

    fn test_event(event_type: &str, aggregate_id: &str) -> DomainEvent {
        DomainEvent::new(event_type, aggregate_id, "{}", "test-op-1", 42)
    }

    #[test]
    fn test_fanout_topic_room_events() {
        let event = test_event("RoomPhysicsUpdated", "kueche-eg");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/room/kueche-eg/audio".to_string())
        );

        let event = test_event("SmellEventTriggered", "kueche-eg");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/room/kueche-eg/smell".to_string())
        );
    }

    #[test]
    fn test_fanout_topic_agent_events() {
        let event = test_event("AgentSpawned", "AGENT-01");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/agent/AGENT-01/state".to_string())
        );

        let event = test_event("BioStateUpdated", "AGENT-05");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/agent/AGENT-05/state".to_string())
        );
    }

    #[test]
    fn test_fanout_topic_chaos() {
        let event = test_event("ChaosTriggered", "buero-dev-1");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/chaos/event".to_string())
        );
    }

    #[test]
    fn test_fanout_topic_tick_snapshot() {
        let event = test_event("TickSnapshot", "simulation");
        assert!(fanout_topic(&event).is_some());
    }

    #[test]
    fn test_fanout_topic_nightrun_excluded() {
        assert!(fanout_topic(&test_event("NightRunStarted", "run-1")).is_none());
        assert!(fanout_topic(&test_event("NightRunCompleted", "run-1")).is_none());
        assert!(fanout_topic(&test_event("AgentConsolidated", "run-1")).is_none());
    }

    #[test]
    fn test_fanout_topic_judge_excluded() {
        assert!(fanout_topic(&test_event("JudgeAlertReceived", "AGENT-01")).is_none());
    }

    #[test]
    fn test_fanout_topic_unknown_excluded() {
        assert!(fanout_topic(&test_event("SomeUnknownEvent", "x")).is_none());
    }
}
