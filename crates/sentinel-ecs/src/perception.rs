//! Perception Generation und Format-Injection fuer LLM-Prompts.
//!
//! Wandelt numerische ECS-Zustaende in natuerlichsprachliche deutsche
//! Wahrnehmungstexte um, die als System-Injection in Agent-LLM-Prompts
//! eingefuegt werden.

use sentinel_common::components::{BioState, Personality, Position};

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
#[derive(Debug, Clone)]
pub struct PerceptionTexts {
    pub circadian_text: String,
    pub body_text: String,
    pub environment_text: String,
    pub acoustic_text: String,
    pub presence_text: String,
    pub impulse_text: String,
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
    // Circadian
    let circadian_text = format!(
        "{} (Du arbeitest seit {:.0}h konzentriert)",
        sim_time, focus_hours
    );

    // Koerperwahrnehmung
    let body_text = generate_body_text(bio, personality);

    // Umgebung (Temperatur, CO2, Gerueche)
    let environment_text = generate_environment_text(room_temp_c, room_co2_ppm, room_smells);

    // Akustik
    let acoustic_text = generate_acoustic_text(room_noise_db);

    // Anwesende
    let presence_text = generate_presence_text(present_agents);

    // Impuls
    let impulse_text = generate_impulse_text(bio, personality);

    PerceptionTexts {
        circadian_text,
        body_text,
        environment_text,
        acoustic_text,
        presence_text,
        impulse_text,
    }
}

/// Formatiert Wahrnehmungstexte als `[SYSTEM_INJECTION]` Block fuer LLM-Prompts.
///
/// Nur nicht-leere Felder werden aufgenommen (CIRCADIAN ist immer dabei).
pub fn format_injection(perception: &PerceptionTexts) -> String {
    let mut lines = Vec::new();
    lines.push("[SYSTEM_INJECTION]".to_string());

    // CIRCADIAN ist immer dabei
    lines.push(format!("CIRCADIAN: {}", perception.circadian_text));

    if !perception.body_text.is_empty() {
        lines.push(format!("KOERPER: {}", perception.body_text));
    }
    if !perception.environment_text.is_empty() {
        lines.push(format!("ENVIRONMENT: {}", perception.environment_text));
    }
    if !perception.acoustic_text.is_empty() {
        lines.push(format!("AKUSTIK: {}", perception.acoustic_text));
    }
    if !perception.presence_text.is_empty() {
        lines.push(format!("ANWESEND: {}", perception.presence_text));
    }
    if !perception.impulse_text.is_empty() {
        lines.push(format!("IMPULS: {}", perception.impulse_text));
    }

    lines.push("[/SYSTEM_INJECTION]".to_string());
    lines.join("\n")
}

/// Generiert Koerperwahrnehmungstext aus Bio-Zustand
fn generate_body_text(bio: &BioState, personality: &Personality) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Hunger-Schwellenwerte
    if bio.hunger > 90.0 {
        parts.push("Dir ist schwindelig vor Hunger.".to_string());
    } else if bio.hunger > 80.0 {
        parts.push("Dein Magen krampft.".to_string());
    } else if bio.hunger > 70.0 {
        parts.push("Du koenntest etwas essen.".to_string());
    }

    // Blasendrang
    if bio.bladder > 90.0 {
        parts.push("Du musst JETZT zur Toilette.".to_string());
    } else if bio.bladder > 80.0 {
        parts.push("Dringend: Toilette.".to_string());
    } else if bio.bladder > 60.0 {
        parts.push("Du solltest bald eine Pause einlegen.".to_string());
    }

    // Energie
    if bio.energy < 20.0 {
        parts.push("Du kannst kaum die Augen offen halten.".to_string());
    } else if bio.energy < 40.0 {
        parts.push("Du bist muede.".to_string());
    }

    // Stress
    if bio.stress > 80.0 {
        parts.push("Dein Herz rast.".to_string());
    } else if bio.stress > 60.0 {
        parts.push("Du stehst unter Druck.".to_string());
    }

    // Koffein-Entzug: caffeine_mg < 20.0 UND caffeine_tolerance > 0.3
    if bio.caffeine_mg < 20.0 && personality.caffeine_tolerance > 0.3 {
        parts.push("Leichte Kopfschmerzen - du brauchst Kaffee.".to_string());
    }

    // Gesund (keine Schwellenwerte ueberschritten)
    if parts.is_empty() {
        parts.push("Du fuehlst dich gut.".to_string());
    }

    parts.join(" ")
}

/// Generiert Umgebungstext aus Temperatur, CO2 und Geruechen
fn generate_environment_text(temp_c: f32, co2_ppm: f32, smells: &[SmellEvent]) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Temperatur
    if temp_c > 24.0 {
        parts.push("Es ist warm.".to_string());
    } else if temp_c < 19.0 {
        parts.push("Es ist kuehl.".to_string());
    } else {
        parts.push("Die Temperatur ist angenehm.".to_string());
    }

    // CO2
    if co2_ppm > 1000.0 {
        parts.push("Stickig.".to_string());
    }

    // Gerueche (nur intensity > 0.3)
    for smell in smells {
        if smell.intensity > 0.3 {
            let smell_text = match smell.smell_type.as_str() {
                "coffee" => "Kaffeeduft.".to_string(),
                "food" => "Essensgeruch.".to_string(),
                "perfume" => "Parfuemduft.".to_string(),
                "printer_toner" => "Tonergeruch.".to_string(),
                other => format!("Ein Geruch von {}.", other),
            };
            parts.push(smell_text);
        }
    }

    parts.join(" ")
}

/// Generiert Akustiktext aus Laermpegel
fn generate_acoustic_text(noise_db: f32) -> String {
    match noise_db as u32 {
        0..=35 => "Stille.".to_string(),
        36..=50 => "Normales Buerogeraeusch.".to_string(),
        51..=65 => "Lebhafte Unterhaltungen.".to_string(),
        _ => "Es ist laut. Konzentration faellt schwer.".to_string(),
    }
}

/// Generiert Anwesenheitstext
fn generate_presence_text(present_agents: &[(String, String)]) -> String {
    if present_agents.is_empty() {
        "Du bist allein.".to_string()
    } else {
        present_agents
            .iter()
            .map(|(name, activity)| format!("{} ({})", name, activity))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Generiert Impulstext (Prioritaet: Toilette > Hunger > Muedigkeit > Sozial > Ruhe)
fn generate_impulse_text(bio: &BioState, personality: &Personality) -> String {
    if bio.bladder > 85.0 {
        "Dringendes Beduerfnis, zur Toilette zu gehen.".to_string()
    } else if bio.hunger > 85.0 {
        "Dringendes Beduerfnis, etwas zu essen.".to_string()
    } else if bio.energy < 25.0 {
        "Dringendes Beduerfnis, Pause zu machen.".to_string()
    } else if bio.social_need > 80.0 && personality.extraversion > 0.5 {
        "Du moechtest mit jemandem reden.".to_string()
    } else if bio.social_need < 20.0 && personality.extraversion < 0.5 {
        "Du geniesst die Ruhe.".to_string()
    } else {
        String::new()
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
        }
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
            result.environment_text.contains("Stickig"),
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
            environment_text: "Die Temperatur ist angenehm.".to_string(),
            acoustic_text: "Normales Buerogeraeusch.".to_string(),
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
