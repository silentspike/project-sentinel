//! ECS Systems fuer Agent-Simulation.
//!
//! Definiert 12 Systems in strikter Ausfuehrungsreihenfolge:
//! 1. input_system - Empfaengt Agent-Aktionen via Channel
//!    1b. operator_command_system - Injiziert manuelles Chaos aus der Operator-API
//! 2. bio_system - Aktualisiert biologische Zustaende (sentinel-bio) + Auto-Coffee
//! 3. physics_system - Berechnet Raum-Physik (sentinel-physics)
//! 4. transit_system - Verarbeitet Raumwechsel + Transit-Events
//!    4b. work_context_system - Deriviert WorkContext (Meeting/Deadline/Conflict)
//! 5. chaos_system - Generiert Zufallsereignisse + setzt Conflict-Cooldown
//! 6. mood_system - Berechnet Stimmung aus Bio+Kontext
//! 7. perception_system - Generiert Wahrnehmungstext fuer LLM-Prompt
//! 8. decision_system - Priorisiert Events fuer impulse_text (P0-P3)
//! 9. output_system - Sendet Wahrnehmung via Channel
//! 10. persist_system - Persistiert Events (Limbo) + State-Snapshots (redb)

use super::components::*;
use super::world::{
    ActionReceiver, ActiveChaos, ActiveChaosEvent, ActiveRoomStimuli, EventBuffer, LimboEventStore,
    OperatorCommandReceiver, PersistTelemetry, PsiMetrics, RedbStateStore, RoomDistanceMap,
    RoomPhysicsState, ToolRuntimeResource, ZenohFanoutSender,
};
use super::world::{PerceptionSender, SimulationTime};
use bevy_ecs::prelude::*;
use sentinel_common::{
    ActionType, DomainEvent, DomainEventPayload, Emotion, EventType, OperatorCommand, Perception,
    RoomStimulusType, Timestamp,
};
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, warn};

/// Ausfuehrungsreihenfolge der Simulation-Systems
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum SimulationPhase {
    Input,
    Biology,
    Physics,
    Transit,
    Chaos,
    Mood,
    Perception,
    Decision,
    Output,
    Persist,
}

/// 1. Empfaengt Agent-Aktionen via Channel (extern: Zenoh-Subscriber fuettert den Channel).
///
/// Verarbeitet alle pending Actions in einem Tick:
/// - Move → Position.transit_target + in_transit setzen
/// - Chat → WorkContext aktualisieren
/// - ToolUse → Bio-Actions (drink_coffee, eat_meal, use_bathroom) oder WorkContext
/// - Emote/PhoneCall → nur Event-Trail (keine State-Mutation)
/// - Erzeugt DomainEvent (AgentActionReceived) pro Aktion mit Causation-Chain
pub fn input_system(
    receiver: Option<Res<ActionReceiver>>,
    tool_runtime: Option<Res<ToolRuntimeResource>>,
    room_distances: Option<Res<RoomDistanceMap>>,
    _room_physics_state: Option<Res<RoomPhysicsState>>,
    mut query: Query<(
        &AgentIdentity,
        &mut Position,
        &mut WorkContext,
        &mut BioState,
        &AgentCapabilities,
    )>,
    mut event_buffer: ResMut<EventBuffer>,
    time: Res<SimulationTime>,
    mut active_smells: ResMut<super::world::ActiveSmells>,
) {
    let Some(receiver) = receiver else { return };
    let Ok(rx) = receiver.0.lock() else { return };

    while let Ok(action) = rx.try_recv() {
        // Correlation-ID fuer diesen Vorgang (gruppiert Action + Folge-Events)
        let correlation_id = uuid::Uuid::new_v4().to_string();
        let mut bio_action: Option<&str> = None;
        let mut bio_room: Option<String> = None;

        // Agent im ECS finden
        let mut found = false;
        for (identity, mut position, mut work_ctx, mut bio, capabilities) in &mut query {
            if identity.agent_id != action.agent_id {
                continue;
            }
            found = true;

            match action.action_type {
                ActionType::Move => {
                    if let Some(target_room) = &action.target_room {
                        let from_room = position.room_id.clone();
                        let to_room = target_room.clone();
                        // Distance-basierte Transit-Dauer: 1500ms Basis + 800ms pro Hop
                        // Clamp auf 2000-5000ms (TRANSIT_MIN/MAX aus sentinel-physics)
                        let hops = room_distances
                            .as_ref()
                            .map(|rd| rd.distance(&from_room, &to_room))
                            .unwrap_or(2);
                        let raw_ms = 1500 + hops * 800;
                        let duration_ms: u32 = raw_ms.clamp(2000, 5000);
                        position.in_transit = true;
                        position.transit_target = Some(to_room.clone());
                        position.transit_remaining_ms = duration_ms;
                        position.transit_correlation_id = Some(correlation_id.clone());

                        // TransitStarted Event (Causation-Chain wird unten via action_event gesetzt)
                        let transit_payload = DomainEventPayload::TransitStarted {
                            agent_id: identity.agent_id,
                            from_room,
                            to_room,
                            duration_ms,
                        };
                        let transit_event = DomainEvent::new(
                            transit_payload.event_type_str(),
                            &identity.agent_id.to_string(),
                            &transit_payload.to_json(),
                            &correlation_id,
                            time.tick.0,
                        );
                        event_buffer.events.push(transit_event);
                    }
                }
                ActionType::Chat => {
                    if let Some(content) = &action.content {
                        work_ctx.current_task = Some(content.clone());
                    }
                }
                ActionType::ToolUse => {
                    if let Some(content) = &action.content {
                        match content.as_str() {
                            "drink_coffee" => {
                                sentinel_bio::drink_coffee(&mut bio);
                                bio_action = Some("drink_coffee");
                                if !position.in_transit {
                                    bio_room = Some(position.room_id.clone());
                                }
                            }
                            "eat_meal" => {
                                sentinel_bio::eat_meal(&mut bio);
                                bio_action = Some("eat_meal");
                                if !position.in_transit {
                                    bio_room = Some(position.room_id.clone());
                                }
                            }
                            "use_bathroom" => {
                                sentinel_bio::use_bathroom(&mut bio);
                                bio_action = Some("use_bathroom");
                            }
                            "drink_water" => {
                                sentinel_bio::drink_water(&mut bio);
                                bio_action = Some("drink_water");
                            }
                            _ => {
                                // Tool-Dispatch: Versuche via ToolRuntime
                                if let Some(ref runtime) = tool_runtime {
                                    if let Some((tool_name, tool_input)) =
                                        parse_tool_content(content)
                                    {
                                        // Agent-Home als allowed_path setzen (mapped auf
                                        // sentinel-fs CoW-Layer via bwrap Bind-Mount).
                                        let agent_home = std::path::PathBuf::from(format!(
                                            "/home/AGENT-{:02}",
                                            identity.agent_id.0
                                        ));
                                        let sandbox = sentinel_wasm::SandboxConfig {
                                            allowed_paths: vec![agent_home],
                                            ..sentinel_wasm::SandboxConfig::restrictive()
                                        };
                                        #[cfg(feature = "wasm")]
                                        let room_physics_state = _room_physics_state.as_deref();
                                        #[cfg(not(feature = "wasm"))]
                                        let _ = &_room_physics_state;
                                        let ctx = sentinel_wasm::ExecutionContext {
                                            agent_id: format!("AGENT-{:02}", identity.agent_id.0),
                                            agent_capabilities: capabilities.tools.clone(),
                                            sandbox,
                                            correlation_id: correlation_id.clone(),
                                            tick: time.tick.0,
                                            #[cfg(feature = "wasm")]
                                            agent_snapshot: Some(build_agent_snapshot(
                                                identity, &bio, &position,
                                            )),
                                            #[cfg(feature = "wasm")]
                                            rooms: Some(build_room_snapshots(
                                                room_physics_state,
                                                &position,
                                            )),
                                        };

                                        match runtime.0.execute(&tool_name, &tool_input, &ctx) {
                                            Ok(result) => {
                                                let tool_event = result
                                                    .to_domain_event(&correlation_id, time.tick.0);
                                                event_buffer.events.push(tool_event);
                                                tracing::info!(
                                                    agent = %identity.agent_id.0,
                                                    tool = %tool_name,
                                                    "Tool ausgefuehrt"
                                                );
                                            }
                                            Err(e) => {
                                                warn!(
                                                    agent = %identity.agent_id.0,
                                                    tool = %tool_name,
                                                    error = %e,
                                                    "Tool-Fehler"
                                                );
                                            }
                                        }
                                    } else {
                                        work_ctx.current_task = Some(content.clone());
                                    }
                                } else {
                                    work_ctx.current_task = Some(content.clone());
                                }
                            }
                        }
                    }
                }
                ActionType::Emote | ActionType::PhoneCall => {
                    // Keine State-Mutation, Event wird unten erzeugt
                }
            }

            break;
        }

        if !found {
            warn!(agent_id = %action.agent_id, "input_system: Agent nicht gefunden");
            continue;
        }

        // AgentActionReceived-Event erzeugen (fuer JEDE Action)
        let payload = DomainEventPayload::AgentActionReceived {
            agent_id: action.agent_id,
            action_type: format!("{:?}", action.action_type),
            target_room: action.target_room.clone(),
            content: action.content.clone(),
        };
        let action_event = DomainEvent::new(
            payload.event_type_str(),
            &action.agent_id.to_string(),
            &payload.to_json(),
            &correlation_id,
            time.tick.0,
        );
        let action_event_id = action_event.event_id.clone();
        event_buffer.events.push(action_event);

        // BioActionPerformed-Event (Causation-Chain: Action → Bio-Effect)
        if let Some(action_name) = bio_action {
            let bio_payload = DomainEventPayload::BioActionPerformed {
                agent_id: action.agent_id,
                action: action_name.to_string(),
            };
            let bio_event = DomainEvent::new(
                bio_payload.event_type_str(),
                &action.agent_id.to_string(),
                &bio_payload.to_json(),
                &correlation_id,
                time.tick.0,
            )
            .with_causation(&action_event_id);
            event_buffer.events.push(bio_event);

            // SmellEvent bei drink_coffee/eat_meal (nur wenn Agent nicht in Transit)
            if let Some(ref room) = bio_room {
                let (smell_type, intensity, duration) = match action_name {
                    "drink_coffee" => ("coffee", 0.8f32, 120u64),
                    "eat_meal" => ("food", 0.6, 90),
                    _ => continue,
                };
                let smell_payload = DomainEventPayload::SmellEventTriggered {
                    room_id: room.clone(),
                    smell_type: smell_type.to_string(),
                    intensity,
                    duration_ticks: duration,
                };
                let smell_event = DomainEvent::new(
                    smell_payload.event_type_str(),
                    room,
                    &smell_payload.to_json(),
                    &correlation_id,
                    time.tick.0,
                )
                .with_causation(&action_event_id);
                event_buffer.events.push(smell_event);
                active_smells.add(
                    room,
                    smell_type.to_string(),
                    intensity,
                    time.tick.0,
                    duration,
                );
            }
        }
    }
}

