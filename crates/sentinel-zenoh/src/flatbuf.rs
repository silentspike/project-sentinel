//! FlatBuffer encode/decode helpers for Zenoh zero-copy payloads.
//!
//! Provides bidirectional conversion between sentinel_common domain types
//! and FlatBuffer binary format for SHM transport.
//!
//! Content-type detection: first byte `0x00` = FlatBuffer, `0x7B` (`{`) = JSON.
//! This enables auto-detection without topic changes (backwards-compatible).

use anyhow::{bail, Context, Result};
use flatbuffers::FlatBufferBuilder;
use sentinel_common::events::DomainEvent;
use sentinel_common::generated as fb;
use sentinel_common::{
    ActionType, AgentAction, AgentId, BioStateUpdate, ChaosEvent, Emotion, EventType, MoodUpdate,
    Perception, PositionUpdate, RoomId, Tick, Timestamp,
};

/// Marker byte prepended to all FlatBuffer payloads.
/// JSON payloads start with `{` (0x7B), so 0x00 is unambiguous.
pub const FB_MARKER: u8 = 0x00;

/// Check if a payload uses FlatBuffer encoding.
#[inline]
pub fn is_flatbuffer(bytes: &[u8]) -> bool {
    bytes.first() == Some(&FB_MARKER)
}

/// Strip the marker byte, returning the raw FlatBuffer data.
fn strip_marker(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.first() != Some(&FB_MARKER) {
        bail!("payload does not start with FlatBuffer marker byte 0x00");
    }
    Ok(&bytes[1..])
}

/// Prepend the marker byte to raw FlatBuffer data.
fn prepend_marker(fb_data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(fb_data.len() + 1);
    out.push(FB_MARKER);
    out.extend_from_slice(fb_data);
    out
}

// ── Enum Conversions ──────────────────────────────────────────

fn action_type_to_fb(at: &ActionType) -> fb::ActionType {
    match at {
        ActionType::Chat => fb::ActionType::Chat,
        ActionType::Move => fb::ActionType::Move,
        ActionType::ToolUse => fb::ActionType::ToolUse,
        ActionType::Emote => fb::ActionType::Emote,
        ActionType::PhoneCall => fb::ActionType::PhoneCall,
    }
}

fn fb_to_action_type(at: fb::ActionType) -> ActionType {
    if at == fb::ActionType::Move {
        ActionType::Move
    } else if at == fb::ActionType::ToolUse {
        ActionType::ToolUse
    } else if at == fb::ActionType::Emote {
        ActionType::Emote
    } else if at == fb::ActionType::PhoneCall {
        ActionType::PhoneCall
    } else {
        ActionType::Chat
    }
}

fn event_type_to_fb(et: &EventType) -> fb::EventType {
    match et {
        EventType::PhoneRing => fb::EventType::PhoneRing,
        EventType::PrinterBroken => fb::EventType::PrinterBroken,
        EventType::PackageDelivery => fb::EventType::PackageDelivery,
        EventType::SBahnDelay => fb::EventType::SBahnDelay,
        EventType::FireAlarmDrill => fb::EventType::FireAlarmDrill,
        EventType::CakeInKitchen => fb::EventType::CakeInKitchen,
        EventType::AirConBroken => fb::EventType::AirConBroken,
        EventType::InternetOutage => fb::EventType::InternetOutage,
    }
}

fn fb_to_event_type(et: fb::EventType) -> EventType {
    if et == fb::EventType::PrinterBroken {
        EventType::PrinterBroken
    } else if et == fb::EventType::PackageDelivery {
        EventType::PackageDelivery
    } else if et == fb::EventType::SBahnDelay {
        EventType::SBahnDelay
    } else if et == fb::EventType::FireAlarmDrill {
        EventType::FireAlarmDrill
    } else if et == fb::EventType::CakeInKitchen {
        EventType::CakeInKitchen
    } else if et == fb::EventType::AirConBroken {
        EventType::AirConBroken
    } else if et == fb::EventType::InternetOutage {
        EventType::InternetOutage
    } else {
        EventType::PhoneRing
    }
}

