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
pub async fn zenoh_fanout_task(bus: SentinelBus, mut rx: tokio::sync::mpsc::Receiver<DomainEvent>) {
    let mut publish_count: u64 = 0;
    let mut error_count: u64 = 0;

    while let Some(event) = rx.recv().await {
        let topic = match fanout_topic(&event) {
            Some(t) => t,
            None => continue, // Event-Typ nicht fuer Fan-Out vorgesehen
        };

        // FlatBuffer encoding if schema available, JSON fallback otherwise
        let payload = match sentinel_zenoh::flatbuf::encode_domain_event(&event) {
            Some(fb_bytes) => fb_bytes,
            None => match serde_json::to_vec(&event) {
                Ok(json) => json,
                Err(e) => {
                    warn!(error = %e, event_type = %event.event_type, "Zenoh fanout serialization failed");
                    continue;
                }
            },
        };

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
    // event_type ist snake_case (aus DomainEventPayload::event_type_str())
    match event.event_type.as_str() {
        // Room Events
        "room_physics_updated" => Some(sentinel_zenoh::topics::room_audio(&event.aggregate_id)),
        "smell_event_triggered" => Some(sentinel_zenoh::topics::room_smell(&event.aggregate_id)),
        "transit_completed" => Some(sentinel_zenoh::topics::room_presence(&event.aggregate_id)),

        // Agent State Events
        "agent_spawned" | "agent_despawned" | "bio_state_updated" | "agent_status_changed" => {
            Some(sentinel_zenoh::topics::agent_state(&event.aggregate_id))
        }

        // Agent Actions
        "agent_action_received" => Some(sentinel_zenoh::topics::agent_action(&event.aggregate_id)),

        // Chaos Events
        "chaos_triggered" => Some(sentinel_zenoh::topics::CHAOS_EVENT.to_string()),

        // Tick Snapshots
        "tick_snapshot" => {
            // tick Feld aus dem Event extrahieren (aggregate_id ist "simulation")
            // Verwende eine stabile Topic-ID statt den Tick-Wert
            Some(sentinel_zenoh::topics::physics_tick(0))
        }

        // Transit Events
        "transit_started" => Some(sentinel_zenoh::topics::room_presence(&event.aggregate_id)),

        // Shift Events
        "shift_transition_completed" => Some(format!(
            "{}/shift/transition",
            sentinel_zenoh::topics::PREFIX
        )),

        // Nightrun/Judge/Consolidation — NICHT auf Zenoh (Limbo-only, Determinismus)
        "nightrun_started"
        | "nightrun_completed"
        | "agent_consolidated"
        | "agent_consolidation_failed" => None,

        // Judge Alerts — bereits ueber NATS verteilt
        "judge_alert_received" => None,

        // Bio Actions, Hallway Encounters
        "bio_action_performed" => Some(sentinel_zenoh::topics::agent_state(&event.aggregate_id)),
        "hallway_encounter_detected" => {
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
        let event = test_event("room_physics_updated", "kueche-eg");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/room/kueche-eg/audio".to_string())
        );

        let event = test_event("smell_event_triggered", "kueche-eg");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/room/kueche-eg/smell".to_string())
        );
    }

    #[test]
    fn test_fanout_topic_agent_events() {
        let event = test_event("agent_spawned", "AGENT-01");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/agent/AGENT-01/state".to_string())
        );

        let event = test_event("bio_state_updated", "AGENT-05");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/agent/AGENT-05/state".to_string())
        );
    }

    #[test]
    fn test_fanout_topic_chaos() {
        let event = test_event("chaos_triggered", "buero-dev-1");
        assert_eq!(
            fanout_topic(&event),
            Some("sentinel/chaos/event".to_string())
        );
    }

    #[test]
    fn test_fanout_topic_tick_snapshot() {
        let event = test_event("tick_snapshot", "simulation");
        assert!(fanout_topic(&event).is_some());
    }

    #[test]
    fn test_fanout_topic_nightrun_excluded() {
        assert!(fanout_topic(&test_event("nightrun_started", "run-1")).is_none());
        assert!(fanout_topic(&test_event("nightrun_completed", "run-1")).is_none());
        assert!(fanout_topic(&test_event("agent_consolidated", "run-1")).is_none());
    }

    #[test]
    fn test_fanout_topic_judge_excluded() {
        assert!(fanout_topic(&test_event("judge_alert_received", "AGENT-01")).is_none());
    }

    #[test]
    fn test_fanout_topic_unknown_excluded() {
        assert!(fanout_topic(&test_event("SomeUnknownEvent", "x")).is_none());
    }
}