/// 1b. Empfaengt Operator-Kommandos und injiziert manuelles Chaos.
pub fn operator_command_system(
    receiver: Option<Res<OperatorCommandReceiver>>,
    time: Res<SimulationTime>,
    mut event_buffer: ResMut<EventBuffer>,
    mut active_chaos: ResMut<ActiveChaos>,
    mut active_room_stimuli: ResMut<ActiveRoomStimuli>,
    mut agents: Query<(&Position, &mut WorkContext)>,
) {
    let Some(receiver) = receiver else { return };
    let Ok(rx) = receiver.0.lock() else { return };

    while let Ok(command) = rx.try_recv() {
        match command {
            OperatorCommand::Chaos(command) => {
                let duration_ticks = command.duration_ticks.unwrap_or_else(|| {
                    sentinel_physics::default_chaos_duration_ticks(command.chaos_type)
                });
                let metadata = InjectedChaosMetadata {
                    event_id: command.event_id.as_str(),
                    correlation_id: command.correlation_id.as_str(),
                    operation_id: command.operation_id.as_str(),
                };

                inject_chaos_event(
                    command.chaos_type,
                    &command.room_id,
                    &command.description,
                    time.tick.0,
                    duration_ticks,
                    &mut event_buffer,
                    &mut active_chaos,
                    &mut agents,
                    Some(metadata),
                );
            }
            OperatorCommand::RoomStimulus(command) => {
                let duration_ticks = command
                    .duration_ticks
                    .unwrap_or(DEFAULT_ROOM_STIMULUS_DURATION_TICKS);
                let metadata = InjectedStimulusMetadata {
                    event_id: command.event_id.as_str(),
                    correlation_id: command.correlation_id.as_str(),
                    operation_id: command.operation_id.as_str(),
                };
                inject_room_stimulus(
                    &command.room_id,
                    command.stimulus_type,
                    command.delta,
                    &command.description,
                    time.tick.0,
                    duration_ticks,
                    &mut event_buffer,
                    &mut active_room_stimuli,
                    Some(metadata),
                );
            }
        }
    }
}

