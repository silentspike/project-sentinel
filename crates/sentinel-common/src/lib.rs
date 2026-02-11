//! Shared types and FlatBuffer schemas for Project Sentinel.
//!
//! This crate defines the common data structures used across all sentinel crates.
//! Serialization strategy:
//! - Internal (Zenoh Pub/Sub): FlatBuffers (zero-copy)
//! - External (Dashboard, Logs): MessagePack

pub mod types;

pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_action_serialization() {
        let action = AgentAction {
            agent_name: "Thomas Schmidt".to_string(),
            action_type: ActionType::Chat,
            target_room: Some("kueche".to_string()),
            target_agent: Some("Lisa Weber".to_string()),
            content: Some("Guten Morgen!".to_string()),
            timestamp_ms: 1000,
            tick: 42,
        };
        let json = serde_json::to_string(&action).unwrap();
        let deserialized: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.agent_name, "Thomas Schmidt");
        assert_eq!(deserialized.action_type, ActionType::Chat);
    }

    #[test]
    fn test_perception_default() {
        let p = Perception::default();
        assert_eq!(p.agent_name, "");
        assert_eq!(p.body_text, "");
    }

    #[test]
    fn test_bio_state_update_serialization() {
        let bio = BioStateUpdate {
            agent_name: "Andreas Mueller".to_string(),
            hunger: 45.5,
            energy: 72.0,
            caffeine_mg: 95.0,
            bladder: 30.0,
            stress: 55.0,
            social_need: 20.0,
            comfort: 80.0,
            timestamp_ms: 2000,
            tick: 100,
        };
        let json = serde_json::to_string(&bio).unwrap();
        let deserialized: BioStateUpdate = serde_json::from_str(&json).unwrap();
        assert!((deserialized.hunger - 45.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_chaos_event_types() {
        let event = ChaosEvent {
            event_type: EventType::PrinterBroken,
            target_room: Some("buero-dev-1".to_string()),
            target_agent: None,
            description: "Drucker zeigt Papierstau an".to_string(),
            duration_minutes: Some(30),
            timestamp_ms: 5000,
            tick: 200,
        };
        assert_eq!(event.event_type, EventType::PrinterBroken);
        assert!(event.target_agent.is_none());
    }
}