fn emotion_to_fb(em: &Emotion) -> fb::Emotion {
    match em {
        Emotion::Neutral => fb::Emotion::Neutral,
        Emotion::Happy => fb::Emotion::Happy,
        Emotion::Frustrated => fb::Emotion::Frustrated,
        Emotion::Stressed => fb::Emotion::Stressed,
        Emotion::Relaxed => fb::Emotion::Relaxed,
        Emotion::Excited => fb::Emotion::Excited,
        Emotion::Bored => fb::Emotion::Bored,
        Emotion::Anxious => fb::Emotion::Anxious,
        Emotion::Focused => fb::Emotion::Focused,
        Emotion::Tired => fb::Emotion::Tired,
    }
}

fn fb_to_emotion(em: fb::Emotion) -> Emotion {
    if em == fb::Emotion::Happy {
        Emotion::Happy
    } else if em == fb::Emotion::Frustrated {
        Emotion::Frustrated
    } else if em == fb::Emotion::Stressed {
        Emotion::Stressed
    } else if em == fb::Emotion::Relaxed {
        Emotion::Relaxed
    } else if em == fb::Emotion::Excited {
        Emotion::Excited
    } else if em == fb::Emotion::Bored {
        Emotion::Bored
    } else if em == fb::Emotion::Anxious {
        Emotion::Anxious
    } else if em == fb::Emotion::Focused {
        Emotion::Focused
    } else if em == fb::Emotion::Tired {
        Emotion::Tired
    } else {
        Emotion::Neutral
    }
}

fn action_type_from_str(s: &str) -> Option<ActionType> {
    match s {
        "Chat" => Some(ActionType::Chat),
        "Move" => Some(ActionType::Move),
        "ToolUse" => Some(ActionType::ToolUse),
        "Emote" => Some(ActionType::Emote),
        "PhoneCall" => Some(ActionType::PhoneCall),
        _ => None,
    }
}

fn event_type_from_str(s: &str) -> Option<EventType> {
    match s {
        "PhoneRing" => Some(EventType::PhoneRing),
        "PrinterBroken" => Some(EventType::PrinterBroken),
        "PackageDelivery" => Some(EventType::PackageDelivery),
        "SBahnDelay" => Some(EventType::SBahnDelay),
        "FireAlarmDrill" => Some(EventType::FireAlarmDrill),
        "CakeInKitchen" => Some(EventType::CakeInKitchen),
        "AirConBroken" => Some(EventType::AirConBroken),
        "InternetOutage" => Some(EventType::InternetOutage),
        _ => None,
    }
}

// ── Encode: Domain Types → FlatBuffer bytes ───────────────────

/// Encode a BioStateUpdate as FlatBuffer bytes (with marker prefix).
pub fn encode_bio_state(bio: &BioStateUpdate) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(128);
    let args = fb::BioStateUpdateArgs {
        agent_id: bio.agent_id.0,
        hunger: bio.hunger,
        energy: bio.energy,
        caffeine_mg: bio.caffeine_mg,
        bladder: bio.bladder,
        stress: bio.stress,
        social_need: bio.social_need,
        comfort: bio.comfort,
        timestamp: bio.timestamp.0,
        tick: bio.tick.0,
    };
    let offset = fb::BioStateUpdate::create(&mut builder, &args);
    fb::finish_bio_state_update_buffer(&mut builder, offset);
    prepend_marker(builder.finished_data())
}

/// Encode an AgentAction as FlatBuffer bytes (with marker prefix).
///
/// Note: `target_room` (String in domain type) is not preserved in FlatBuffer
/// (u16 in schema). The room name is available from the JSON event store.
pub fn encode_agent_action(action: &AgentAction) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(256);
    let content = action.content.as_deref().map(|s| builder.create_string(s));
    let args = fb::AgentActionArgs {
        agent_id: action.agent_id.0,
        action_type: action_type_to_fb(&action.action_type),
        target_room: 0,
        target_agent: action.target_agent.map_or(0, |a| a.0),
        content,
        timestamp: action.timestamp.0,
        tick: action.tick.0,
    };
    let offset = fb::AgentAction::create(&mut builder, &args);
    fb::finish_agent_action_buffer(&mut builder, offset);
    prepend_marker(builder.finished_data())
}

