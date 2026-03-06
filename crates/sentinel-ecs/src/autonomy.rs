//! Regelbasierte Agent-Autonomie.
//!
//! Laeuft als zusaetzliches System nach dem Decision System.
//! Reagiert auf P0-Notfaelle mit deterministischen Aktionen:
//! - Blase > 90 → Zur naechsten Toilette gehen
//! - Hunger > 95 → Zur Kueche gehen
//! - Energie < 15 → Zur Kueche gehen (Kaffee)
//! - Stress > 90 → Zum Flur gehen (frische Luft)
//!
//! Erzeugt DomainEvents (TransitStarted, BioActionPerformed, AgentActionReceived)
//! und mutiert den ECS-State direkt — kein Channel noetig.
//!
//! Cooldown: 30 Ticks pro Agent (verhindert wiederholte Aktionen).

use super::components::*;
use super::world::{EventBuffer, SimulationTime};
use bevy_ecs::prelude::*;
use sentinel_common::{DomainEvent, DomainEventPayload};

/// Cooldown in Ticks bevor ein Agent erneut autonom handeln kann.
const AUTONOMY_COOLDOWN_TICKS: u64 = 30;

/// Component fuer Agent-Autonomie-Cooldown.
#[derive(Component, Debug, Clone, Default)]
pub struct AutonomyCooldown {
    pub last_action_tick: u64,
}

/// Regelbasierte Autonomie: P0-Notfaelle deterministische Reaktionen.
///
/// Prueft Bio-Zustand und erzeugt automatische Aktionen fuer kritische Situationen.
/// Agiert nur wenn der Agent NICHT bereits in Transit ist und der Cooldown abgelaufen ist.
pub fn autonomy_system(
    mut query: Query<(
        &AgentIdentity,
        &mut Position,
        &mut BioState,
        &mut AutonomyCooldown,
    )>,
    time: Res<SimulationTime>,
    mut event_buffer: ResMut<EventBuffer>,
) {
    let tick = time.tick.0;

    for (identity, mut position, mut bio, mut cooldown) in &mut query {
        // Nicht handeln waehrend Transit
        if position.in_transit {
            continue;
        }

        // Cooldown pruefen
        if tick.saturating_sub(cooldown.last_action_tick) < AUTONOMY_COOLDOWN_TICKS {
            continue;
        }

        let correlation_id = uuid::Uuid::new_v4().to_string();

        // P0: Blase > 90 → Zur naechsten Toilette
        if bio.bladder > 90.0 {
            let target = nearest_toilet(&position.room_id, identity.agent_id.0 as u32);
            if position.room_id == target {
                // Bereits auf der Toilette → use_bathroom
                sentinel_bio::use_bathroom(&mut bio);
                let payload = DomainEventPayload::BioActionPerformed {
                    agent_id: identity.agent_id,
                    action: "use_bathroom".to_string(),
                };
                let event = DomainEvent::new(
                    payload.event_type_str(),
                    &identity.agent_id.to_string(),
                    &payload.to_json(),
                    &correlation_id,
                    tick,
                );
                event_buffer.events.push(event);
            } else {
                start_transit(
                    identity,
                    &mut position,
                    &target,
                    &correlation_id,
                    tick,
                    &mut event_buffer,
                );
            }
            cooldown.last_action_tick = tick;
            continue;
        }

        // P0: Hunger > 95 → Zur Kueche
        if bio.hunger > 95.0 {
            let target = "kueche".to_string();
            if position.room_id == target {
                sentinel_bio::eat_meal(&mut bio);
                let payload = DomainEventPayload::BioActionPerformed {
                    agent_id: identity.agent_id,
                    action: "eat_meal".to_string(),
                };
                let event = DomainEvent::new(
                    payload.event_type_str(),
                    &identity.agent_id.to_string(),
                    &payload.to_json(),
                    &correlation_id,
                    tick,
                );
                event_buffer.events.push(event);
            } else {
                start_transit(
                    identity,
                    &mut position,
                    &target,
                    &correlation_id,
                    tick,
                    &mut event_buffer,
                );
            }
            cooldown.last_action_tick = tick;
            continue;
        }

        // P0: Energie < 15 → Kaffee in der Kueche
        if bio.energy < 15.0 {
            let target = "kueche".to_string();
            if position.room_id == target {
                sentinel_bio::drink_coffee(&mut bio);
                let payload = DomainEventPayload::BioActionPerformed {
                    agent_id: identity.agent_id,
                    action: "drink_coffee".to_string(),
                };
                let event = DomainEvent::new(
                    payload.event_type_str(),
                    &identity.agent_id.to_string(),
                    &payload.to_json(),
                    &correlation_id,
                    tick,
                );
                event_buffer.events.push(event);
            } else {
                start_transit(
                    identity,
                    &mut position,
                    &target,
                    &correlation_id,
                    tick,
                    &mut event_buffer,
                );
            }
            cooldown.last_action_tick = tick;
            continue;
        }

        // P0: Stress > 90 → Zum Flur (frische Luft)
        if bio.stress > 90.0 {
            let target = nearest_hallway(&position.room_id);
            if position.room_id != target {
                start_transit(
                    identity,
                    &mut position,
                    &target,
                    &correlation_id,
                    tick,
                    &mut event_buffer,
                );
                cooldown.last_action_tick = tick;
            }
            continue;
        }

        // Kein P0-Notfall aktiv → zurueck zum Arbeitsraum wenn in Utility-Raum
        // (Toilette, Kueche, Flur). Verhindert dass Agents nach Notfall stecken bleiben.
        if is_utility_room(&position.room_id) {
            let home_room = default_work_room(&identity.role, identity.agent_id.0);
            if position.room_id != home_room {
                start_transit(
                    identity,
                    &mut position,
                    &home_room,
                    &correlation_id,
                    tick,
                    &mut event_buffer,
                );
                cooldown.last_action_tick = tick;
            }
        }
    }
}

