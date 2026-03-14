//! Shared types and FlatBuffer schemas for Project Sentinel.
//!
//! This crate defines the common data structures used across all sentinel crates.
//! Serialization strategy:
//! - Internal (Zenoh Pub/Sub): FlatBuffers (zero-copy)
//! - External (Dashboard, Logs): MessagePack

pub mod agent_config;
pub mod components;
pub mod events;
pub mod feature_flags;
pub mod generated;
pub mod psi;
pub mod room;
pub mod types;

pub use events::{DomainEvent, DomainEventPayload};
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    // ── Newtype validation ──────────────────────

    #[test]
    fn test_agent_id_valid() {
        assert!(AgentId::new(1).is_ok());
        assert!(AgentId::new(54).is_ok());
        assert!(AgentId::new(27).is_ok());
    }

    #[test]
    fn test_agent_id_invalid() {
        assert!(AgentId::new(0).is_err());
        assert!(AgentId::new(55).is_err());
        assert!(AgentId::new(u16::MAX).is_err());
    }

    #[test]
    fn test_room_id_valid() {
        assert!(RoomId::new(1).is_ok());
        assert!(RoomId::new(15).is_ok());
    }

    #[test]
    fn test_room_id_invalid() {
        assert!(RoomId::new(0).is_err());
    }

    // ── Newtype Copy semantics ──────────────────

    #[test]
    fn test_newtypes_are_copy() {
        let agent = AgentId(1);
        let agent_copy = agent; // Copy, not move
        assert_eq!(agent, agent_copy);

        let room = RoomId(1);
        let room_copy = room;
        assert_eq!(room, room_copy);

        let tick = Tick(42);
        let tick_copy = tick;
        assert_eq!(tick, tick_copy);

        let ts = Timestamp(1000);
        let ts_copy = ts;
        assert_eq!(ts, ts_copy);
    }

    // ── Display implementations ─────────────────

    #[test]
    fn test_display_agent_id() {
        assert_eq!(format!("{}", AgentId(1)), "AGENT-01");
        assert_eq!(format!("{}", AgentId(54)), "AGENT-54");
    }

    #[test]
    fn test_display_room_id() {
        assert_eq!(format!("{}", RoomId(1)), "ROOM-1");
        assert_eq!(format!("{}", RoomId(15)), "ROOM-15");
    }

    #[test]
    fn test_display_tick() {
        assert_eq!(format!("{}", Tick(0)), "t0");
        assert_eq!(format!("{}", Tick(42)), "t42");
    }

    #[test]
    fn test_display_timestamp() {
        assert_eq!(format!("{}", Timestamp(0)), "0ms");
        assert_eq!(format!("{}", Timestamp(1000)), "1000ms");
    }

    // ── BioStateUpdate validation ───────────────

    #[test]
    fn test_bio_state_valid() {
        let bio = BioStateUpdate::new(
            AgentId(1),
            45.5,
            72.0,
            95.0,
            30.0,
            55.0,
            20.0,
            80.0,
            Timestamp(2000),
            Tick(100),
        );
        assert!(bio.is_ok());
        let bio = bio.unwrap();
        assert!((bio.hunger - 45.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_bio_state_boundary_valid() {
        // Exact boundaries: 0.0 and 100.0 are valid
        assert!(BioStateUpdate::new(
            AgentId(1),
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            Timestamp(0),
            Tick(0),
        )
        .is_ok());
        assert!(BioStateUpdate::new(
            AgentId(1),
            100.0,
            100.0,
            100.0,
            100.0,
            100.0,
            100.0,
            100.0,
            Timestamp(0),
            Tick(0),
        )
        .is_ok());
    }

    #[test]
    fn test_bio_state_invalid_negative() {
        let result = BioStateUpdate::new(
            AgentId(1),
            -1.0,
            72.0,
            95.0,
            30.0,
            55.0,
            20.0,
            80.0,
            Timestamp(2000),
            Tick(100),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_bio_state_invalid_over_100() {
        let result = BioStateUpdate::new(
            AgentId(1),
            45.5,
            101.0,
            95.0,
            30.0,
            55.0,
            20.0,
            80.0,
            Timestamp(2000),
            Tick(100),
        );
        assert!(result.is_err());
    }

    // ── MoodUpdate validation ───────────────────

    #[test]
    fn test_mood_update_valid() {
        let mood = MoodUpdate::new(
            AgentId(1),
            0.5,
            0.7,
            Emotion::Happy,
            Timestamp(3000),
            Tick(150),
        );
        assert!(mood.is_ok());
    }

    #[test]
    fn test_mood_update_boundary_valid() {
        assert!(MoodUpdate::new(
            AgentId(1),
            -1.0,
            0.0,
            Emotion::Neutral,
            Timestamp(0),
            Tick(0),
        )
        .is_ok());
        assert!(MoodUpdate::new(
            AgentId(1),
            1.0,
            1.0,
            Emotion::Excited,
            Timestamp(0),
            Tick(0),
        )
        .is_ok());
    }

    #[test]
    fn test_mood_update_invalid_valence() {
        assert!(MoodUpdate::new(
            AgentId(1),
            -1.1,
            0.5,
            Emotion::Neutral,
            Timestamp(0),
            Tick(0),
        )
        .is_err());
        assert!(MoodUpdate::new(
            AgentId(1),
            1.1,
            0.5,
            Emotion::Neutral,
            Timestamp(0),
            Tick(0),
        )
        .is_err());
    }

    #[test]
    fn test_mood_update_invalid_arousal() {
        assert!(MoodUpdate::new(
            AgentId(1),
            0.5,
            -0.1,
            Emotion::Neutral,
            Timestamp(0),
            Tick(0),
        )
        .is_err());
        assert!(MoodUpdate::new(
            AgentId(1),
            0.5,
            1.1,
            Emotion::Neutral,
            Timestamp(0),
            Tick(0),
        )
        .is_err());
    }

    // ── Serialization ───────────────────────────

    #[test]
    fn test_agent_action_serialization() {
        let action = AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::Chat,
            target_room: Some("konferenz-1".to_string()),
            target_agent: Some(AgentId(5)),
            content: Some("Guten Morgen!".to_string()),
            timestamp: Timestamp(1000),
            tick: Tick(42),
        };
        let json = serde_json::to_string(&action).unwrap();
        let deserialized: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.agent_id, AgentId(1));
        assert_eq!(deserialized.action_type, ActionType::Chat);
        assert_eq!(deserialized.target_room, Some("konferenz-1".to_string()));
    }

    #[test]
    fn test_chaos_event_serialization() {
        let event = ChaosEvent {
            event_type: EventType::PrinterBroken,
            target_room: Some(RoomId(5)),
            target_agent: None,
            description: "Drucker zeigt Papierstau an".to_string(),
            duration_minutes: Some(30),
            timestamp: Timestamp(5000),
            tick: Tick(200),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: ChaosEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type, EventType::PrinterBroken);
        assert!(deserialized.target_agent.is_none());
        assert_eq!(deserialized.target_room, Some(RoomId(5)));
    }
}
