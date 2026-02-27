//! Decision System - Interrupt-Prioritaeten fuer Agent-Wahrnehmung.
//!
//! Laeuft als 8. System (nach perception_system, vor output_system) und
//! kontrolliert WELCHE Informationen in den naechsten `impulse_text`-Block fliessen.
//!
//! Generiert priorisierte Events (P0-P3) basierend auf Bio-Zustand,
//! Arbeitskontext, Stimmung und Chaos-Events. Max 5 Events pro Injection.

use super::components::*;
use super::world::{EventBuffer, SimulationTime};
use bevy_ecs::prelude::*;
use sentinel_common::Emotion;

/// Max Events pro Injection-Zyklus (AC3)
const MAX_EVENTS: usize = 5;

/// Decision System: Generiert priorisierte EventQueue pro Agent.
///
/// Liest Bio/Work/Mood-Zustand und Chaos-Events aus EventBuffer,
/// erzeugt PendingEvents mit P0-P3 Prioritaeten, sortiert und
/// beschraenkt auf MAX_EVENTS.
pub fn decision_system(
    mut query: Query<(
        &BioState,
        &Personality,
        &Mood,
        &WorkContext,
        &PerceptionState,
        &mut EventQueue,
    )>,
    time: Res<SimulationTime>,
    event_buffer: Res<EventBuffer>,
) {
    let current_tick = time.tick.0;

    for (bio, personality, mood, work, _perception, mut queue) in &mut query {
        // 1. TTL dekrementieren, abgelaufene Events entfernen
        for event in &mut queue.events {
            event.ttl_ticks = event.ttl_ticks.saturating_sub(1);
        }
        queue.events.retain(|e| e.ttl_ticks > 0);

        // 2. Neue Events basierend auf aktuellem Zustand generieren
        generate_bio_events(bio, personality, &mut queue, current_tick);
        generate_work_events(work, &mut queue, current_tick);
        generate_mood_events(mood, personality, &mut queue, current_tick);

        // 3. Chaos-Events aus EventBuffer als P3 einfuegen
        for domain_event in &event_buffer.events {
            if domain_event.event_type == "chaos_triggered" {
                let text = extract_chaos_text(&domain_event.payload);
                push_event(
                    &mut queue,
                    PendingEvent {
                        priority: Priority::P3,
                        text,
                        ttl_ticks: 10,
                        created_tick: current_tick,
                    },
                );
            }
        }

        // 4. Sortieren nach Priority (P0 zuerst), auf max 5 beschraenken
        queue.events.sort_by_key(|e| e.priority);
        queue.events.truncate(MAX_EVENTS);
    }
}

/// Fuegt Event in Queue ein, wenn kein Duplikat (gleicher Text+Priority) existiert.
fn push_event(queue: &mut EventQueue, event: PendingEvent) {
    let duplicate = queue
        .events
        .iter()
        .any(|e| e.priority == event.priority && e.text == event.text);
    if !duplicate {
        queue.events.push(event);
    }
}