/// 2. Aktualisiert biologische Zustaende via sentinel-bio Differenzialgleichungen.
///
/// Erzeugt BioStateUpdated DomainEvents alle 20 Ticks (periodischer Snapshot).
/// Auto-Coffee: Agents trinken automatisch Kaffee wenn Energy < 50 und kein Koffein im System.
pub fn bio_system(
    mut query: Query<(
        &AgentIdentity,
        &mut BioState,
        &Personality,
        &WorkContext,
        &Position,
        &Mood,
    )>,
    time: Res<SimulationTime>,
    psi: Option<Res<PsiMetrics>>,
    mut event_buffer: ResMut<EventBuffer>,
    mut active_smells: ResMut<super::world::ActiveSmells>,
) {
    let tick = time.tick.0;

    for (identity, mut bio, personality, work, position, mood) in &mut query {
        sentinel_bio::update_bio_state(
            &mut bio,
            personality,
            work,
            time.delta_seconds,
            time.sim_hour,
        );

        // PSI→Bio Integration: Hardware-Druck wird zu Agent-Stress/Comfort
        if let Some(ref psi) = psi {
            sentinel_bio::apply_psi_stress(&mut bio, psi.cpu_avg10, psi.mem_avg10);
        }

        // Auto-Coffee: Agent trinkt Kaffee bei moderater Muedigkeit
        // Threshold: energy<70 (realistisch — Menschen trinken Kaffee bevor sie erschoepft sind)
        // Frequenz: alle 3 Minuten (180 Ticks bei 1Hz)
        if bio.energy < 70.0
            && bio.caffeine_mg < 10.0
            && (8.0..16.0).contains(&time.sim_hour)
            && tick > 0
            && tick.is_multiple_of(180)
        {
            sentinel_bio::drink_coffee(&mut bio);
            let correlation_id = uuid::Uuid::new_v4().to_string();
            let bio_payload = DomainEventPayload::BioActionPerformed {
                agent_id: identity.agent_id,
                action: "drink_coffee".to_string(),
            };
            let bio_event = DomainEvent::new(
                bio_payload.event_type_str(),
                &identity.agent_id.to_string(),
                &bio_payload.to_json(),
                &correlation_id,
                tick,
            );
            event_buffer.events.push(bio_event);

            // SmellEvent bei Auto-Coffee (nur wenn Agent nicht in Transit)
            if !position.in_transit {
                let smell_payload = DomainEventPayload::SmellEventTriggered {
                    room_id: position.room_id.clone(),
                    smell_type: "coffee".to_string(),
                    intensity: 0.8,
                    duration_ticks: 120,
                };
                let smell_event = DomainEvent::new(
                    smell_payload.event_type_str(),
                    &position.room_id,
                    &smell_payload.to_json(),
                    &correlation_id,
                    tick,
                );
                event_buffer.events.push(smell_event);
                active_smells.add(&position.room_id, "coffee".to_string(), 0.8, tick, 120);
            }
        }

        // Periodischer Bio-State Snapshot alle 20 Ticks (~20 Sekunden bei 1Hz)
        if tick.is_multiple_of(20) {
            let payload = DomainEventPayload::BioStateUpdated {
                agent_id: identity.agent_id,
                hunger: bio.hunger,
                energy: bio.energy,
                stress: bio.stress,
                bladder: bio.bladder,
                social_need: bio.social_need,
                caffeine_mg: bio.caffeine_mg,
                room_id: position.room_id.clone(),
                mood: format!("{:?}", mood.dominant_emotion),
                valence: mood.valence,
                arousal: mood.arousal,
            };
            let event = DomainEvent::new(
                payload.event_type_str(),
                &identity.agent_id.to_string(),
                &payload.to_json(),
                &uuid::Uuid::new_v4().to_string(),
                tick,
            );
            event_buffer.events.push(event);
        }
    }
}