/// Encode a ChaosEvent as FlatBuffer bytes (with marker prefix).
pub fn encode_chaos_event(chaos: &ChaosEvent) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(256);
    let description = builder.create_string(&chaos.description);
    let args = fb::ChaosEventArgs {
        event_type: event_type_to_fb(&chaos.event_type),
        target_room: chaos.target_room.map_or(0, |r| r.0),
        target_agent: chaos.target_agent.map_or(0, |a| a.0),
        description: Some(description),
        duration_minutes: chaos.duration_minutes.unwrap_or(0),
        timestamp: chaos.timestamp.0,
        tick: chaos.tick.0,
    };
    let offset = fb::ChaosEvent::create(&mut builder, &args);
    fb::finish_chaos_event_buffer(&mut builder, offset);
    prepend_marker(builder.finished_data())
}

/// Encode a Perception as FlatBuffer bytes (with marker prefix).
pub fn encode_perception(perception: &Perception) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(1024);
    let circadian_text = builder.create_string(&perception.circadian_text);
    let body_text = builder.create_string(&perception.body_text);
    let environment_text = builder.create_string(&perception.environment_text);
    let acoustic_text = builder.create_string(&perception.acoustic_text);
    let presence_text = builder.create_string(&perception.presence_text);
    let impulse_text = builder.create_string(&perception.impulse_text);
    let args = fb::PerceptionArgs {
        agent_id: perception.agent_id.0,
        circadian_text: Some(circadian_text),
        body_text: Some(body_text),
        environment_text: Some(environment_text),
        acoustic_text: Some(acoustic_text),
        presence_text: Some(presence_text),
        impulse_text: Some(impulse_text),
        timestamp: perception.timestamp.0,
        tick: perception.tick.0,
    };
    let offset = fb::Perception::create(&mut builder, &args);
    fb::finish_perception_buffer(&mut builder, offset);
    prepend_marker(builder.finished_data())
}

/// Encode a MoodUpdate as FlatBuffer bytes (with marker prefix).
pub fn encode_mood_update(mood: &MoodUpdate) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(64);
    let args = fb::MoodUpdateArgs {
        agent_id: mood.agent_id.0,
        valence: mood.valence,
        arousal: mood.arousal,
        dominant_emotion: emotion_to_fb(&mood.dominant_emotion),
        timestamp: mood.timestamp.0,
        tick: mood.tick.0,
    };
    let offset = fb::MoodUpdate::create(&mut builder, &args);
    builder.finish(offset, None);
    prepend_marker(builder.finished_data())
}

/// Encode a PositionUpdate as FlatBuffer bytes (with marker prefix).
pub fn encode_position_update(pos: &PositionUpdate) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(64);
    let args = fb::PositionUpdateArgs {
        agent_id: pos.agent_id.0,
        room_id: pos.room_id.0,
        in_transit: pos.in_transit,
        transit_target: pos.transit_target.map_or(0, |r| r.0),
        timestamp: pos.timestamp.0,
        tick: pos.tick.0,
    };
    let offset = fb::PositionUpdate::create(&mut builder, &args);
    builder.finish(offset, None);
    prepend_marker(builder.finished_data())
}

// ── Decode: FlatBuffer bytes → Domain Types ───────────────────

/// Decode FlatBuffer bytes (with marker prefix) into a BioStateUpdate.
pub fn decode_bio_state(bytes: &[u8]) -> Result<BioStateUpdate> {
    let fb_bytes = strip_marker(bytes)?;
    let bio =
        fb::root_as_bio_state_update(fb_bytes).context("invalid FlatBuffer: BioStateUpdate")?;
    Ok(BioStateUpdate {
        agent_id: AgentId(bio.agent_id()),
        hunger: bio.hunger(),
        energy: bio.energy(),
        caffeine_mg: bio.caffeine_mg(),
        bladder: bio.bladder(),
        stress: bio.stress(),
        social_need: bio.social_need(),
        comfort: bio.comfort(),
        timestamp: Timestamp(bio.timestamp()),
        tick: Tick(bio.tick()),
    })
}