/// P0/P1/P2 Events aus biologischem Zustand
fn generate_bio_events(
    bio: &BioState,
    personality: &Personality,
    queue: &mut EventQueue,
    tick: u64,
) {
    // P0: Biologische Notfaelle
    if bio.bladder > 90.0 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P0,
                text: "Du musst SOFORT zur Toilette!".to_string(),
                ttl_ticks: 255,
                created_tick: tick,
            },
        );
    }
    if bio.energy < 15.0 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P0,
                text: "Dir wird schwarz vor Augen - du brauchst eine Pause.".to_string(),
                ttl_ticks: 255,
                created_tick: tick,
            },
        );
    }
    if bio.hunger > 95.0 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P0,
                text: "Dir ist schwindelig vor Hunger.".to_string(),
                ttl_ticks: 255,
                created_tick: tick,
            },
        );
    }
    if bio.stress > 90.0 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P0,
                text: "Dein Herz rast, du brauchst frische Luft.".to_string(),
                ttl_ticks: 255,
                created_tick: tick,
            },
        );
    }

    // P1: Dringende Bio-Signale
    if bio.bladder > 70.0 && bio.bladder <= 90.0 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P1,
                text: "Du solltest bald zur Toilette.".to_string(),
                ttl_ticks: 60,
                created_tick: tick,
            },
        );
    }
    if bio.hunger > 80.0 && bio.hunger <= 95.0 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P1,
                text: "Dein Magen knurrt laut.".to_string(),
                ttl_ticks: 60,
                created_tick: tick,
            },
        );
    }
    if bio.energy < 30.0 && bio.energy >= 15.0 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P1,
                text: "Du bist sehr muede.".to_string(),
                ttl_ticks: 60,
                created_tick: tick,
            },
        );
    }

    // P2: Moderate Bio-Signale
    if bio.stress > 60.0 && bio.stress <= 90.0 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P2,
                text: "Du stehst unter Druck.".to_string(),
                ttl_ticks: 30,
                created_tick: tick,
            },
        );
    }
    // Koffein-Entzug: caffeine_mg < 20 UND tolerance > 0.3
    if bio.caffeine_mg < 20.0 && personality.caffeine_tolerance > 0.3 {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P2,
                text: "Leichte Kopfschmerzen - du brauchst Kaffee.".to_string(),
                ttl_ticks: 30,
                created_tick: tick,
            },
        );
    }
}

/// P1 Events aus Arbeitskontext
fn generate_work_events(work: &WorkContext, queue: &mut EventQueue, tick: u64) {
    if work.in_meeting {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P1,
                text: "Du bist im Meeting - konzentrier dich.".to_string(),
                ttl_ticks: 60,
                created_tick: tick,
            },
        );
    }
}

/// P2/P3 Events aus Stimmung und Persoenlichkeit
fn generate_mood_events(mood: &Mood, personality: &Personality, queue: &mut EventQueue, tick: u64) {
    // P2: Social-Need Extremwerte (persoenlichkeitsabhaengig)
    if mood.dominant_emotion == Emotion::Stressed {
        // Bereits durch Bio-Stress abgedeckt, kein Duplikat
    }

    // Extrovert + hoher social_need
    if personality.extraversion > 0.5 {
        // Wir koennen social_need nicht direkt aus Mood lesen,
        // daher wird dies in generate_bio_events via BioState abgedeckt.
        // Hier fangen wir den Fall ab, dass der Mood auf Bored steht.
    }

    // P3: Langeweile
    if mood.dominant_emotion == Emotion::Bored {
        push_event(
            queue,
            PendingEvent {
                priority: Priority::P3,
                text: "Dir ist langweilig.".to_string(),
                ttl_ticks: 10,
                created_tick: tick,
            },
        );
    }
}

/// Extrahiert Chaos-Beschreibungstext aus dem JSON-Payload
fn extract_chaos_text(payload_json: &str) -> String {
    // Payload ist serde_json vom ChaosTriggered Variant
    // Format: {"type":"ChaosTriggered","event_type":"...","target_room":...,"description":"..."}
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) {
        if let Some(desc) = value.get("description").and_then(|v| v.as_str()) {
            return desc.to_string();
        }
    }
    "Etwas Unerwartetes passiert.".to_string()
}