/// 3. Berechnet Raum-Physik (Akustik, Temperatur, CO2)
///
/// Zaehlt Agenten pro Raum und berechnet physikalische Parameter.
/// Emittiert RoomPhysicsUpdated Events alle 20 Ticks fuer die Projection.
pub fn physics_system(
    query: Query<(&Position, Option<&WorkContext>)>,
    time: Res<SimulationTime>,
    room_distances: Option<Res<RoomDistanceMap>>,
    active_chaos: Option<Res<ActiveChaos>>,
    mut active_room_stimuli: ResMut<ActiveRoomStimuli>,
    mut room_physics_state: ResMut<RoomPhysicsState>,
    mut event_buffer: ResMut<EventBuffer>,
) {
    let tick = time.tick.0;
    active_room_stimuli.cleanup(tick);

    // Agenten pro Raum zaehlen und Meeting-Status ermitteln
    let mut room_agents: std::collections::HashMap<String, (usize, bool)> =
        std::collections::HashMap::new();

    // Initialisiere alle bekannten Raeume, damit leere Transit-Raeume keine
    // veralteten Physics-Werte im Read Model behalten.
    if let Some(distances) = room_distances.as_ref() {
        for room_id in distances.all_rooms() {
            room_agents.entry(room_id.clone()).or_insert((0, false));
        }
    }

    // Raeume mit aktivem Chaos muessen ebenfalls weiter berechnet werden,
    // auch wenn aktuell niemand darin sitzt.
    if let Some(chaos) = active_chaos.as_ref() {
        for room_id in chaos.active_rooms(tick) {
            room_agents.entry(room_id.to_string()).or_insert((0, false));
        }
    }
    for room_id in active_room_stimuli.active_rooms(tick) {
        room_agents.entry(room_id.to_string()).or_insert((0, false));
    }

    for (pos, work) in &query {
        if !pos.in_transit {
            let entry = room_agents.entry(pos.room_id.clone()).or_insert((0, false));
            entry.0 += 1;
            if let Some(w) = work {
                if w.in_meeting {
                    entry.1 = true;
                }
            }
        }
    }

    // Seed-Laerm ohne Nachbarraum-Einfluesse vorberechnen.
    let mut seed_noise: std::collections::HashMap<&str, f32> = std::collections::HashMap::new();
    for (room_id, (agent_count, has_meeting)) in &room_agents {
        let chaos_bonus = active_chaos
            .as_ref()
            .and_then(|chaos| chaos.get_active(room_id, tick))
            .map(|event| sentinel_physics::chaos_noise_bonus_db(event.event_type))
            .unwrap_or(0.0);
        let stimulus_noise = active_room_stimuli.delta_for(room_id, RoomStimulusType::Noise, tick);
        let local_noise =
            sentinel_physics::calculate_noise_level(*agent_count, *has_meeting, false, &[])
                + chaos_bonus
                + stimulus_noise;
        seed_noise.insert(room_id.as_str(), local_noise);
    }

    // Physik pro Raum berechnen
    for (room_id, (agent_count, has_meeting)) in &room_agents {
        let adjacent_noise = room_distances
            .as_ref()
            .map(|distances| {
                distances
                    .rooms_within(room_id, 1)
                    .into_iter()
                    .filter_map(|(adjacent_room, _)| seed_noise.get(adjacent_room).copied())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let active_chaos_event = active_chaos
            .as_ref()
            .and_then(|chaos| chaos.get_active(room_id, tick));
        let active_stimulus_delta_temperature =
            active_room_stimuli.delta_for(room_id, RoomStimulusType::Temperature, tick);
        let active_stimulus_delta_noise =
            active_room_stimuli.delta_for(room_id, RoomStimulusType::Noise, tick);
        let active_stimulus_delta_co2 =
            active_room_stimuli.delta_for(room_id, RoomStimulusType::Co2, tick);
        let chaos_elapsed_hours = active_chaos_event
            .map(|event| chaos_elapsed_hours(event, &time))
            .unwrap_or(0.0);

        let noise_db = sentinel_physics::calculate_noise_level(
            *agent_count,
            *has_meeting,
            false,
            &adjacent_noise,
        ) + active_chaos_event
            .map(|event| sentinel_physics::chaos_noise_bonus_db(event.event_type))
            .unwrap_or(0.0)
            + active_stimulus_delta_noise;

        let temperature =
            sentinel_physics::calculate_temperature(21.0, *agent_count, false, 15.0, 0.3)
                + active_chaos_event
                    .map(|event| {
                        sentinel_physics::chaos_temperature_delta_celsius(
                            event.event_type,
                            chaos_elapsed_hours,
                        )
                    })
                    .unwrap_or(0.0)
                + active_stimulus_delta_temperature;
        let co2 = sentinel_physics::calculate_co2(400.0, *agent_count, 0.5, 1.0)
            + active_stimulus_delta_co2;
        let has_active_stimulus = active_stimulus_delta_temperature.abs() > f32::EPSILON
            || active_stimulus_delta_noise.abs() > f32::EPSILON
            || active_stimulus_delta_co2.abs() > f32::EPSILON;
        let has_active_chaos = active_chaos_event.is_some();

        room_physics_state.set(
            room_id,
            tick,
            *agent_count as u32,
            temperature,
            co2,
            noise_db,
        );

        // Alle 20 Ticks Physics-Snapshot als Event emittieren
        // Bei aktivem Chaos/Stimulus: jeden Tick emittieren (Echtzeit-Feedback)
        if tick > 0 && (tick.is_multiple_of(20) || has_active_stimulus || has_active_chaos) {
            let payload = DomainEventPayload::RoomPhysicsUpdated {
                room_id: room_id.to_string(),
                temperature,
                co2_ppm: co2,
                noise_db,
                occupant_count: *agent_count as u32,
            };
            let event = DomainEvent::new(
                payload.event_type_str(),
                room_id,
                &payload.to_json(),
                &uuid::Uuid::new_v4().to_string(),
                tick,
            );
            event_buffer.events.push(event);
        }
    }
}

/// 4. Verarbeitet Raumwechsel (Transit-Timer runterzaehlen)
///
/// Erzeugt TransitCompleted DomainEvents bei abgeschlossenem Transit.
pub fn transit_system(
    mut query: Query<(&AgentIdentity, &mut Position)>,
    time: Res<SimulationTime>,
    mut event_buffer: ResMut<EventBuffer>,
) {
    let delta_ms = (time.delta_seconds * 1000.0) as u32;
    for (identity, mut pos) in &mut query {
        if pos.in_transit {
            pos.transit_remaining_ms = pos.transit_remaining_ms.saturating_sub(delta_ms);
            if pos.transit_remaining_ms == 0 {
                // Transit abgeschlossen: Agent kommt im Zielraum an
                let target_room = pos.transit_target.take();
                if let Some(target) = target_room {
                    pos.room_id = target.clone();

                    // DomainEvent: TransitCompleted (mit Causation-Chain vom Move-Action)
                    let payload = DomainEventPayload::TransitCompleted {
                        agent_id: identity.agent_id,
                        room_id: target,
                    };
                    let correlation = pos
                        .transit_correlation_id
                        .take()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    let event = DomainEvent::new(
                        payload.event_type_str(),
                        &identity.agent_id.to_string(),
                        &payload.to_json(),
                        &correlation,
                        time.tick.0,
                    );
                    event_buffer.events.push(event);
                }
                pos.in_transit = false;
            }
        }
    }
}

/// Cooldown-Dauer in Ticks fuer Conflict-Stress nach stressausloesendem Chaos-Event
const CONFLICT_COOLDOWN_TICKS: u32 = 120;
const DEFAULT_ROOM_STIMULUS_DURATION_TICKS: u64 = 120;

/// 4b. Deriviert WorkContext automatisch aus Raum, Zeit und Conflict-State.
///
/// - `in_meeting`: Agent in Meetingraum mit >= 2 Agents
/// - `has_deadline`: Nachmittagsdruck 14-17 Uhr
/// - `has_conflict`: Zerfallender Cooldown nach Chaos-Events (gesetzt im chaos_system)
pub fn work_context_system(
    mut agents: Query<(&Position, &mut WorkContext)>,
    time: Res<SimulationTime>,
) {
    // Raum-Belegung zaehlen (immutabler Pass)
    let room_counts: std::collections::HashMap<String, usize> = {
        let mut counts = std::collections::HashMap::new();
        for (pos, _) in agents.iter() {
            if !pos.in_transit {
                *counts.entry(pos.room_id.clone()).or_insert(0) += 1;
            }
        }
        counts
    };

    // WorkContext aktualisieren (mutabler Pass)
    for (pos, mut work) in &mut agents {
        // in_meeting: Agent in meetingraum-* mit >= 2 Agents im selben Raum
        let is_meetingroom = pos.room_id.starts_with("meetingraum");
        let occupancy = room_counts.get(&pos.room_id).copied().unwrap_or(0);
        work.in_meeting = is_meetingroom && occupancy >= 2;

        // has_deadline: Nachmittagsdruck 14-17 Uhr
        work.has_deadline = (14.0..17.0).contains(&time.sim_hour);

        // conflict_cooldown Decay + has_conflict Flag
        if work.conflict_cooldown > 0 {
            work.conflict_cooldown -= 1;
        }
        work.has_conflict = work.conflict_cooldown > 0;
    }
}

/// Parst Tool-Content. Format: `tool:NAME:INPUT` oder JSON `{"tool":"NAME","input":"..."}`.
///
/// Gibt `None` zurueck wenn der Content kein Tool-Call ist (normaler WorkContext-Text).
fn parse_tool_content(content: &str) -> Option<(String, String)> {
    // Versuche JSON zuerst
    if content.starts_with('{') {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) {
            let tool = parsed.get("tool")?.as_str()?.to_string();
            let input = parsed
                .get("input")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Some((tool, input));
        }
    }
    // Fallback: "tool:NAME:INPUT"
    if let Some(rest) = content.strip_prefix("tool:") {
        let (name, input) = rest.split_once(':').unwrap_or((rest, ""));
        return Some((name.to_string(), input.to_string()));
    }
    None
}

#[cfg(feature = "wasm")]
fn build_agent_snapshot(
    identity: &AgentIdentity,
    bio: &BioState,
    position: &Position,
) -> sentinel_wasm::AgentSnapshot {
    sentinel_wasm::AgentSnapshot {
        agent_id: format!("AGENT-{:02}", identity.agent_id.0),
        name: identity.name.clone(),
        role: identity.role.clone(),
        hunger: bio.hunger,
        energy: bio.energy,
        stress: bio.stress,
        social_need: bio.social_need,
        caffeine: bio.caffeine_mg,
        bladder: bio.bladder,
        room_id: position.room_id.clone(),
    }
}

#[cfg(feature = "wasm")]
fn build_room_snapshots(
    room_physics_state: Option<&RoomPhysicsState>,
    position: &Position,
) -> HashMap<String, sentinel_wasm::RoomSnapshot> {
    let mut rooms = HashMap::new();

    if let Some(state) = room_physics_state {
        for (room_id, snapshot) in &state.rooms {
            rooms.insert(
                room_id.clone(),
                sentinel_wasm::RoomSnapshot {
                    room_id: room_id.clone(),
                    name: room_id_to_german(room_id),
                    floor: room_floor(room_id),
                    temperature: snapshot.temperature,
                    noise_db: snapshot.noise_db,
                    occupant_count: snapshot.occupant_count,
                },
            );
        }
    }

    rooms
        .entry(position.room_id.clone())
        .or_insert_with(|| sentinel_wasm::RoomSnapshot {
            room_id: position.room_id.clone(),
            name: room_id_to_german(&position.room_id),
            floor: room_floor(&position.room_id),
            temperature: 21.6,
            noise_db: 30.0,
            occupant_count: 0,
        });

    rooms
}

/// Prueft ob ein Chaos-EventType stressausloesend ist
fn is_stressful_chaos(event_type: sentinel_common::EventType) -> bool {
    matches!(
        event_type,
        sentinel_common::EventType::PrinterBroken
            | sentinel_common::EventType::FireAlarmDrill
            | sentinel_common::EventType::AirConBroken
            | sentinel_common::EventType::InternetOutage
    )
}

struct InjectedChaosMetadata<'a> {
    event_id: &'a str,
    correlation_id: &'a str,
    operation_id: &'a str,
}

struct InjectedStimulusMetadata<'a> {
    event_id: &'a str,
    correlation_id: &'a str,
    operation_id: &'a str,
}

