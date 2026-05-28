//! Perception Generation und Format-Injection fuer LLM-Prompts.
//!
//! Wandelt numerische ECS-Zustaende in natuerlichsprachliche deutsche
//! Wahrnehmungstexte um, die als System-Injection in Agent-LLM-Prompts
//! eingefuegt werden.

use sentinel_common::components::{BioState, Personality, Position};
use std::fmt::Write as _;

/// Geruchsereignis aus der Physics-Engine
#[derive(Debug, Clone)]
pub struct SmellEvent {
    pub source_room: String,
    pub smell_type: String,
    pub intensity: f32,
    pub radius_rooms: u32,
    pub decay_per_room: f32,
    pub created_tick: u64,
    pub duration_ticks: u64,
}

/// Generierte Wahrnehmungstexte (ohne Metadaten wie agent_id/timestamp)
#[derive(Debug, Clone, Default)]
pub struct PerceptionTexts {
    pub circadian_text: String,
    pub body_text: String,
    pub environment_text: String,
    pub acoustic_text: String,
    pub presence_text: String,
    pub impulse_text: String,
}

impl PerceptionTexts {
    pub fn clear(&mut self) {
        self.circadian_text.clear();
        self.body_text.clear();
        self.environment_text.clear();
        self.acoustic_text.clear();
        self.presence_text.clear();
        self.impulse_text.clear();
    }
}

/// Generiert Wahrnehmungstexte aus ECS-Zustandsdaten.
///
/// Berechnet Koerperwahrnehmung, Umgebung, Akustik, Anwesende und
/// Handlungsimpulse basierend auf Bio-Zustand, Persoenlichkeit und
/// Raum-Physik-Daten.
#[allow(clippy::too_many_arguments)]
pub fn generate_perception(
    bio: &BioState,
    _position: &Position,
    personality: &Personality,
    room_noise_db: f32,
    room_temp_c: f32,
    room_co2_ppm: f32,
    room_smells: &[SmellEvent],
    present_agents: &[(String, String)],
    sim_time: &str,
    focus_hours: f32,
) -> PerceptionTexts {
    let mut perception = PerceptionTexts::default();
    generate_perception_into(
        &mut perception,
        bio,
        _position,
        personality,
        room_noise_db,
        room_temp_c,
        room_co2_ppm,
        room_smells,
        present_agents,
        sim_time,
        focus_hours,
    );
    perception
}

/// Generiert Wahrnehmungstexte in wiederverwendete String-Buffer.
#[allow(clippy::too_many_arguments)]
pub fn generate_perception_into(
    perception: &mut PerceptionTexts,
    bio: &BioState,
    _position: &Position,
    personality: &Personality,
    room_noise_db: f32,
    room_temp_c: f32,
    room_co2_ppm: f32,
    room_smells: &[SmellEvent],
    present_agents: &[(String, String)],
    sim_time: &str,
    focus_hours: f32,
) {
    perception.clear();

    // Circadian
    write!(
        perception.circadian_text,
        "{} (Du arbeitest seit {:.0}h konzentriert)",
        sim_time, focus_hours
    )
    .expect("writing to String cannot fail");

    // Koerperwahrnehmung
    generate_body_text_into(&mut perception.body_text, bio, personality);

    // Umgebung (Temperatur, CO2, Gerueche)
    generate_environment_text_into(
        &mut perception.environment_text,
        room_temp_c,
        room_co2_ppm,
        room_smells,
    );

    // Akustik
    generate_acoustic_text_into(&mut perception.acoustic_text, room_noise_db);

    // Anwesende
    generate_presence_text_into(&mut perception.presence_text, present_agents);

    // Impuls
    generate_impulse_text_into(&mut perception.impulse_text, bio, personality);
}