/// Decode FlatBuffer bytes (with marker prefix) into an AgentAction.
///
/// Note: `target_room` is always decoded as `None` (u16 in FlatBuffer,
/// String in domain type). The room name is available from JSON event store.
pub fn decode_agent_action(bytes: &[u8]) -> Result<AgentAction> {
    let fb_bytes = strip_marker(bytes)?;
    let action = fb::root_as_agent_action(fb_bytes).context("invalid FlatBuffer: AgentAction")?;
    Ok(AgentAction {
        agent_id: AgentId(action.agent_id()),
        action_type: fb_to_action_type(action.action_type()),
        target_room: None,
        target_agent: if action.target_agent() == 0 {
            None
        } else {
            Some(AgentId(action.target_agent()))
        },
        content: action.content().map(|s| s.to_string()),
        timestamp: Timestamp(action.timestamp()),
        tick: Tick(action.tick()),
    })
}

/// Decode FlatBuffer bytes (with marker prefix) into a ChaosEvent.
pub fn decode_chaos_event(bytes: &[u8]) -> Result<ChaosEvent> {
    let fb_bytes = strip_marker(bytes)?;
    let chaos = fb::root_as_chaos_event(fb_bytes).context("invalid FlatBuffer: ChaosEvent")?;
    Ok(ChaosEvent {
        event_type: fb_to_event_type(chaos.event_type()),
        target_room: if chaos.target_room() == 0 {
            None
        } else {
            Some(RoomId(chaos.target_room()))
        },
        target_agent: if chaos.target_agent() == 0 {
            None
        } else {
            Some(AgentId(chaos.target_agent()))
        },
        description: chaos.description().to_string(),
        duration_minutes: if chaos.duration_minutes() == 0 {
            None
        } else {
            Some(chaos.duration_minutes())
        },
        timestamp: Timestamp(chaos.timestamp()),
        tick: Tick(chaos.tick()),
    })
}

/// Decode FlatBuffer bytes (with marker prefix) into a Perception.
pub fn decode_perception(bytes: &[u8]) -> Result<Perception> {
    let fb_bytes = strip_marker(bytes)?;
    let perc = fb::root_as_perception(fb_bytes).context("invalid FlatBuffer: Perception")?;
    Ok(Perception {
        agent_id: AgentId(perc.agent_id()),
        circadian_text: perc.circadian_text().unwrap_or_default().to_string(),
        body_text: perc.body_text().unwrap_or_default().to_string(),
        environment_text: perc.environment_text().unwrap_or_default().to_string(),
        acoustic_text: perc.acoustic_text().unwrap_or_default().to_string(),
        presence_text: perc.presence_text().unwrap_or_default().to_string(),
        impulse_text: perc.impulse_text().unwrap_or_default().to_string(),
        timestamp: Timestamp(perc.timestamp()),
        tick: Tick(perc.tick()),
    })
}

/// Decode FlatBuffer bytes (with marker prefix) into a MoodUpdate.
pub fn decode_mood_update(bytes: &[u8]) -> Result<MoodUpdate> {
    let fb_bytes = strip_marker(bytes)?;
    let mood =
        flatbuffers::root::<fb::MoodUpdate>(fb_bytes).context("invalid FlatBuffer: MoodUpdate")?;
    Ok(MoodUpdate {
        agent_id: AgentId(mood.agent_id()),
        valence: mood.valence(),
        arousal: mood.arousal(),
        dominant_emotion: fb_to_emotion(mood.dominant_emotion()),
        timestamp: Timestamp(mood.timestamp()),
        tick: Tick(mood.tick()),
    })
}