/// Startet einen Transit und erzeugt TransitStarted + AgentActionReceived Events.
fn start_transit(
    identity: &AgentIdentity,
    position: &mut Position,
    target: &str,
    correlation_id: &str,
    tick: u64,
    event_buffer: &mut EventBuffer,
) {
    let from_room = position.room_id.clone();
    let duration_ms: u32 = 3000;

    position.in_transit = true;
    position.transit_target = Some(target.to_string());
    position.transit_remaining_ms = duration_ms;
    position.transit_correlation_id = Some(correlation_id.to_string());

    // AgentActionReceived Event (autonome Aktion)
    let action_payload = DomainEventPayload::AgentActionReceived {
        agent_id: identity.agent_id,
        action_type: "Move".to_string(),
        target_room: Some(target.to_string()),
        content: Some("autonomy:bio_emergency".to_string()),
    };
    let action_event = DomainEvent::new(
        action_payload.event_type_str(),
        &identity.agent_id.to_string(),
        &action_payload.to_json(),
        correlation_id,
        tick,
    );
    let action_event_id = action_event.event_id.clone();
    event_buffer.events.push(action_event);

    // TransitStarted Event (Causation-Chain vom Action-Event)
    let transit_payload = DomainEventPayload::TransitStarted {
        agent_id: identity.agent_id,
        from_room,
        to_room: target.to_string(),
        duration_ms,
    };
    let transit_event = DomainEvent::new(
        transit_payload.event_type_str(),
        &identity.agent_id.to_string(),
        &transit_payload.to_json(),
        correlation_id,
        tick,
    )
    .with_causation(&action_event_id);
    event_buffer.events.push(transit_event);
}

/// Bestimmt die naechste Toilette basierend auf der aktuellen Position und Agent-ID.
/// Gerade Agent-IDs → Damen, ungerade → Herren (deterministische Zuweisung).
fn nearest_toilet(current_room: &str, agent_id: u32) -> String {
    let gender_suffix = if agent_id.is_multiple_of(2) {
        "damen"
    } else {
        "herren"
    };
    if current_room.contains("og")
        || current_room.contains("design")
        || current_room == "meetingraum-02"
        || current_room == "meetingraum-03"
    {
        format!("toilette-og-{}", gender_suffix)
    } else {
        format!("toilette-eg-{}", gender_suffix)
    }
}

/// Prueft ob ein Raum ein Utility-Raum ist (Toilette, Kueche, Flur).
/// Agents sollen nach P0-Notfaellen nicht in diesen Raeumen verweilen.
fn is_utility_room(room_id: &str) -> bool {
    room_id.starts_with("toilette")
        || room_id.starts_with("flur")
        || room_id == "kueche"
        || room_id == "empfang"
}