/// Formatiert Wahrnehmungstexte als `[SYSTEM_INJECTION]` Block fuer LLM-Prompts.
///
/// Nur nicht-leere Felder werden aufgenommen (CIRCADIAN ist immer dabei).
pub fn format_injection(perception: &PerceptionTexts) -> String {
    let mut injection = String::with_capacity(
        perception.circadian_text.len()
            + perception.body_text.len()
            + perception.environment_text.len()
            + perception.acoustic_text.len()
            + perception.presence_text.len()
            + perception.impulse_text.len()
            + 96,
    );
    injection.push_str("[SYSTEM_INJECTION]\nCIRCADIAN: ");
    injection.push_str(&perception.circadian_text);

    if !perception.body_text.is_empty() {
        append_injection_field(&mut injection, "KOERPER", &perception.body_text);
    }
    if !perception.environment_text.is_empty() {
        append_injection_field(&mut injection, "ENVIRONMENT", &perception.environment_text);
    }
    if !perception.acoustic_text.is_empty() {
        append_injection_field(&mut injection, "AKUSTIK", &perception.acoustic_text);
    }
    if !perception.presence_text.is_empty() {
        append_injection_field(&mut injection, "ANWESEND", &perception.presence_text);
    }
    if !perception.impulse_text.is_empty() {
        append_injection_field(&mut injection, "IMPULS", &perception.impulse_text);
    }

    injection.push_str("\n[/SYSTEM_INJECTION]");
    injection
}

fn generate_body_text_into(output: &mut String, bio: &BioState, personality: &Personality) {
    output.clear();

    // Hunger-Schwellenwerte
    if bio.hunger > 90.0 {
        append_part(output, "Dir ist schwindelig vor Hunger.");
    } else if bio.hunger > 80.0 {
        append_part(output, "Dein Magen krampft.");
    } else if bio.hunger > 70.0 {
        append_part(output, "Du koenntest etwas essen.");
    }

    // Blasendrang
    if bio.bladder > 90.0 {
        append_part(output, "Du musst JETZT zur Toilette.");
    } else if bio.bladder > 80.0 {
        append_part(output, "Dringend: Toilette.");
    } else if bio.bladder > 60.0 {
        append_part(output, "Du solltest bald eine Pause einlegen.");
    }

    // Energie
    if bio.energy < 20.0 {
        append_part(output, "Du kannst kaum die Augen offen halten.");
    } else if bio.energy < 40.0 {
        append_part(output, "Du bist muede.");
    }

    // Stress
    if bio.stress > 80.0 {
        append_part(output, "Dein Herz rast.");
    } else if bio.stress > 60.0 {
        append_part(output, "Du stehst unter Druck.");
    }

    // Koffein-Entzug: caffeine_mg < 20.0 UND caffeine_tolerance > 0.3
    if bio.caffeine_mg < 20.0 && personality.caffeine_tolerance > 0.3 {
        append_part(output, "Leichte Kopfschmerzen - du brauchst Kaffee.");
    }

    // Gesund (keine Schwellenwerte ueberschritten)
    if output.is_empty() {
        output.push_str("Du fuehlst dich gut.");
    }
}

fn generate_environment_text_into(
    output: &mut String,
    temp_c: f32,
    co2_ppm: f32,
    smells: &[SmellEvent],
) {
    output.clear();

    // Temperatur
    begin_part(output);
    if temp_c > 26.0 {
        write!(output, "Es ist deutlich zu warm ({temp_c:.1} °C).")
            .expect("writing to String cannot fail");
    } else if temp_c > 24.0 {
        write!(output, "Es ist warm ({temp_c:.1} °C).").expect("writing to String cannot fail");
    } else if temp_c < 17.0 {
        write!(output, "Es ist unangenehm kuehl ({temp_c:.1} °C).")
            .expect("writing to String cannot fail");
    } else if temp_c < 19.0 {
        write!(output, "Es ist kuehl ({temp_c:.1} °C).").expect("writing to String cannot fail");
    } else {
        write!(output, "Die Temperatur ist angenehm ({temp_c:.1} °C).")
            .expect("writing to String cannot fail");
    }

    // CO2
    if co2_ppm > 1400.0 {
        begin_part(output);
        write!(output, "Die Luft ist sehr stickig ({co2_ppm:.0} ppm CO2).")
            .expect("writing to String cannot fail");
    } else if co2_ppm > 1000.0 {
        begin_part(output);
        write!(output, "Die Luft ist stickig ({co2_ppm:.0} ppm CO2).")
            .expect("writing to String cannot fail");
    }

    // Gerueche (nur intensity > 0.3)
    for smell in smells {
        if smell.intensity > 0.3 {
            match smell.smell_type.as_str() {
                "coffee" => append_part(output, "Kaffeeduft."),
                "food" => append_part(output, "Essensgeruch."),
                "perfume" => append_part(output, "Parfuemduft."),
                "printer_toner" => append_part(output, "Tonergeruch."),
                other => {
                    begin_part(output);
                    write!(output, "Ein Geruch von {}.", other)
                        .expect("writing to String cannot fail");
                }
            }
        }
    }
}