/// Decode FlatBuffer bytes (with marker prefix) into a PositionUpdate.
pub fn decode_position_update(bytes: &[u8]) -> Result<PositionUpdate> {
    let fb_bytes = strip_marker(bytes)?;
    let pos = flatbuffers::root::<fb::PositionUpdate>(fb_bytes)
        .context("invalid FlatBuffer: PositionUpdate")?;
    Ok(PositionUpdate {
        agent_id: AgentId(pos.agent_id()),
        room_id: RoomId(pos.room_id()),
        in_transit: pos.in_transit(),
        transit_target: if pos.transit_target() == 0 {
            None
        } else {
            Some(RoomId(pos.transit_target()))
        },
        timestamp: Timestamp(pos.timestamp()),
        tick: Tick(pos.tick()),
    })
}

// ── DomainEvent Encode (for Fan-Out Bridge) ───────────────────

/// Encode a DomainEvent payload as FlatBuffer bytes.
///
/// Returns `None` for event types without FlatBuffer schemas (JSON fallback).
/// The returned bytes include the `FB_MARKER` prefix for format detection.
pub fn encode_domain_event(event: &DomainEvent) -> Option<Vec<u8>> {
    match event.event_type.as_str() {
        "bio_state_updated" => encode_bio_state_from_event(event),
        "agent_action_received" => encode_agent_action_from_event(event),
        "chaos_triggered" => encode_chaos_event_from_event(event),
        _ => None,
    }
}

fn encode_bio_state_from_event(event: &DomainEvent) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_str(&event.payload).ok()?;
    let bio = BioStateUpdate {
        agent_id: AgentId(v.get("agent_id").and_then(|x| x.as_u64())? as u16),
        hunger: v.get("hunger").and_then(|x| x.as_f64())? as f32,
        energy: v.get("energy").and_then(|x| x.as_f64())? as f32,
        caffeine_mg: v.get("caffeine_mg").and_then(|x| x.as_f64())? as f32,
        bladder: v.get("bladder").and_then(|x| x.as_f64())? as f32,
        stress: v.get("stress").and_then(|x| x.as_f64())? as f32,
        social_need: v.get("social_need").and_then(|x| x.as_f64())? as f32,
        comfort: v.get("comfort").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
        timestamp: Timestamp(event.timestamp_ms),
        tick: Tick(event.tick),
    };
    Some(encode_bio_state(&bio))
}

fn encode_agent_action_from_event(event: &DomainEvent) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_str(&event.payload).ok()?;
    let action_type_str = v.get("action_type").and_then(|x| x.as_str())?;
    let action_type = action_type_from_str(action_type_str)?;
    let target_agent = v
        .get("target_agent")
        .and_then(|x| x.as_u64())
        .map(|id| AgentId(id as u16));
    let action = AgentAction {
        agent_id: AgentId(v.get("agent_id").and_then(|x| x.as_u64())? as u16),
        action_type,
        target_room: v
            .get("target_room")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        target_agent,
        content: v
            .get("content")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        timestamp: Timestamp(event.timestamp_ms),
        tick: Tick(event.tick),
    };
    Some(encode_agent_action(&action))
}