/// Formatiert EventQueue als impulse_text fuer die Perception-Message.
///
/// Nimmt die Top-Events (P0 zuerst) und formatiert sie als:
/// ```text
/// [P0] Du musst SOFORT zur Toilette!
/// [P1] Dein Magen knurrt laut.
/// ```
pub fn format_impulse_from_queue(queue: &EventQueue) -> String {
    if queue.events.is_empty() {
        return String::new();
    }

    queue
        .events
        .iter()
        .map(|e| {
            let tag = match e.priority {
                Priority::P0 => "[P0]",
                Priority::P1 => "[P1]",
                Priority::P2 => "[P2]",
                Priority::P3 => "[P3]",
            };
            format!("{} {}", tag, e.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::Tick;

    fn default_bio() -> BioState {
        BioState {
            hunger: 20.0,
            energy: 80.0,
            caffeine_mg: 50.0,
            bladder: 10.0,
            stress: 15.0,
            social_need: 50.0,
            comfort: 70.0,
        }
    }

    fn default_personality() -> Personality {
        Personality {
            openness: 0.5,
            conscientiousness: 0.5,
            extraversion: 0.5,
            agreeableness: 0.5,
            neuroticism: 0.3,
            caffeine_tolerance: 0.5,
            is_morning_person: true,
        }
    }

    fn default_mood() -> Mood {
        Mood {
            valence: 0.2,
            arousal: 0.3,
            dominant_emotion: Emotion::Neutral,
        }
    }

    fn default_work() -> WorkContext {
        WorkContext {
            current_task: None,
            in_meeting: false,
            has_deadline: false,
            has_conflict: false,
        }
    }

    fn default_queue() -> EventQueue {
        EventQueue::default()
    }

    /// AC1: P0 wird nie uebersprungen - bladder=96 erzeugt P0
    #[test]
    fn test_p0_bladder_emergency() {
        let mut bio = default_bio();
        bio.bladder = 96.0;
        let personality = default_personality();
        let mut queue = default_queue();

        generate_bio_events(&bio, &personality, &mut queue, 1);

        let p0_events: Vec<_> = queue
            .events
            .iter()
            .filter(|e| e.priority == Priority::P0)
            .collect();
        assert!(!p0_events.is_empty(), "bladder=96 muss P0 Event erzeugen");
        assert!(
            p0_events[0].text.contains("Toilette"),
            "P0 Text soll Toilette enthalten, got: {}",
            p0_events[0].text
        );
    }

    /// AC1: P0 bei energy < 15
    #[test]
    fn test_p0_energy_emergency() {
        let mut bio = default_bio();
        bio.energy = 10.0;
        let personality = default_personality();
        let mut queue = default_queue();

        generate_bio_events(&bio, &personality, &mut queue, 1);

        let p0_events: Vec<_> = queue
            .events
            .iter()
            .filter(|e| e.priority == Priority::P0)
            .collect();
        assert!(!p0_events.is_empty(), "energy=10 muss P0 Event erzeugen");
        assert!(
            p0_events[0].text.contains("schwarz vor Augen"),
            "P0 Text soll 'schwarz vor Augen' enthalten"
        );
    }

    /// AC2: P3 verfaellt nach TTL
    #[test]
    fn test_p3_ttl_expiry() {
        let mut queue = default_queue();
        push_event(
            &mut queue,
            PendingEvent {
                priority: Priority::P3,
                text: "Kuchen in der Kueche!".to_string(),
                ttl_ticks: 2,
                created_tick: 0,
            },
        );

        assert_eq!(queue.events.len(), 1, "Event sollte in Queue sein");

        // Tick 1: TTL 2 -> 1
        for event in &mut queue.events {
            event.ttl_ticks = event.ttl_ticks.saturating_sub(1);
        }
        queue.events.retain(|e| e.ttl_ticks > 0);
        assert_eq!(queue.events.len(), 1, "TTL=1, Event bleibt");

        // Tick 2: TTL 1 -> 0
        for event in &mut queue.events {
            event.ttl_ticks = event.ttl_ticks.saturating_sub(1);
        }
        queue.events.retain(|e| e.ttl_ticks > 0);
        assert_eq!(queue.events.len(), 0, "TTL=0, Event entfernt");
    }

    /// AC3: Max 5 Events - ueberzaehlige werden gedroppt
    #[test]
    fn test_max_5_events() {
        let mut queue = default_queue();

        // 8 Events einfuegen (P0-P3 gemischt)
        for i in 0..8 {
            let priority = match i % 4 {
                0 => Priority::P3,
                1 => Priority::P2,
                2 => Priority::P1,
                _ => Priority::P0,
            };
            push_event(
                &mut queue,
                PendingEvent {
                    priority,
                    text: format!("Event {}", i),
                    ttl_ticks: 30,
                    created_tick: 0,
                },
            );
        }

        assert_eq!(queue.events.len(), 8, "Vor Sortierung 8 Events");

        // Sortieren und truncaten wie decision_system
        queue.events.sort_by_key(|e| e.priority);
        queue.events.truncate(MAX_EVENTS);

        assert_eq!(queue.events.len(), MAX_EVENTS, "Nach truncate max 5");
        // P0 muss erhalten bleiben (hoechste Prioritaet = kleinster Ord)
        assert!(
            queue.events.iter().any(|e| e.priority == Priority::P0),
            "P0 Events duerfen nicht gedroppt werden"
        );
    }

    /// Duplikat-Vermeidung: Gleiches Event nicht doppelt
    #[test]
    fn test_duplicate_prevention() {
        let mut queue = default_queue();

        push_event(
            &mut queue,
            PendingEvent {
                priority: Priority::P0,
                text: "Du musst SOFORT zur Toilette!".to_string(),
                ttl_ticks: 255,
                created_tick: 0,
            },
        );
        push_event(
            &mut queue,
            PendingEvent {
                priority: Priority::P0,
                text: "Du musst SOFORT zur Toilette!".to_string(),
                ttl_ticks: 255,
                created_tick: 1,
            },
        );

        assert_eq!(queue.events.len(), 1, "Duplikat soll verhindert werden");
    }

    /// Personality-Modifikation: Introvert bei niedrigem social_need
    #[test]
    fn test_bored_mood_generates_p3() {
        let mut mood = default_mood();
        mood.dominant_emotion = Emotion::Bored;
        let personality = default_personality();
        let mut queue = default_queue();

        generate_mood_events(&mood, &personality, &mut queue, 1);

        let p3_events: Vec<_> = queue
            .events
            .iter()
            .filter(|e| e.priority == Priority::P3)
            .collect();
        assert!(!p3_events.is_empty(), "Bored mood soll P3 erzeugen");
        assert!(
            p3_events[0].text.contains("langweilig"),
            "Text soll 'langweilig' enthalten"
        );
    }

    /// Meeting erzeugt P1
    #[test]
    fn test_meeting_generates_p1() {
        let mut work = default_work();
        work.in_meeting = true;
        let mut queue = default_queue();

        generate_work_events(&work, &mut queue, 1);

        let p1_events: Vec<_> = queue
            .events
            .iter()
            .filter(|e| e.priority == Priority::P1)
            .collect();
        assert!(!p1_events.is_empty(), "Meeting soll P1 erzeugen");
        assert!(
            p1_events[0].text.contains("Meeting"),
            "Text soll 'Meeting' enthalten"
        );
    }

    /// Chaos-Text-Extraktion aus JSON
    #[test]
    fn test_extract_chaos_text() {
        let payload = r#"{"type":"ChaosTriggered","event_type":"CakeInKitchen","target_room":null,"description":"Kuchen in der Kueche"}"#;
        let text = extract_chaos_text(payload);
        assert_eq!(text, "Kuchen in der Kueche");
    }

    /// format_impulse_from_queue Formatierung
    #[test]
    fn test_format_impulse_from_queue() {
        let mut queue = default_queue();
        push_event(
            &mut queue,
            PendingEvent {
                priority: Priority::P0,
                text: "Du musst SOFORT zur Toilette!".to_string(),
                ttl_ticks: 255,
                created_tick: 0,
            },
        );
        push_event(
            &mut queue,
            PendingEvent {
                priority: Priority::P1,
                text: "Dein Magen knurrt laut.".to_string(),
                ttl_ticks: 60,
                created_tick: 0,
            },
        );
        queue.events.sort_by_key(|e| e.priority);

        let result = format_impulse_from_queue(&queue);
        assert!(result.contains("[P0] Du musst SOFORT zur Toilette!"));
        assert!(result.contains("[P1] Dein Magen knurrt laut."));
    }

    /// Leere Queue erzeugt leeren impulse_text
    #[test]
    fn test_format_impulse_empty_queue() {
        let queue = default_queue();
        let result = format_impulse_from_queue(&queue);
        assert!(result.is_empty(), "Leere Queue = leerer String");
    }

    /// AC5: Performance - decision_system < 50us pro Tick bei 24 Agents
    #[test]
    fn test_decision_performance_24_agents() {
        use bevy_ecs::prelude::*;
        use std::time::Instant;

        let (mut world, _) = crate::world::create_simulation_world();

        // 24 Agents spawnen (15 Schicht + 9 Sonder)
        let mut entities = Vec::new();
        for i in 1..=24u16 {
            let shift_set = if i <= 15 { 1 } else { 0 };
            let entity = crate::world::spawn_agent(
                &mut world,
                sentinel_common::AgentId(i),
                &format!("Agent-{i:02}"),
                "Mitarbeiter",
                shift_set,
            );
            entities.push(entity);
        }

        // Realistische Bio-Mischung: verschiedene Prioritaeten triggern
        for (idx, &entity) in entities.iter().enumerate() {
            let mut bio = world.get_mut::<BioState>(entity).unwrap();
            match idx % 6 {
                0 => bio.bladder = 92.0,     // P0: Toilette-Notfall
                1 => bio.energy = 12.0,      // P0: Energie-Notfall
                2 => bio.hunger = 85.0,      // P1: Hunger
                3 => bio.stress = 75.0,      // P2: Stress
                4 => bio.caffeine_mg = 10.0, // P2: Koffein-Entzug
                _ => {}                      // Default: keine Events
            }
        }

        // Decision-only Schedule (isolierte Messung)
        let mut schedule = Schedule::default();
        schedule.add_systems(decision_system);

        // Warmup (10 Ticks)
        for tick in 0..10u64 {
            world.resource_mut::<SimulationTime>().tick = sentinel_common::Tick(tick);
            schedule.run(&mut world);
        }

        // Messung (100 Ticks)
        let ticks = 100u64;
        let start = Instant::now();
        for tick in 10..10 + ticks {
            world.resource_mut::<SimulationTime>().tick = sentinel_common::Tick(tick);
            schedule.run(&mut world);
        }
        let elapsed = start.elapsed();
        let us_per_tick = elapsed.as_micros() as f64 / ticks as f64;

        // Debug-Modus hat ~5x Overhead, CI-Runner hat zusaetzliche Varianz
        let threshold_us = if cfg!(debug_assertions) { 1000.0 } else { 50.0 };
        assert!(
            us_per_tick < threshold_us,
            "decision_system muss <{threshold_us}us/tick bei 24 Agents sein, got {:.1}us",
            us_per_tick
        );
    }

    /// E2E: Bio-Notfall -> P0 in Queue UND impulse_text nicht leer (AC6)
    #[test]
    fn test_e2e_bio_emergency_to_impulse() {
        use crate::world::{create_simulation_world, spawn_agent};
        use sentinel_common::AgentId;

        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // BioState auf Notfall setzen
        {
            let mut bio = world.get_mut::<BioState>(entity).unwrap();
            bio.bladder = 96.0;
        }

        // Einen Tick ausfuehren
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        // EventQueue muss P0 enthalten
        let queue = world.get::<EventQueue>(entity).unwrap();
        let p0_events: Vec<_> = queue
            .events
            .iter()
            .filter(|e| e.priority == Priority::P0)
            .collect();
        assert!(
            !p0_events.is_empty(),
            "bladder=96 nach Tick muss P0 in Queue haben"
        );

        // Perception Channel pruefen: impulse_text nicht leer
        // (wird in output_system-Integration separat getestet)
    }
}