/// Bestimmt den Standard-Arbeitsraum eines Agents basierend auf Rolle und ID.
/// Deterministisch: gleiche Inputs → gleicher Raum.
fn default_work_room(role: &str, agent_id: u16) -> String {
    let role_lower = role.to_lowercase();
    if role_lower.contains("ceo") || role_lower.contains("geschaeft") {
        "buero-ceo".to_string()
    } else if role_lower.contains("design") {
        // Design-Agents: auf buero-design-1 und buero-design-2 verteilen
        if agent_id.is_multiple_of(2) {
            "buero-design-1".to_string()
        } else {
            "buero-design-2".to_string()
        }
    } else {
        // Dev und alle anderen: auf buero-dev-1 und buero-dev-2 verteilen
        if agent_id.is_multiple_of(2) {
            "buero-dev-1".to_string()
        } else {
            "buero-dev-2".to_string()
        }
    }
}

/// Bestimmt den naechsten Flur (frische Luft).
fn nearest_hallway(current_room: &str) -> String {
    if current_room.contains("og")
        || current_room.contains("design")
        || current_room == "meetingraum-02"
        || current_room == "meetingraum-03"
    {
        "flur-og".to_string()
    } else {
        "flur-eg".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{create_simulation_world, spawn_agent, SimulationTime};
    use sentinel_common::{AgentId, Tick};

    #[test]
    fn test_autonomy_bladder_emergency_triggers_transit() {
        let (mut world, _) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test", "Dev", 1);

        // AutonomyCooldown hinzufuegen
        world.entity_mut(entity).insert(AutonomyCooldown::default());

        // Blase auf Notfall setzen
        world.get_mut::<BioState>(entity).unwrap().bladder = 95.0;

        // Autonomy-System als Schedule
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(autonomy_system);

        // Tick ausfuehren
        world.resource_mut::<SimulationTime>().tick = Tick(31);
        schedule.run(&mut world);

        // Agent sollte jetzt in Transit sein
        let pos = world.get::<Position>(entity).unwrap();
        assert!(pos.in_transit, "Agent sollte nach P0-Blase in Transit sein");
        assert!(
            pos.transit_target.as_ref().unwrap().contains("toilette"),
            "Ziel sollte Toilette sein"
        );

        // Events sollten erzeugt worden sein
        let buffer = world.resource::<EventBuffer>();
        let transit_events: Vec<_> = buffer
            .events
            .iter()
            .filter(|e| e.event_type == "transit_started")
            .collect();
        assert!(
            !transit_events.is_empty(),
            "TransitStarted Event sollte erzeugt werden"
        );
    }

    #[test]
    fn test_autonomy_cooldown_prevents_rapid_actions() {
        let (mut world, _) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test", "Dev", 1);
        world.entity_mut(entity).insert(AutonomyCooldown::default());
        world.get_mut::<BioState>(entity).unwrap().bladder = 95.0;

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(autonomy_system);

        // Erster Tick (31): Action wird ausgefuehrt
        world.resource_mut::<SimulationTime>().tick = Tick(31);
        schedule.run(&mut world);
        assert!(
            world.get::<Position>(entity).unwrap().in_transit,
            "Erster Tick: Transit starten"
        );

        // Transit manuell abschliessen
        {
            let mut pos = world.get_mut::<Position>(entity).unwrap();
            pos.in_transit = false;
            pos.room_id = "toilette-eg-herren".to_string();
            pos.transit_target = None;
        }

        // Zweiter Tick (32): Cooldown aktiv, keine Aktion
        world.resource_mut::<SimulationTime>().tick = Tick(32);
        let buffer_len_before = world.resource::<EventBuffer>().events.len();
        schedule.run(&mut world);

        // Keine neuen Events (Cooldown blockiert)
        let buffer_len_after = world.resource::<EventBuffer>().events.len();
        // Die Spawn-Events sind noch im Buffer, aber keine neuen autonomy events
        assert_eq!(
            buffer_len_before, buffer_len_after,
            "Cooldown: keine neuen Events"
        );
    }

    #[test]
    fn test_autonomy_at_target_performs_action() {
        let (mut world, _) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test", "Dev", 1);
        world.entity_mut(entity).insert(AutonomyCooldown::default());

        // Agent ist bereits auf der Toilette mit voller Blase (AgentId(1) = ungerade → herren)
        world.get_mut::<Position>(entity).unwrap().room_id = "toilette-eg-herren".to_string();
        world.get_mut::<BioState>(entity).unwrap().bladder = 95.0;

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(autonomy_system);

        world.resource_mut::<SimulationTime>().tick = Tick(31);
        schedule.run(&mut world);

        // Blase sollte reduziert worden sein (use_bathroom)
        let bio = world.get::<BioState>(entity).unwrap();
        assert!(
            bio.bladder < 30.0,
            "use_bathroom sollte Blase reduzieren, got: {}",
            bio.bladder
        );

        // BioActionPerformed Event
        let buffer = world.resource::<EventBuffer>();
        let bio_events: Vec<_> = buffer
            .events
            .iter()
            .filter(|e| e.event_type == "bio_action_performed")
            .collect();
        assert!(
            !bio_events.is_empty(),
            "BioActionPerformed Event sollte erzeugt werden"
        );
    }

    #[test]
    fn test_return_to_work_from_toilet() {
        let (mut world, _) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test", "Dev", 1);
        world.entity_mut(entity).insert(AutonomyCooldown::default());

        // Agent auf Toilette, KEIN P0-Notfall (bladder=10, hunger=20, energy=80, stress=0)
        world.get_mut::<Position>(entity).unwrap().room_id = "toilette-eg-herren".to_string();
        world.get_mut::<BioState>(entity).unwrap().bladder = 10.0;
        world.get_mut::<BioState>(entity).unwrap().hunger = 20.0;

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(autonomy_system);

        world.resource_mut::<SimulationTime>().tick = Tick(31);
        schedule.run(&mut world);

        // Agent sollte Transit zum Arbeitsraum starten
        let pos = world.get::<Position>(entity).unwrap();
        assert!(
            pos.in_transit,
            "Agent sollte nach P0-Abschluss zum Arbeitsraum zurueckkehren"
        );
        assert!(
            pos.transit_target.as_ref().unwrap().starts_with("buero-"),
            "Ziel sollte ein Buero sein, got: {:?}",
            pos.transit_target
        );
    }

    #[test]
    fn test_no_return_during_p0() {
        let (mut world, _) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test", "Dev", 1);
        world.entity_mut(entity).insert(AutonomyCooldown::default());

        // Agent auf Toilette MIT P0-Notfall (bladder=95)
        world.get_mut::<Position>(entity).unwrap().room_id = "toilette-eg-herren".to_string();
        world.get_mut::<BioState>(entity).unwrap().bladder = 95.0;

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(autonomy_system);

        world.resource_mut::<SimulationTime>().tick = Tick(31);
        schedule.run(&mut world);

        // P0 hat Prioritaet: Agent bleibt (use_bathroom wird ausgefuehrt)
        let bio = world.get::<BioState>(entity).unwrap();
        assert!(
            bio.bladder < 30.0,
            "P0 sollte use_bathroom ausfuehren: bladder={}",
            bio.bladder
        );
    }

    #[test]
    fn test_is_utility_room() {
        assert!(is_utility_room("toilette-eg-herren"));
        assert!(is_utility_room("toilette-og-damen"));
        assert!(is_utility_room("kueche"));
        assert!(is_utility_room("flur-eg"));
        assert!(is_utility_room("empfang"));
        assert!(!is_utility_room("buero-dev-1"));
        assert!(!is_utility_room("buero-ceo"));
        assert!(!is_utility_room("meetingraum-01"));
    }

    #[test]
    fn test_default_work_room() {
        assert_eq!(default_work_room("CEO", 1), "buero-ceo");
        assert_eq!(default_work_room("Design", 2), "buero-design-1");
        assert_eq!(default_work_room("Design", 3), "buero-design-2");
        assert_eq!(default_work_room("Dev", 2), "buero-dev-1");
        assert_eq!(default_work_room("Dev", 3), "buero-dev-2");
        assert_eq!(default_work_room("HR", 1), "buero-dev-2"); // Default
    }
}