fn generate_acoustic_text_into(output: &mut String, noise_db: f32) {
    output.clear();
    match noise_db as u32 {
        0..=35 => write!(output, "Stille ({noise_db:.0} dB)."),
        36..=50 => write!(output, "Normales Buerogeraeusch ({noise_db:.0} dB)."),
        51..=65 => write!(output, "Lebhafte Unterhaltungen ({noise_db:.0} dB)."),
        _ => write!(
            output,
            "Es ist laut ({noise_db:.0} dB). Konzentration faellt schwer."
        ),
    }
    .expect("writing to String cannot fail");
}

fn generate_presence_text_into(output: &mut String, present_agents: &[(String, String)]) {
    output.clear();
    if present_agents.is_empty() {
        output.push_str("Du bist allein.");
    } else {
        for (index, (name, activity)) in present_agents.iter().enumerate() {
            if index > 0 {
                output.push_str(", ");
            }
            write!(output, "{} ({})", name, activity).expect("writing to String cannot fail");
        }
    }
}

fn generate_impulse_text_into(output: &mut String, bio: &BioState, personality: &Personality) {
    output.clear();
    if bio.bladder > 85.0 {
        output.push_str("Dringendes Beduerfnis, zur Toilette zu gehen.");
    } else if bio.hunger > 85.0 {
        output.push_str("Dringendes Beduerfnis, etwas zu essen.");
    } else if bio.energy < 25.0 {
        output.push_str("Dringendes Beduerfnis, Pause zu machen.");
    } else if bio.social_need > 80.0 && personality.extraversion > 0.5 {
        output.push_str("Du moechtest mit jemandem reden.");
    } else if bio.social_need < 20.0 && personality.extraversion < 0.5 {
        output.push_str("Du geniesst die Ruhe.");
    }
}

fn append_injection_field(output: &mut String, label: &str, text: &str) {
    output.push('\n');
    output.push_str(label);
    output.push_str(": ");
    output.push_str(text);
}

fn append_part(output: &mut String, text: &str) {
    begin_part(output);
    output.push_str(text);
}

