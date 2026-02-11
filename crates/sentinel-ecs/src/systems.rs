//! ECS Systems fuer Agent-Simulation.
//!
//! Definiert 9 Systems in strikter Ausfuehrungsreihenfolge:
//! 1. input_system - Empfaengt Agent-Aktionen (via Zenoh) [Phase 3]
//! 2. bio_system - Aktualisiert biologische Zustaende (sentinel-bio)
//! 3. physics_system - Berechnet Raum-Physik (sentinel-physics)
//! 4. transit_system - Verarbeitet Raumwechsel (sentinel-physics)
//! 5. chaos_system - Generiert Zufallsereignisse (sentinel-physics)
//! 6. mood_system - Berechnet Stimmung aus Bio+Kontext
//! 7. perception_system - Generiert Wahrnehmungstext fuer LLM-Prompt
//! 8. output_system - Sendet Wahrnehmung via Zenoh [Phase 3]
//! 9. persist_system - Persistiert Zustand [Phase 3]

use super::components::*;
use super::world::SimulationTime;
use bevy_ecs::prelude::*;
use sentinel_common::Emotion;

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
    Output,
    Persist,
}

/// 1. Empfaengt Agent-Aktionen (via Zenoh) - Phase 3 Integration
pub fn input_system() {
    // Wird in Phase 3 (LLM Bridge) mit Zenoh verbunden:
    // - Subscribed auf sentinel/actions/{agent_id}
    // - Deserialisiert AgentAction
    // - Aktualisiert WorkContext, Position (Move-Requests)
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
pub fn transit_system(mut query: Query<&mut Position>, time: Res<SimulationTime>) {
    let delta_ms = (time.delta_seconds * 1000.0) as u32;
    for mut pos in &mut query {
        if pos.in_transit {
            pos.transit_remaining_ms = pos.transit_remaining_ms.saturating_sub(delta_ms);
            if pos.transit_remaining_ms == 0 {
                // Transit abgeschlossen: Agent kommt im Zielraum an
                if let Some(target) = pos.transit_target.take() {
                    pos.room_id = target;
                }
                pos.in_transit = false;
            }
        }
    }
}

/// 5. Generiert Zufallsereignisse (Poisson-verteilt)
///
/// Nutzt Tick-basierte Pseudo-Zufallszahlen. Echte RNG-Resource
/// wird in Phase 3 injiziert.
pub fn chaos_system(time: Res<SimulationTime>) {
    // Pseudo-RNG basierend auf Tick (einfacher xorshift-Hash)
    let tick = time.tick.0;
    let pseudo_rng = |seed: u64| -> f32 {
        let mut x = seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % 10000) as f32 / 10000.0
    };

    // Alle Chaos-Event-Typen pruefen
    let event_types = [
        sentinel_physics::ChaosEventType::PhoneRing,
        sentinel_physics::ChaosEventType::PrinterBroken,
        sentinel_physics::ChaosEventType::PackageDelivery,
        sentinel_physics::ChaosEventType::SBahnDelay,
        sentinel_physics::ChaosEventType::FireAlarmDrill,
        sentinel_physics::ChaosEventType::CakeInKitchen,
        sentinel_physics::ChaosEventType::AirConBroken,
        sentinel_physics::ChaosEventType::InternetOutage,
    ];

    for (i, event_type) in event_types.iter().enumerate() {
        let freq = sentinel_physics::chaos_frequency_per_hour(*event_type);
        let rng = pseudo_rng(tick.wrapping_mul(31).wrapping_add(i as u64));
        let _triggered = sentinel_physics::should_trigger_chaos(freq, time.delta_seconds, rng);
        // Phase 3: Getriggerte Events in Event-Queue schreiben
        // und per Zenoh an betroffene Raeume/Agenten dispatchen
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

/// 8. Sendet Wahrnehmung via Zenoh an LLM - Phase 3 Integration
pub fn output_system(_query: Query<(&AgentIdentity, &PerceptionState)>) {
    // Wird in Phase 3 (LLM Bridge) implementiert:
    // - Serialisiert PerceptionState zu Perception-Message
    // - Publiziert auf sentinel/perception/{agent_id}
    // - LLM-Bridge subscribed und generiert Agent-Aktionen
}

/// 9. Persistiert Zustand in redb/Limbo (BATCHED) - Phase 3 Integration
pub fn persist_system() {
    // Wird in Phase 3 implementiert:
    // - Batched Write via sentinel-redb (Agent-State Snapshots)
    // - Event-Log via sentinel-limbo (alle Ticks)
}