fn encode_chaos_event_from_event(event: &DomainEvent) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_str(&event.payload).ok()?;
    let event_type_str = v.get("event_type").and_then(|x| x.as_str())?;
    let et = event_type_from_str(event_type_str)?;
    let chaos = ChaosEvent {
        event_type: et,
        target_room: None,
        target_agent: None,
        description: v
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        duration_minutes: None,
        timestamp: Timestamp(event.timestamp_ms),
        tick: Tick(event.tick),
    };
    Some(encode_chaos_event(&chaos))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── BioStateUpdate Roundtrip ──────────────────────────────

    #[test]
    fn test_bio_state_roundtrip() {
        let original = BioStateUpdate {
            agent_id: AgentId(7),
            hunger: 45.5,
            energy: 72.0,
            caffeine_mg: 95.0,
            bladder: 30.0,
            stress: 55.0,
            social_need: 20.0,
            comfort: 80.0,
            timestamp: Timestamp(2000),
            tick: Tick(100),
        };

        let bytes = encode_bio_state(&original);
        assert!(is_flatbuffer(&bytes));

        let decoded = decode_bio_state(&bytes).expect("decode failed");
        assert_eq!(decoded.agent_id, original.agent_id);
        assert!((decoded.hunger - original.hunger).abs() < f32::EPSILON);
        assert!((decoded.energy - original.energy).abs() < f32::EPSILON);
        assert!((decoded.caffeine_mg - original.caffeine_mg).abs() < f32::EPSILON);
        assert!((decoded.bladder - original.bladder).abs() < f32::EPSILON);
        assert!((decoded.stress - original.stress).abs() < f32::EPSILON);
        assert!((decoded.social_need - original.social_need).abs() < f32::EPSILON);
        assert!((decoded.comfort - original.comfort).abs() < f32::EPSILON);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.tick, original.tick);
    }

    #[test]
    fn test_bio_state_boundary_values() {
        let original = BioStateUpdate {
            agent_id: AgentId(1),
            hunger: 0.0,
            energy: 100.0,
            caffeine_mg: 0.0,
            bladder: 100.0,
            stress: 0.0,
            social_need: 100.0,
            comfort: 0.0,
            timestamp: Timestamp(0),
            tick: Tick(0),
        };

        let bytes = encode_bio_state(&original);
        let decoded = decode_bio_state(&bytes).expect("decode failed");
        assert!((decoded.hunger - 0.0).abs() < f32::EPSILON);
        assert!((decoded.energy - 100.0).abs() < f32::EPSILON);
    }

    // ── AgentAction Roundtrip ─────────────────────────────────

    #[test]
    fn test_agent_action_roundtrip() {
        let original = AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::Chat,
            target_room: Some("konferenz-1".to_string()),
            target_agent: Some(AgentId(5)),
            content: Some("Guten Morgen!".to_string()),
            timestamp: Timestamp(1000),
            tick: Tick(42),
        };

        let bytes = encode_agent_action(&original);
        assert!(is_flatbuffer(&bytes));

        let decoded = decode_agent_action(&bytes).expect("decode failed");
        assert_eq!(decoded.agent_id, original.agent_id);
        assert_eq!(decoded.action_type, original.action_type);
        // target_room is lossy (String→u16 not supported)
        assert_eq!(decoded.target_room, None);
        assert_eq!(decoded.target_agent, original.target_agent);
        assert_eq!(decoded.content, original.content);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.tick, original.tick);
    }

    #[test]
    fn test_agent_action_all_action_types() {
        for at in [
            ActionType::Chat,
            ActionType::Move,
            ActionType::ToolUse,
            ActionType::Emote,
            ActionType::PhoneCall,
        ] {
            let action = AgentAction {
                agent_id: AgentId(1),
                action_type: at,
                target_room: None,
                target_agent: None,
                content: None,
                timestamp: Timestamp(0),
                tick: Tick(0),
            };
            let bytes = encode_agent_action(&action);
            let decoded = decode_agent_action(&bytes).expect("decode failed");
            assert_eq!(decoded.action_type, at, "roundtrip failed for {at:?}");
        }
    }

    // ── ChaosEvent Roundtrip ──────────────────────────────────

    #[test]
    fn test_chaos_event_roundtrip() {
        let original = ChaosEvent {
            event_type: EventType::PrinterBroken,
            target_room: Some(RoomId(5)),
            target_agent: None,
            description: "Drucker zeigt Papierstau an".to_string(),
            duration_minutes: Some(30),
            timestamp: Timestamp(5000),
            tick: Tick(200),
        };

        let bytes = encode_chaos_event(&original);
        assert!(is_flatbuffer(&bytes));

        let decoded = decode_chaos_event(&bytes).expect("decode failed");
        assert_eq!(decoded.event_type, original.event_type);
        assert_eq!(decoded.target_room, original.target_room);
        assert_eq!(decoded.target_agent, original.target_agent);
        assert_eq!(decoded.description, original.description);
        assert_eq!(decoded.duration_minutes, original.duration_minutes);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.tick, original.tick);
    }

    #[test]
    fn test_chaos_event_all_event_types() {
        for et in [
            EventType::PhoneRing,
            EventType::PrinterBroken,
            EventType::PackageDelivery,
            EventType::SBahnDelay,
            EventType::FireAlarmDrill,
            EventType::CakeInKitchen,
            EventType::AirConBroken,
            EventType::InternetOutage,
        ] {
            let chaos = ChaosEvent {
                event_type: et,
                target_room: None,
                target_agent: None,
                description: "Test".to_string(),
                duration_minutes: None,
                timestamp: Timestamp(0),
                tick: Tick(0),
            };
            let bytes = encode_chaos_event(&chaos);
            let decoded = decode_chaos_event(&bytes).expect("decode failed");
            assert_eq!(decoded.event_type, et, "roundtrip failed for {et:?}");
        }
    }

    // ── Perception Roundtrip ──────────────────────────────────

    #[test]
    fn test_perception_roundtrip() {
        let original = Perception {
            agent_id: AgentId(3),
            circadian_text: "Es ist frueh am Morgen".to_string(),
            body_text: "Du spuerst leichten Hunger".to_string(),
            environment_text: "Das Buero riecht nach Kaffee".to_string(),
            acoustic_text: "Leises Tippen im Raum".to_string(),
            presence_text: "Lisa sitzt dir gegenueber".to_string(),
            impulse_text: "Du moechtest einen Kaffee trinken".to_string(),
            timestamp: Timestamp(8000),
            tick: Tick(400),
        };

        let bytes = encode_perception(&original);
        assert!(is_flatbuffer(&bytes));

        let decoded = decode_perception(&bytes).expect("decode failed");
        assert_eq!(decoded.agent_id, original.agent_id);
        assert_eq!(decoded.circadian_text, original.circadian_text);
        assert_eq!(decoded.body_text, original.body_text);
        assert_eq!(decoded.environment_text, original.environment_text);
        assert_eq!(decoded.acoustic_text, original.acoustic_text);
        assert_eq!(decoded.presence_text, original.presence_text);
        assert_eq!(decoded.impulse_text, original.impulse_text);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.tick, original.tick);
    }

    // ── MoodUpdate Roundtrip ──────────────────────────────────

    #[test]
    fn test_mood_update_roundtrip() {
        let original = MoodUpdate {
            agent_id: AgentId(12),
            valence: 0.7,
            arousal: 0.4,
            dominant_emotion: Emotion::Excited,
            timestamp: Timestamp(3000),
            tick: Tick(150),
        };

        let bytes = encode_mood_update(&original);
        assert!(is_flatbuffer(&bytes));

        let decoded = decode_mood_update(&bytes).expect("decode failed");
        assert_eq!(decoded.agent_id, original.agent_id);
        assert!((decoded.valence - original.valence).abs() < f32::EPSILON);
        assert!((decoded.arousal - original.arousal).abs() < f32::EPSILON);
        assert_eq!(decoded.dominant_emotion, original.dominant_emotion);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.tick, original.tick);
    }

    #[test]
    fn test_mood_update_all_emotions() {
        for em in [
            Emotion::Neutral,
            Emotion::Happy,
            Emotion::Frustrated,
            Emotion::Stressed,
            Emotion::Relaxed,
            Emotion::Excited,
            Emotion::Bored,
            Emotion::Anxious,
            Emotion::Focused,
            Emotion::Tired,
        ] {
            let mood = MoodUpdate {
                agent_id: AgentId(1),
                valence: 0.0,
                arousal: 0.5,
                dominant_emotion: em,
                timestamp: Timestamp(0),
                tick: Tick(0),
            };
            let bytes = encode_mood_update(&mood);
            let decoded = decode_mood_update(&bytes).expect("decode failed");
            assert_eq!(decoded.dominant_emotion, em, "roundtrip failed for {em:?}");
        }
    }

    // ── PositionUpdate Roundtrip ──────────────────────────────

    #[test]
    fn test_position_update_roundtrip() {
        let original = PositionUpdate {
            agent_id: AgentId(42),
            room_id: RoomId(3),
            in_transit: true,
            transit_target: Some(RoomId(7)),
            timestamp: Timestamp(9000),
            tick: Tick(450),
        };

        let bytes = encode_position_update(&original);
        assert!(is_flatbuffer(&bytes));

        let decoded = decode_position_update(&bytes).expect("decode failed");
        assert_eq!(decoded.agent_id, original.agent_id);
        assert_eq!(decoded.room_id, original.room_id);
        assert_eq!(decoded.in_transit, original.in_transit);
        assert_eq!(decoded.transit_target, original.transit_target);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.tick, original.tick);
    }

    // ── Content-Type Detection ────────────────────────────────

    #[test]
    fn test_is_flatbuffer() {
        assert!(is_flatbuffer(&[0x00, 0x01, 0x02]));
        assert!(!is_flatbuffer(&[0x7B, 0x22])); // JSON: {"
        assert!(!is_flatbuffer(&[]));
    }

    // ── DomainEvent Encode ────────────────────────────────────

    #[test]
    fn test_encode_domain_event_bio_state() {
        let payload = serde_json::json!({
            "type": "BioStateUpdated",
            "agent_id": 5,
            "hunger": 45.5,
            "energy": 72.0,
            "caffeine_mg": 95.0,
            "bladder": 30.0,
            "stress": 55.0,
            "social_need": 20.0,
            "room_id": "buero-dev-1",
            "mood": "Neutral",
            "valence": 0.3,
            "arousal": 0.5
        });
        let event = DomainEvent::new(
            "bio_state_updated",
            "AGENT-05",
            &payload.to_string(),
            "corr-1",
            100,
        );

        let bytes = encode_domain_event(&event).expect("encode should succeed");
        assert!(is_flatbuffer(&bytes));

        let decoded = decode_bio_state(&bytes).expect("decode failed");
        assert_eq!(decoded.agent_id, AgentId(5));
        assert!((decoded.hunger - 45.5).abs() < f32::EPSILON);
        assert!((decoded.energy - 72.0).abs() < f32::EPSILON);
        assert_eq!(decoded.tick, Tick(100));
    }

    #[test]
    fn test_encode_domain_event_agent_action() {
        let payload = serde_json::json!({
            "type": "AgentActionReceived",
            "agent_id": 1,
            "action_type": "Chat",
            "target_room": "konferenz-1",
            "content": "Guten Morgen!"
        });
        let event = DomainEvent::new(
            "agent_action_received",
            "AGENT-01",
            &payload.to_string(),
            "corr-2",
            42,
        );

        let bytes = encode_domain_event(&event).expect("encode should succeed");
        let decoded = decode_agent_action(&bytes).expect("decode failed");
        assert_eq!(decoded.agent_id, AgentId(1));
        assert_eq!(decoded.action_type, ActionType::Chat);
        assert_eq!(decoded.content, Some("Guten Morgen!".to_string()));
    }

    #[test]
    fn test_encode_domain_event_chaos() {
        let payload = serde_json::json!({
            "type": "ChaosTriggered",
            "event_type": "PrinterBroken",
            "target_room": "buero-dev-1",
            "description": "Drucker zeigt Papierstau"
        });
        let event = DomainEvent::new(
            "chaos_triggered",
            "buero-dev-1",
            &payload.to_string(),
            "corr-3",
            200,
        );

        let bytes = encode_domain_event(&event).expect("encode should succeed");
        let decoded = decode_chaos_event(&bytes).expect("decode failed");
        assert_eq!(decoded.event_type, EventType::PrinterBroken);
        assert_eq!(decoded.description, "Drucker zeigt Papierstau");
    }

    #[test]
    fn test_encode_domain_event_unknown_returns_none() {
        let event = DomainEvent::new("nightrun_started", "run-1", "{}", "corr-4", 300);
        assert!(encode_domain_event(&event).is_none());
    }

    // ── Error Cases ───────────────────────────────────────────

    #[test]
    fn test_decode_invalid_marker() {
        let result = decode_bio_state(&[0x7B, 0x22]);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_empty_bytes() {
        let result = decode_bio_state(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_only_marker() {
        let result = decode_bio_state(&[0x00]);
        assert!(result.is_err());
    }
}