fn begin_part(output: &mut String) {
    if !output.is_empty() {
        output.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_bio() -> BioState {
        BioState {
            hunger: 20.0,
            energy: 80.0,
            caffeine_mg: 0.0,
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

    fn default_position() -> Position {
        Position {
            room_id: "buero-dev-1".to_string(),
            in_transit: false,
            transit_target: None,
            transit_remaining_ms: 0,
            transit_correlation_id: None,
            transit_route: Vec::new(),
            transit_total_ms: 0,
            transit_paused: false,
            transit_pause_tick: 0,
            transit_source: None,
        }
    }

    #[test]
    fn test_generate_perception_into_reuses_output_buffers() {
        let mut bio = default_bio();
        bio.hunger = 95.0;
        bio.bladder = 95.0;
        bio.caffeine_mg = 50.0;
        bio.social_need = 90.0;
        let personality = default_personality();
        let pos = default_position();
        let smells = vec![SmellEvent {
            source_room: "kueche".to_string(),
            smell_type: "coffee".to_string(),
            intensity: 0.8,
            radius_rooms: 2,
            decay_per_room: 0.3,
            created_tick: 100,
            duration_ticks: 500,
        }];
        let agents = vec![
            ("Lisa".to_string(), "Telefonat".to_string()),
            ("Andreas".to_string(), "Coding".to_string()),
        ];
        let mut texts = PerceptionTexts {
            circadian_text: String::with_capacity(128),
            body_text: String::with_capacity(256),
            environment_text: String::with_capacity(256),
            acoustic_text: String::with_capacity(128),
            presence_text: String::with_capacity(128),
            impulse_text: String::with_capacity(128),
        };

        let capacities_before = (
            texts.circadian_text.capacity(),
            texts.body_text.capacity(),
            texts.environment_text.capacity(),
            texts.acoustic_text.capacity(),
            texts.presence_text.capacity(),
            texts.impulse_text.capacity(),
        );

        generate_perception_into(
            &mut texts,
            &bio,
            &pos,
            &personality,
            55.0,
            25.5,
            1200.0,
            &smells,
            &agents,
            "12:00",
            2.0,
        );
        assert!(texts.body_text.contains("schwindelig"));

        bio.hunger = 20.0;
        bio.bladder = 10.0;
        generate_perception_into(
            &mut texts,
            &bio,
            &pos,
            &personality,
            30.0,
            21.0,
            600.0,
            &[],
            &[],
            "13:00",
            3.0,
        );

        assert_eq!(
            capacities_before,
            (
                texts.circadian_text.capacity(),
                texts.body_text.capacity(),
                texts.environment_text.capacity(),
                texts.acoustic_text.capacity(),
                texts.presence_text.capacity(),
                texts.impulse_text.capacity(),
            )
        );
        assert_eq!(texts.body_text, "Du fuehlst dich gut.");
        assert_eq!(texts.presence_text, "Du bist allein.");
    }

    #[test]
    fn test_perception_hungry_agent() {
        let mut bio = default_bio();
        bio.hunger = 95.0;
        bio.bladder = 85.0;
        let personality = default_personality();
        let pos = default_position();

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            22.0,
            800.0,
            &[],
            &[],
            "12:00",
            3.0,
        );

        assert!(
            result.body_text.contains("schwindelig"),
            "body_text should mention schwindelig for hunger=90, got: {}",
            result.body_text
        );
    }

    #[test]
    fn test_perception_alone() {
        let bio = default_bio();
        let personality = default_personality();
        let pos = default_position();

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            30.0,
            22.0,
            600.0,
            &[],
            &[],
            "09:00",
            1.0,
        );

        assert_eq!(result.presence_text, "Du bist allein.");
    }

    #[test]
    fn test_perception_noisy_room() {
        let bio = default_bio();
        let personality = default_personality();
        let pos = default_position();
        let agents = vec![
            ("Lisa".to_string(), "Telefonat".to_string()),
            ("Andreas".to_string(), "Coding".to_string()),
            ("Thomas".to_string(), "Meeting".to_string()),
            ("Julia".to_string(), "Design".to_string()),
            ("Martin".to_string(), "Pause".to_string()),
        ];

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            55.0,
            22.0,
            800.0,
            &[],
            &agents,
            "10:00",
            2.0,
        );

        assert!(
            result.acoustic_text.contains("Lebhafte Unterhaltungen"),
            "acoustic_text should mention 'Lebhafte Unterhaltungen' for 55dB, got: {}",
            result.acoustic_text
        );
        assert!(
            result.presence_text.contains("Lisa"),
            "presence_text should list agents, got: {}",
            result.presence_text
        );
        assert_eq!(agents.len(), 5);
    }

    #[test]
    fn test_perception_caffeinated_ok() {
        let mut bio = default_bio();
        bio.caffeine_mg = 50.0;
        let mut personality = default_personality();
        personality.caffeine_tolerance = 0.5;
        let pos = default_position();

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            22.0,
            600.0,
            &[],
            &[],
            "10:00",
            2.0,
        );

        assert!(
            !result.body_text.contains("Kopfschmerzen"),
            "caffeine=50 should NOT cause withdrawal, got: {}",
            result.body_text
        );
    }

    #[test]
    fn test_perception_caffeine_withdrawal() {
        let mut bio = default_bio();
        bio.caffeine_mg = 10.0;
        let mut personality = default_personality();
        personality.caffeine_tolerance = 0.5;
        let pos = default_position();

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            22.0,
            600.0,
            &[],
            &[],
            "10:00",
            2.0,
        );

        assert!(
            result.body_text.contains("Kopfschmerzen"),
            "caffeine=10, tolerance=0.5 should cause withdrawal, got: {}",
            result.body_text
        );
    }

    #[test]
    fn test_perception_introvert_alone() {
        let mut bio = default_bio();
        bio.social_need = 15.0;
        let mut personality = default_personality();
        personality.extraversion = 0.3;
        let pos = default_position();

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            30.0,
            22.0,
            600.0,
            &[],
            &[],
            "10:00",
            2.0,
        );

        assert!(
            result.impulse_text.contains("Ruhe"),
            "introvert with low social_need should enjoy quiet, got: '{}'",
            result.impulse_text
        );
    }

    #[test]
    fn test_perception_circadian_format() {
        let bio = default_bio();
        let personality = default_personality();
        let pos = default_position();

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            22.0,
            600.0,
            &[],
            &[],
            "11:42",
            4.0,
        );

        assert_eq!(
            result.circadian_text,
            "11:42 (Du arbeitest seit 4h konzentriert)"
        );
    }

    #[test]
    fn test_perception_environment_coffee_and_co2() {
        let bio = default_bio();
        let personality = default_personality();
        let pos = default_position();
        let smells = vec![SmellEvent {
            source_room: "kueche".to_string(),
            smell_type: "coffee".to_string(),
            intensity: 0.8,
            radius_rooms: 2,
            decay_per_room: 0.3,
            created_tick: 100,
            duration_ticks: 500,
        }];

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            25.5,
            1200.0,
            &smells,
            &[],
            "14:00",
            6.0,
        );

        assert!(
            result.environment_text.contains("Kaffeeduft"),
            "should smell coffee, got: {}",
            result.environment_text
        );
        assert!(
            result.environment_text.contains("warm"),
            "25.5C should be warm, got: {}",
            result.environment_text
        );
        assert!(
            result.environment_text.to_lowercase().contains("stickig"),
            "1200ppm should be stickig, got: {}",
            result.environment_text
        );
    }

    #[test]
    fn test_perception_healthy_agent() {
        let mut bio = default_bio();
        bio.caffeine_mg = 50.0; // Kein Koffein-Entzug
        let personality = default_personality();
        let pos = default_position();

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            22.0,
            600.0,
            &[],
            &[],
            "10:00",
            2.0,
        );

        assert!(
            result.body_text.contains("Du fuehlst dich gut."),
            "healthy agent should feel good, got: {}",
            result.body_text
        );
    }

    #[test]
    fn test_format_injection() {
        let perception = PerceptionTexts {
            circadian_text: "10:00 (Du arbeitest seit 2h konzentriert)".to_string(),
            body_text: "Du fuehlst dich gut.".to_string(),
            environment_text: "Die Temperatur ist angenehm (22.0 °C).".to_string(),
            acoustic_text: "Normales Buerogeraeusch (40 dB).".to_string(),
            presence_text: "Lisa (Design), Andreas (Coding)".to_string(),
            impulse_text: String::new(),
        };

        let injection = format_injection(&perception);

        assert!(
            injection.starts_with("[SYSTEM_INJECTION]"),
            "should start with [SYSTEM_INJECTION], got: {}",
            injection
        );
        assert!(
            injection.ends_with("[/SYSTEM_INJECTION]"),
            "should end with [/SYSTEM_INJECTION], got: {}",
            injection
        );
        assert!(injection.contains("CIRCADIAN:"));
        assert!(injection.contains("KOERPER:"));
        assert!(injection.contains("ENVIRONMENT:"));
        assert!(injection.contains("AKUSTIK:"));
        assert!(injection.contains("ANWESEND:"));
        // IMPULS sollte NICHT drin sein (leer)
        assert!(
            !injection.contains("IMPULS:"),
            "empty impulse should be omitted, got: {}",
            injection
        );
    }

    #[test]
    fn test_perception_impulse_priority() {
        let personality = default_personality();
        let pos = default_position();

        // Bladder > Hunger > Energy: Bladder gewinnt
        let mut bio = default_bio();
        bio.bladder = 90.0;
        bio.hunger = 90.0;
        bio.energy = 20.0;

        let result = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            22.0,
            600.0,
            &[],
            &[],
            "10:00",
            2.0,
        );

        assert!(
            result.impulse_text.contains("Toilette"),
            "bladder=90 should win over hunger=90, got: '{}'",
            result.impulse_text
        );

        // Hunger > Energy: Hunger gewinnt wenn bladder niedrig
        bio.bladder = 50.0;
        let result2 = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            22.0,
            600.0,
            &[],
            &[],
            "10:00",
            2.0,
        );

        assert!(
            result2.impulse_text.contains("essen"),
            "hunger=90 should win over energy=20 when bladder=50, got: '{}'",
            result2.impulse_text
        );

        // Energy gewinnt wenn Bladder und Hunger niedrig
        bio.hunger = 50.0;
        let result3 = generate_perception(
            &bio,
            &pos,
            &personality,
            40.0,
            22.0,
            600.0,
            &[],
            &[],
            "10:00",
            2.0,
        );

        assert!(
            result3.impulse_text.contains("Pause"),
            "energy=20 should trigger pause impulse when bladder=50 and hunger=50, got: '{}'",
            result3.impulse_text
        );
    }
}
