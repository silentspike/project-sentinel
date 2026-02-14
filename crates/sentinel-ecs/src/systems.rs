//! ECS Systems fuer Agent-Simulation.
//!
//! Definiert 10 Systems in strikter Ausfuehrungsreihenfolge:
//! 1. input_system - Empfaengt Agent-Aktionen via Channel
//! 2. bio_system - Aktualisiert biologische Zustaende (sentinel-bio)
//! 3. physics_system - Berechnet Raum-Physik (sentinel-physics)
//! 4. transit_system - Verarbeitet Raumwechsel + Transit-Events
//! 5. chaos_system - Generiert Zufallsereignisse + Chaos-Events
//! 6. mood_system - Berechnet Stimmung aus Bio+Kontext
//! 7. perception_system - Generiert Wahrnehmungstext fuer LLM-Prompt
//! 8. decision_system - Priorisiert Events fuer impulse_text (P0-P3)
//! 9. output_system - Sendet Wahrnehmung via Channel
//! 10. persist_system - Persistiert Events (Limbo) + State-Snapshots (redb)

use super::components::*;
use super::world::{
    ActionReceiver, EventBuffer, LimboEventStore, PersistTelemetry, RedbStateStore,
};
use super::world::{PerceptionSender, SimulationTime};
use bevy_ecs::prelude::*;
use sentinel_common::{
    ActionType, DomainEvent, DomainEventPayload, Emotion, Perception, Timestamp,
};
use std::time::Instant;
use tracing::warn;

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
    mut query: Query<(
        &AgentIdentity,
        &mut Position,
        &mut WorkContext,
        &mut BioState,
    )>,
    mut event_buffer: ResMut<EventBuffer>,
    time: Res<SimulationTime>,
) {
    let Some(receiver) = receiver else { return };
    let Ok(rx) = receiver.0.lock() else { return };

    while let Ok(action) = rx.try_recv() {
        // Correlation-ID fuer diesen Vorgang (gruppiert Action + Folge-Events)
        let correlation_id = uuid::Uuid::new_v4().to_string();
        let mut bio_action: Option<&str> = None;

        // Agent im ECS finden
        let mut found = false;
        for (identity, mut position, mut work_ctx, mut bio) in &mut query {
            if identity.agent_id != action.agent_id {
                continue;
            }
            found = true;

            match action.action_type {
                ActionType::Move => {
                    if let Some(target_room) = &action.target_room {
                        position.in_transit = true;
                        position.transit_target = Some(format!("ROOM-{}", target_room.0));
                        // Default Transit-Dauer: 3000ms (wird spaeter aus rooms.toml berechnet)
                        position.transit_remaining_ms = 3000;
                        position.transit_correlation_id = Some(correlation_id.clone());
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
                            }
                            "eat_meal" => {
                                sentinel_bio::eat_meal(&mut bio);
                                bio_action = Some("eat_meal");
                            }
                            "use_bathroom" => {
                                sentinel_bio::use_bathroom(&mut bio);
                                bio_action = Some("use_bathroom");
                            }
                            _ => {
                                work_ctx.current_task = Some(content.clone());
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
            target_room: action.target_room.map(|r| format!("ROOM-{}", r.0)),
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
        }
    }
}

/// 2. Aktualisiert biologische Zustaende via sentinel-bio Differenzialgleichungen
pub fn bio_system(
    mut query: Query<(&mut BioState, &Personality, &WorkContext)>,
    time: Res<SimulationTime>,
) {
    for (mut bio, personality, work) in &mut query {
        sentinel_bio::update_bio_state(
            &mut bio,
            personality,
            work,
            time.delta_seconds,
            time.sim_hour,
        );
    }
}

/// 3. Berechnet Raum-Physik (Akustik, Temperatur, CO2)
///
/// Zaehlt Agenten pro Raum und berechnet physikalische Parameter.
/// Ergebnisse werden aktuell nicht persistiert - perception_system liest
/// direkt aus den Queries. Raum-State-Resource kommt in Phase 3.
pub fn physics_system(query: Query<(&Position, Option<&WorkContext>)>) {
    // Agenten pro Raum zaehlen und Meeting-Status ermitteln
    let mut room_agents: std::collections::HashMap<&str, (usize, bool)> =
        std::collections::HashMap::new();
    for (pos, work) in &query {
        if !pos.in_transit {
            let entry = room_agents
                .entry(pos.room_id.as_str())
                .or_insert((0, false));
            entry.0 += 1;
            if let Some(w) = work {
                if w.in_meeting {
                    entry.1 = true;
                }
            }
        }
    }

    // Physik pro Raum berechnen (Ergebnisse via tracing geloggt)
    for (room_id, (agent_count, has_meeting)) in &room_agents {
        let noise_db =
            sentinel_physics::calculate_noise_level(*agent_count, *has_meeting, false, &[]);
        let temperature = sentinel_physics::calculate_temperature(
            21.0,
            *agent_count,
            false,
            15.0, // Default-Aussentemperatur
            0.3,  // Mittlere Sonnenexposition
        );
        let _co2 = sentinel_physics::calculate_co2(400.0, *agent_count, 0.5, 1.0);
        let _noise_text = sentinel_physics::noise_to_text(noise_db);
        let _temp = temperature; // Wird in perception_system genutzt via Room-State (Phase 3)
        let _ = room_id; // Compiler-Hint: Room-ID wird in Phase 3 fuer Room-State genutzt
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

/// 5. Generiert Zufallsereignisse (Poisson-verteilt)
///
/// Nutzt Tick-basierte Pseudo-Zufallszahlen. Erzeugt DomainEvents
/// fuer getriggerte Chaos-Events.
pub fn chaos_system(time: Res<SimulationTime>, mut event_buffer: ResMut<EventBuffer>) {
    // Pseudo-RNG basierend auf Tick (einfacher xorshift-Hash)
    let tick = time.tick.0;
    let pseudo_rng = |seed: u64| -> f32 {
        let mut x = seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
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

    for (i, (physics_type, common_type, description)) in event_types.iter().enumerate() {
        let freq = sentinel_physics::chaos_frequency_per_hour(*physics_type);
        let rng = pseudo_rng(tick.wrapping_mul(31).wrapping_add(i as u64));
        let triggered = sentinel_physics::should_trigger_chaos(freq, time.delta_seconds, rng);

        if triggered {
            let payload = DomainEventPayload::ChaosTriggered {
                event_type: *common_type,
                target_room: None,
                description: description.to_string(),
            };
            let event = DomainEvent::new(
                payload.event_type_str(),
                "building",
                &payload.to_json(),
                &uuid::Uuid::new_v4().to_string(),
                time.tick.0,
            );
            event_buffer.events.push(event);
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
        "toilette-eg" => "auf der Toilette (EG)".to_string(),
        "toilette-og" => "auf der Toilette (OG)".to_string(),
        "treppenhaus" => "im Treppenhaus".to_string(),
        other => format!("im Raum '{}'", other),
    }
}

/// 9. Sendet Wahrnehmung via Channel an externen Zenoh-Publisher.
///
/// Serialisiert PerceptionState + Position + EventQueue zu Perception-Message
/// und sendet sie ueber den PerceptionSender Channel.
/// impulse_text wird aus der priorisierten EventQueue generiert.
pub fn output_system(
    sender: Option<Res<PerceptionSender>>,
    query: Query<(&AgentIdentity, &PerceptionState, &EventQueue)>,
    time: Res<SimulationTime>,
) {
    let Some(sender) = sender else { return };

    for (identity, perception, queue) in &query {
        let impulse_text = super::decision::format_impulse_from_queue(queue);
        let msg = Perception {
            agent_id: identity.agent_id,
            circadian_text: format!("{:.0}:00 Uhr", time.sim_hour),
            body_text: perception.body_text.clone(),
            environment_text: perception.environment_text.clone(),
            acoustic_text: String::new(),
            presence_text: perception.social_text.clone(),
            impulse_text,
            timestamp: Timestamp(time.tick.0),
            tick: time.tick,
        };
        // try_send: Non-blocking. Bei vollem Channel wird die Perception gedroppt
        // (naechster Tick liefert frische Daten).
        let _ = sender.0.try_send(msg);
    }
}

/// 9. Persistiert Zustand: Events nach Limbo, Snapshots nach redb (BATCHED).
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
) {
    telemetry.ticks_observed = telemetry.ticks_observed.saturating_add(1);

    // 1. Events aus Buffer nach Limbo schreiben (mit Outbox)
    if let Some(es) = &event_store {
        for event in event_buffer.events.drain(..) {
            let topic = event_topic(&event);
            if let Err(err) = es.0.append_with_outbox(&event, &topic) {
                warn!(event_id = %event.event_id, "persist_system: failed to write event: {err}");
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