/// Wendet ein Chaos-Event einheitlich auf ECS-State und Event-Trail an.
fn inject_chaos_event(
    event_type: EventType,
    target_room: &str,
    description: &str,
    tick: u64,
    duration_ticks: u64,
    event_buffer: &mut EventBuffer,
    active_chaos: &mut ActiveChaos,
    agents: &mut Query<(&Position, &mut WorkContext)>,
    metadata: Option<InjectedChaosMetadata<'_>>,
) {
    active_chaos.set(
        target_room,
        event_type,
        description.to_string(),
        tick,
        duration_ticks,
    );

    let payload = DomainEventPayload::ChaosTriggered {
        event_type,
        target_room: Some(target_room.to_string()),
        description: description.to_string(),
    };
    let correlation_id = metadata
        .as_ref()
        .map(|meta| meta.correlation_id.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut event = DomainEvent::new(
        payload.event_type_str(),
        target_room,
        &payload.to_json(),
        &correlation_id,
        tick,
    );
    if let Some(meta) = metadata {
        event.event_id = meta.event_id.to_string();
        event = event.with_operation_id(meta.operation_id);
    }
    event_buffer.events.push(event);

    // Stressausloesende Chaos-Events setzen conflict_cooldown auf Agents im Raum
    if is_stressful_chaos(event_type) {
        for (pos, mut work) in agents.iter_mut() {
            if !pos.in_transit && pos.room_id == target_room {
                work.conflict_cooldown = CONFLICT_COOLDOWN_TICKS;
            }
        }
    }
}

/// Wendet einen direkten Raumreiz einheitlich auf Physics-State und Event-Trail an.
fn inject_room_stimulus(
    room_id: &str,
    stimulus_type: RoomStimulusType,
    delta: f32,
    description: &str,
    tick: u64,
    duration_ticks: u64,
    event_buffer: &mut EventBuffer,
    active_room_stimuli: &mut ActiveRoomStimuli,
    metadata: Option<InjectedStimulusMetadata<'_>>,
) {
    active_room_stimuli.set(
        room_id,
        stimulus_type,
        delta,
        description.to_string(),
        tick,
        duration_ticks,
    );

    let payload = DomainEventPayload::RoomStimulusApplied {
        room_id: room_id.to_string(),
        stimulus_type,
        delta,
        duration_ticks,
        description: description.to_string(),
    };
    let correlation_id = metadata
        .as_ref()
        .map(|meta| meta.correlation_id.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut event = DomainEvent::new(
        payload.event_type_str(),
        room_id,
        &payload.to_json(),
        &correlation_id,
        tick,
    );
    if let Some(meta) = metadata {
        event.event_id = meta.event_id.to_string();
        event = event.with_operation_id(meta.operation_id);
    }
    event_buffer.events.push(event);
}

fn chaos_elapsed_hours(active_chaos: &ActiveChaosEvent, time: &SimulationTime) -> f32 {
    let elapsed_ticks = time.tick.0.saturating_sub(active_chaos.created_tick) as f32;
    (elapsed_ticks * time.delta_seconds.max(0.0)) / 3600.0
}

/// 5. Generiert Zufallsereignisse (Poisson-verteilt)
///
/// Nutzt splitmix64-basierte Pseudo-Zufallszahlen fuer gleichmaessige Verteilung.
/// Globaler Cooldown verhindert Event-Flut: max 1 Chaos-Event alle 30 Ticks.
/// Stressausloesende Events (PrinterBroken, FireAlarm, AirCon, Internet) setzen
/// conflict_cooldown auf Agents im betroffenen Raum.
pub fn chaos_system(
    time: Res<SimulationTime>,
    mut event_buffer: ResMut<EventBuffer>,
    mut active_chaos: ResMut<ActiveChaos>,
    mut agents: Query<(&Position, &mut WorkContext)>,
) {
    active_chaos.cleanup(time.tick.0);

    // SENTINEL_CHAOS_ENABLED runtime gate (Issue #233 AC-3)
    if !sentinel_common::feature_flags::RuntimeFlags::global().chaos_enabled {
        return;
    }

    let tick = time.tick.0;

    // Globaler Cooldown: max 1 Chaos-Event alle 30 Ticks (~30 Sekunden bei 1Hz)
    // Ueber Tick-Modulo gesteuert — kein zusaetzlicher State noetig
    if !tick.is_multiple_of(30) {
        return;
    }

    // Besetzte Raeume sammeln fuer realistische Chaos-Zuweisung
    let occupied_rooms: Vec<String> = {
        let mut rooms: Vec<String> = agents
            .iter()
            .filter(|(p, _)| !p.in_transit)
            .map(|(p, _)| p.room_id.clone())
            .collect();
        rooms.sort();
        rooms.dedup();
        rooms
    };

    // splitmix64-basierte Hash-Funktion (gut verteilt, auch fuer kleine Seeds)
    let pseudo_rng = |seed: u64| -> f32 {
        let mut x = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        (x % 10000) as f32 / 10000.0
    };

    // Chaos-Event-Typen mit zugehoerigen sentinel_common::EventType Mappings
    let event_types: [(
        sentinel_physics::ChaosEventType,
        sentinel_common::EventType,
        &str,
    ); 8] = [
        (
            sentinel_physics::ChaosEventType::PhoneRing,
            sentinel_common::EventType::PhoneRing,
            "Telefon klingelt",
        ),
        (
            sentinel_physics::ChaosEventType::PrinterBroken,
            sentinel_common::EventType::PrinterBroken,
            "Drucker defekt",
        ),
        (
            sentinel_physics::ChaosEventType::PackageDelivery,
            sentinel_common::EventType::PackageDelivery,
            "Paketlieferung",
        ),
        (
            sentinel_physics::ChaosEventType::SBahnDelay,
            sentinel_common::EventType::SBahnDelay,
            "S-Bahn Verspaetung",
        ),
        (
            sentinel_physics::ChaosEventType::FireAlarmDrill,
            sentinel_common::EventType::FireAlarmDrill,
            "Feueralarm-Uebung",
        ),
        (
            sentinel_physics::ChaosEventType::CakeInKitchen,
            sentinel_common::EventType::CakeInKitchen,
            "Kuchen in der Kueche",
        ),
        (
            sentinel_physics::ChaosEventType::AirConBroken,
            sentinel_common::EventType::AirConBroken,
            "Klimaanlage defekt",
        ),
        (
            sentinel_physics::ChaosEventType::InternetOutage,
            sentinel_common::EventType::InternetOutage,
            "Internetausfall",
        ),
    ];

    // delta_seconds korrigiert fuer den Cooldown-Faktor (30 Ticks gebuendelt)
    let effective_delta = time.delta_seconds * 30.0;

    for (i, (physics_type, common_type, description)) in event_types.iter().enumerate() {
        let freq = sentinel_physics::chaos_frequency_per_hour(*physics_type);
        let rng = pseudo_rng(tick.wrapping_mul(31).wrapping_add(i as u64));
        let triggered = sentinel_physics::should_trigger_chaos(freq, effective_delta, rng);

        if triggered {
            // Zufaelligen besetzten Raum waehlen (Fallback: "empfang")
            let target = if occupied_rooms.is_empty() {
                "empfang".to_string()
            } else {
                let room_rng = pseudo_rng(tick.wrapping_mul(97).wrapping_add(i as u64));
                let idx = (room_rng * occupied_rooms.len() as f32) as usize;
                occupied_rooms[idx.min(occupied_rooms.len() - 1)].clone()
            };
            inject_chaos_event(
                *common_type,
                &target,
                description,
                time.tick.0,
                sentinel_physics::default_chaos_duration_ticks(*common_type),
                &mut event_buffer,
                &mut active_chaos,
                &mut agents,
                None,
            );
        }
    }
}

/// 6. Berechnet Stimmung aus Bio-Zustand und Arbeitskontext
///
/// Valenz-Arousal-Modell:
/// - Valenz (-1 bis +1): gewichtete Kombination aus Energie, Stress, Hunger, Sozialbedarf
/// - Arousal (0 bis 1): Stress + Koffein + Meeting-Aktivitaet
/// - Dominante Emotion: Quadranten-Mapping
pub fn mood_system(mut query: Query<(&BioState, &mut Mood, &WorkContext)>) {
    for (bio, mut mood, work) in &mut query {
        // Valenz: positiv = gute Energie, wenig Stress
        let energy_factor = (bio.energy - 50.0) / 50.0;
        let stress_factor = -bio.stress / 100.0;
        let hunger_factor = -(bio.hunger - 30.0).max(0.0) / 70.0;
        let social_factor = -(bio.social_need - 70.0).max(0.0) / 30.0;

        mood.valence = (0.4 * energy_factor
            + 0.3 * stress_factor
            + 0.15 * hunger_factor
            + 0.15 * social_factor)
            .clamp(-1.0, 1.0);

        // Arousal: Stress + Koffein + Meeting-Aktivierung
        let stress_arousal = bio.stress / 100.0;
        let caffeine_arousal = (bio.caffeine_mg / 200.0).min(1.0);
        let meeting_arousal = if work.in_meeting { 0.2 } else { 0.0 };

        mood.arousal =
            (0.5 * stress_arousal + 0.3 * caffeine_arousal + 0.2 * meeting_arousal).clamp(0.0, 1.0);

        // Dominante Emotion aus Valenz-Arousal-Quadranten
        mood.dominant_emotion = valence_arousal_to_emotion(mood.valence, mood.arousal);
    }
}

/// Mappt Valenz+Arousal auf eine dominante Emotion
fn valence_arousal_to_emotion(valence: f32, arousal: f32) -> Emotion {
    match (valence, arousal) {
        (v, a) if v > 0.3 && a > 0.5 => Emotion::Excited,
        (v, a) if v > 0.3 && a <= 0.5 => Emotion::Relaxed,
        (v, _) if v > 0.0 => Emotion::Happy,
        (v, a) if v < -0.3 && a > 0.5 => Emotion::Stressed,
        (v, a) if v < -0.3 && a <= 0.5 => Emotion::Tired,
        (v, _) if v < 0.0 => Emotion::Frustrated,
        (_, a) if a > 0.5 => Emotion::Focused,
        (_, a) if a < 0.2 => Emotion::Bored,
        _ => Emotion::Neutral,
    }
}

/// 7. Generiert Wahrnehmungstext fuer LLM-Prompt
///
/// Wandelt numerische Bio/Mood/Position-Werte in natuerlichsprachliche
/// deutsche Beschreibungen um, die der Agent-LLM als Kontext erhaelt.
pub fn perception_system(
    mut query: Query<(&BioState, &Position, &Mood, &mut PerceptionState)>,
    time: Res<SimulationTime>,
    active_smells: Res<super::world::ActiveSmells>,
) {
    for (bio, position, mood, mut perception) in &mut query {
        // Koerper-Wahrnehmung
        let mut body_parts: Vec<&str> = Vec::new();

        if bio.hunger > 70.0 {
            body_parts.push("Du hast grossen Hunger.");
        } else if bio.hunger > 40.0 {
            body_parts.push("Du spuerst leichten Hunger.");
        }

        if bio.energy < 30.0 {
            body_parts.push("Du bist sehr muede.");
        } else if bio.energy < 50.0 {
            body_parts.push("Du fuehlst dich etwas schlapp.");
        } else if bio.energy > 85.0 {
            body_parts.push("Du fuehlst dich voller Energie.");
        }

        if bio.caffeine_mg > 80.0 {
            body_parts.push("Der Kaffee wirkt, du bist wach und konzentriert.");
        } else if bio.caffeine_mg > 30.0 {
            body_parts.push("Du spuerst noch etwas Koffein.");
        }

        if bio.bladder > 70.0 {
            body_parts.push("Du musst dringend auf die Toilette.");
        } else if bio.bladder > 40.0 {
            body_parts.push("Du bemerkst leichten Blasendrang.");
        }

        if bio.stress > 70.0 {
            body_parts.push("Du bist sehr gestresst.");
        } else if bio.stress > 40.0 {
            body_parts.push("Du fuehlst leichten Stress.");
        }

        perception.body_text = body_parts.join(" ");

        // Umgebungs-Wahrnehmung
        let room_desc = room_id_to_german(&position.room_id);
        perception.environment_text = if position.in_transit {
            format!(
                "Du bist auf dem Weg von {} nach {}.",
                room_desc,
                position
                    .transit_target
                    .as_deref()
                    .map(room_id_to_german)
                    .unwrap_or_else(|| "unbekannt".to_string())
            )
        } else {
            format!("Du bist {}.", room_desc)
        };

        // Aktive Gerueche im aktuellen Raum in die Umgebungswahrnehmung injizieren
        if !position.in_transit {
            let current_smells = active_smells.get_active(&position.room_id, time.tick.0);
            for smell in &current_smells {
                if smell.intensity > 0.3 {
                    let text = match smell.smell_type.as_str() {
                        "coffee" => " Du riechst Kaffeeduft.",
                        "food" => " Es riecht nach Essen.",
                        _ => "",
                    };
                    if !text.is_empty() {
                        debug!(
                            room = %position.room_id,
                            smell_type = %smell.smell_type,
                            intensity = smell.intensity,
                            "Smell injected into perception"
                        );
                        perception.environment_text.push_str(text);
                    }
                }
            }
        }

        // Stimmungs-basierte soziale Wahrnehmung
        perception.social_text = if bio.social_need > 70.0 {
            "Du hast das Beduerfnis, mit jemandem zu reden.".to_string()
        } else if bio.social_need < 20.0 {
            "Du moechtest gerade lieber allein sein.".to_string()
        } else {
            String::new()
        };

        // Stimmungstext anhaengen wenn markant
        let mood_text = match mood.dominant_emotion {
            Emotion::Excited => "Du bist aufgeregt und voller Tatendrang.",
            Emotion::Stressed => "Du fuehlst dich unter Druck.",
            Emotion::Tired => "Du fuehlst dich erschoepft.",
            Emotion::Frustrated => "Du bist etwas frustriert.",
            Emotion::Bored => "Dir ist langweilig.",
            Emotion::Relaxed => "Du fuehlst dich entspannt und zufrieden.",
            _ => "",
        };
        if !mood_text.is_empty() {
            if !perception.body_text.is_empty() {
                perception.body_text.push(' ');
            }
            perception.body_text.push_str(mood_text);
        }

        perception.last_updated = time.tick;
    }
}

/// Mappt Raum-ID auf deutschen Beschreibungstext
fn room_id_to_german(room_id: &str) -> String {
    match room_id {
        "empfang" => "im Empfangsbereich".to_string(),
        "flur-eg" => "im Flur des Erdgeschosses".to_string(),
        "flur-og" => "im Flur des Obergeschosses".to_string(),
        "kueche" => "in der Kueche".to_string(),
        "buero-dev-1" => "im Entwicklerbuero 1".to_string(),
        "buero-dev-2" => "im Entwicklerbuero 2".to_string(),
        "buero-design-1" => "im Designbuero 1".to_string(),
        "buero-design-2" => "im Designbuero 2".to_string(),
        "buero-ceo" => "im Buero der Geschaeftsfuehrung".to_string(),
        "meetingraum-01" => "im Meetingraum 1 (EG)".to_string(),
        "meetingraum-02" => "im Meetingraum 2 (OG)".to_string(),
        "meetingraum-03" => "im Meetingraum 3 (OG)".to_string(),
        "toilette-eg-damen" => "auf der Damentoilette (EG)".to_string(),
        "toilette-eg-herren" => "auf der Herrentoilette (EG)".to_string(),
        "toilette-og-damen" => "auf der Damentoilette (OG)".to_string(),
        "toilette-og-herren" => "auf der Herrentoilette (OG)".to_string(),
        "treppenhaus" => "im Treppenhaus".to_string(),
        other => format!("im Raum '{}'", other),
    }
}

#[cfg(feature = "wasm")]
fn room_floor(room_id: &str) -> u32 {
    if room_id.ends_with("-og") || room_id.contains("ceo") {
        1
    } else {
        0
    }
}

type OutputAgentQueryData = (
    &'static AgentIdentity,
    &'static BioState,
    &'static Position,
    &'static Personality,
    &'static ShiftInfo,
    &'static PerceptionState,
    &'static WorkContext,
    &'static EventQueue,
);

/// 9. Sendet Wahrnehmung via Channel an externen Zenoh-Publisher.
///
/// Serialisiert PerceptionState + Position + EventQueue zu Perception-Message
/// und sendet sie ueber den PerceptionSender Channel.
/// impulse_text wird aus der priorisierten EventQueue generiert.
pub fn output_system(
    sender: Option<Res<PerceptionSender>>,
    active_smells: Res<super::world::ActiveSmells>,
    room_physics_state: Res<RoomPhysicsState>,
    query: Query<OutputAgentQueryData>,
    time: Res<SimulationTime>,
) {
    let Some(sender) = sender else { return };

    let present_agents_by_room: HashMap<String, Vec<(String, String)>> = query
        .iter()
        .filter(|(_, _, position, _, _, _, _, _)| !position.in_transit)
        .fold(
            HashMap::new(),
            |mut acc, (identity, _, position, _, _, _, work_ctx, _)| {
                let activity = work_ctx
                    .current_task
                    .clone()
                    .unwrap_or_else(|| "anwesend".to_string());
                acc.entry(position.room_id.clone())
                    .or_default()
                    .push((identity.name.clone(), activity));
                acc
            },
        );

    for (identity, bio, position, personality, shift, perception, work_ctx, queue) in &query {
        let impulse_text = super::decision::format_impulse_from_queue(queue);
        let focus_hours = focus_hours_since_shift_start(time.sim_hour, shift);
        let sim_time = format!("{:02.0}:00 Uhr", time.sim_hour.floor());

        let room_snapshot = room_physics_state.get(&position.room_id);
        let room_noise_db = room_snapshot
            .map(|snapshot| snapshot.noise_db)
            .unwrap_or(30.0);
        let room_temp_c = room_snapshot
            .map(|snapshot| snapshot.temperature)
            .unwrap_or(21.6);
        let room_co2_ppm = room_snapshot
            .map(|snapshot| snapshot.co2_ppm)
            .unwrap_or(400.0);

        let smells = if position.in_transit {
            Vec::new()
        } else {
            active_smells
                .get_active(&position.room_id, time.tick.0)
                .into_iter()
                .map(|smell| super::perception::SmellEvent {
                    source_room: position.room_id.clone(),
                    smell_type: smell.smell_type.clone(),
                    intensity: smell.intensity,
                    radius_rooms: 0,
                    decay_per_room: 0.0,
                    created_tick: smell.created_tick,
                    duration_ticks: smell.duration_ticks,
                })
                .collect()
        };

        let present_agents = present_agents_by_room
            .get(&position.room_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| name != &identity.name)
            .collect::<Vec<_>>();

        let prompt_perception = super::perception::generate_perception(
            bio,
            position,
            personality,
            room_noise_db,
            room_temp_c,
            room_co2_ppm,
            &smells,
            &present_agents,
            &sim_time,
            focus_hours,
        );

        let mut environment_text = perception.environment_text.clone();
        if !position.in_transit {
            environment_text = format!("Du bist {}.", room_id_to_german(&position.room_id));
            if !prompt_perception.environment_text.is_empty() {
                environment_text.push(' ');
                environment_text.push_str(&prompt_perception.environment_text);
            }
        }

        let body_text = if perception.body_text.is_empty() {
            prompt_perception.body_text.clone()
        } else {
            perception.body_text.clone()
        };

        let presence_text = if prompt_perception.presence_text.is_empty() {
            work_ctx
                .current_task
                .clone()
                .unwrap_or_else(|| "Du bist allein.".to_string())
        } else {
            prompt_perception.presence_text.clone()
        };

        let msg = Perception {
            agent_id: identity.agent_id,
            circadian_text: prompt_perception.circadian_text,
            body_text,
            environment_text,
            acoustic_text: prompt_perception.acoustic_text,
            presence_text,
            impulse_text,
            timestamp: Timestamp(time.tick.0),
            tick: time.tick,
        };
        // try_send: Non-blocking. Bei vollem Channel wird die Perception gedroppt
        // (naechster Tick liefert frische Daten).
        let _ = sender.0.try_send(msg);
    }
}

fn focus_hours_since_shift_start(sim_hour: f32, shift: &ShiftInfo) -> f32 {
    if !shift.is_on_duty {
        return 0.0;
    }

    let current = sim_hour.rem_euclid(24.0);
    let start = f32::from(shift.shift_start_hour);
    let duration = if shift.shift_end_hour >= shift.shift_start_hour {
        f32::from(shift.shift_end_hour - shift.shift_start_hour)
    } else {
        f32::from((24 - shift.shift_start_hour) + shift.shift_end_hour)
    };

    let elapsed = if current >= start {
        current - start
    } else {
        current + 24.0 - start
    };

    elapsed.clamp(0.0, duration.max(0.0))
}

/// 10. Erkennt Flurbegegnungen zwischen Agents die gleichzeitig in Transit sind.
///
/// Paarweise Pruefung aller in-transit Agents (splitmix64-basierte Zufallsentscheidung,
/// 30% Wahrscheinlichkeit). Laeuft alle 10 Ticks um Event-Flut zu vermeiden.
pub fn encounter_system(
    query: Query<(&AgentIdentity, &Position)>,
    time: Res<SimulationTime>,
    mut event_buffer: ResMut<EventBuffer>,
) {
    let tick = time.tick.0;

    // Alle 3 Ticks pruefen (typisch n<5 in Transit, O(n^2) bei max 105 Paaren ist guenstig)
    if tick == 0 || !tick.is_multiple_of(3) {
        return;
    }

    // Sammle alle in-transit Agents
    let in_transit: Vec<_> = query
        .iter()
        .filter(|(_, pos)| pos.in_transit)
        .map(|(id, _)| id.agent_id)
        .collect();

    // Paarweise Encounter-Check (O(n^2/2), typisch n<5 in Transit)
    for i in 0..in_transit.len() {
        for j in (i + 1)..in_transit.len() {
            // splitmix64 deterministische RNG basierend auf Tick + Agent-IDs
            let seed = tick
                .wrapping_mul(31)
                .wrapping_add(in_transit[i].0 as u64 * 97)
                .wrapping_add(in_transit[j].0 as u64 * 53);
            let mut x = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^= x >> 31;
            let rng = (x % 10000) as f32 / 10000.0;

            if sentinel_physics::transit::check_hallway_encounter(true, true, rng) {
                let payload = DomainEventPayload::HallwayEncounterDetected {
                    agent_a: in_transit[i],
                    agent_b: in_transit[j],
                    location: "flur".to_string(),
                };
                let event = DomainEvent::new(
                    payload.event_type_str(),
                    &format!("{}-{}", in_transit[i], in_transit[j]),
                    &payload.to_json(),
                    &uuid::Uuid::new_v4().to_string(),
                    tick,
                );
                event_buffer.events.push(event);
            }
        }
    }
}

/// 11. Generiert und verwaltet Geruchsereignisse in Raeumen.
///
/// Erzeugt SmellEvents bei Bio-Aktionen (drink_coffee → Coffee-Smell in aktuellem Raum).
/// Laeuft alle 20 Ticks synchron mit BioStateUpdated-Snapshots.
pub fn smell_system(
    query: Query<(&AgentIdentity, &Position, &BioState)>,
    time: Res<SimulationTime>,
    mut event_buffer: ResMut<EventBuffer>,
    mut active_smells: ResMut<super::world::ActiveSmells>,
) {
    let tick = time.tick.0;

    // Cleanup abgelaufener Smells bei JEDEM Tick
    active_smells.cleanup(tick);

    // Kaffee-Geruch erzeugen wenn Auto-Coffee getriggert hat (gleicher Tick-Modulo wie bio_system)
    if tick == 0 || !tick.is_multiple_of(180) {
        return;
    }

    for (_identity, position, bio) in &query {
        // Wenn Agent gerade Kaffee getrunken hat (caffeine > 90 = frisch getrunken)
        // UND im passenden Raum ist (nicht in Transit)
        if bio.caffeine_mg > 90.0 && !position.in_transit {
            let room = &position.room_id;
            let payload = DomainEventPayload::SmellEventTriggered {
                room_id: room.clone(),
                smell_type: "coffee".to_string(),
                intensity: 0.8,
                duration_ticks: 120, // 2 Minuten bei 1Hz
            };
            let event = DomainEvent::new(
                payload.event_type_str(),
                room,
                &payload.to_json(),
                &uuid::Uuid::new_v4().to_string(),
                tick,
            );
            event_buffer.events.push(event);
            active_smells.add(room, "coffee".to_string(), 0.8, tick, 120);
        }
    }
}

/// 12. Persistiert Zustand: Events nach Limbo, Snapshots nach redb (BATCHED).
///
/// Zwei Schreib-Pfade (Dual-Write):
/// 1. Events aus EventBuffer → Limbo Event Store (append-only + Outbox)
/// 2. State-Snapshots → redb (alle N Ticks, wie bisher)
pub fn persist_system(
    query: Query<(&AgentIdentity, &Position, &BioState, &Mood)>,
    time: Res<SimulationTime>,
    store: Option<Res<RedbStateStore>>,
    event_store: Option<Res<LimboEventStore>>,
    mut event_buffer: ResMut<EventBuffer>,
    mut telemetry: ResMut<PersistTelemetry>,
    fanout_sender: Option<Res<ZenohFanoutSender>>,
) {
    telemetry.ticks_observed = telemetry.ticks_observed.saturating_add(1);

    // TickSnapshot-Marker alle 60 Ticks (~1 Minute bei 1Hz)
    let tick = time.tick.0;
    if tick.is_multiple_of(60) && tick > 0 {
        let agent_count = query.iter().count() as u32;
        let payload = DomainEventPayload::TickSnapshot { tick, agent_count };
        let event = DomainEvent::new(
            payload.event_type_str(),
            "simulation",
            &payload.to_json(),
            &uuid::Uuid::new_v4().to_string(),
            tick,
        );
        event_buffer.events.push(event);
    }

    // 1. Events aus Buffer nach Limbo schreiben (mit Outbox) + Zenoh Fan-Out
    if let Some(es) = &event_store {
        for event in event_buffer.events.drain(..) {
            let topic = event_topic(&event);
            if let Err(err) = es.0.append_with_outbox(&event, &topic) {
                warn!(event_id = %event.event_id, "persist_system: failed to write event: {err}");
            }
            // Nach erfolgreichem Limbo-Write: Event an Zenoh Fan-Out Bridge senden
            if let Some(ref fanout) = fanout_sender {
                if fanout.sender.try_send(event).is_err() {
                    // Channel voll — Event droppen (Limbo ist SSOT, Zenoh ist best-effort)
                }
            }
        }
    } else {
        // Kein Event Store: Buffer leeren damit er nicht unendlich waechst
        event_buffer.events.clear();
    }

    // 2. Bestehende redb State-Snapshot Logik (unveraendert)
    let Some(store) = store else {
        telemetry.enabled = false;
        telemetry.interval_ticks = 0;
        return;
    };

    telemetry.enabled = true;
    telemetry.interval_ticks = store.persist_every_n_ticks.max(1);

    if !time.tick.0.is_multiple_of(store.persist_every_n_ticks) {
        telemetry.skipped_ticks = telemetry.skipped_ticks.saturating_add(1);
        return;
    }

    let mut batch: Vec<(sentinel_common::AgentId, Vec<u8>)> = Vec::new();
    for (identity, position, bio, mood) in &query {
        batch.push((
            identity.agent_id,
            encode_agent_snapshot(time.tick.0, position, bio, mood),
        ));
    }

    let batch_len = batch.len() as u64;
    telemetry.batch_size_last = batch_len;
    telemetry.batch_size_sum = telemetry.batch_size_sum.saturating_add(batch_len);
    telemetry.batch_size_max = telemetry.batch_size_max.max(batch_len);
    telemetry.flush_attempts = telemetry.flush_attempts.saturating_add(1);

    let flush_start = Instant::now();
    if let Err(err) = store.store.set_agent_states_batch(&batch) {
        telemetry.flush_failures = telemetry.flush_failures.saturating_add(1);
        let flush_us = flush_start.elapsed().as_secs_f64() * 1_000_000.0;
        telemetry.flush_latency_us_sum += flush_us;
        telemetry.flush_latency_us_max = telemetry.flush_latency_us_max.max(flush_us);
        warn!("persist_system: failed to write redb batch: {err}");
    } else {
        telemetry.flush_success = telemetry.flush_success.saturating_add(1);
        let flush_us = flush_start.elapsed().as_secs_f64() * 1_000_000.0;
        telemetry.flush_latency_us_sum += flush_us;
        telemetry.flush_latency_us_max = telemetry.flush_latency_us_max.max(flush_us);
    }
}

/// Bestimmt das Zenoh-Topic fuer ein DomainEvent.
fn event_topic(event: &DomainEvent) -> String {
    format!(
        "sentinel/events/{}/{}",
        event.event_type, event.aggregate_id
    )
}

fn encode_agent_snapshot(tick: u64, position: &Position, bio: &BioState, mood: &Mood) -> Vec<u8> {
    format!(
        "t={tick};room={};transit={};h={:.2};e={:.2};caf={:.2};b={:.2};s={:.2};sn={:.2};v={:.3};a={:.3};emo={:?}",
        position.room_id,
        if position.in_transit { 1 } else { 0 },
        bio.hunger,
        bio.energy,
        bio.caffeine_mg,
        bio.bladder,
        bio.stress,
        bio.social_need,
        mood.valence,
        mood.arousal,
        mood.dominant_emotion
    )
    .into_bytes()
}
