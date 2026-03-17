//! Akustik-Simulation: Laermpegel pro Raum (logarithmische dB-Addition)

/// Basis-Laermpegel in dB fuer leeren Raum (Klimaanlage, PC-Luefter)
const BASE_NOISE_DB: f32 = 30.0;

/// dB-Beitrag pro Agent (einzelne Schallquelle)
const DB_PER_AGENT: f32 = 5.0;

/// Zusaetzliche dB bei Meeting
const MEETING_BONUS_DB: f32 = 10.0;

/// Zusaetzliche dB bei Telefonat
const PHONE_CALL_BONUS_DB: f32 = 5.0;

/// Daempfung fuer Adjacent-Room-Laerm in dB
const ADJACENT_WALL_DAMPING_DB: f32 = 20.0;

/// Realistische Obergrenze fuer Buero-Laermpegel in dB.
/// 85 dB = Staubsauger, alles darueber ist unrealistisch fuer Bueros.
const MAX_OFFICE_NOISE_DB: f32 = 85.0;

/// Konvertiert dB in lineare Leistung: 10^(db/10)
fn db_to_power(db: f32) -> f32 {
    10.0_f32.powf(db / 10.0)
}

/// Konvertiert lineare Leistung in dB: 10 * log10(power)
fn power_to_db(power: f32) -> f32 {
    if power <= 0.0 {
        0.0
    } else {
        10.0 * power.log10()
    }
}

/// Berechnet Laermpegel eines Raums in dB (logarithmische Addition)
///
/// Physikalisch korrekte dB-Addition: Schallquellen werden als
/// Leistungen addiert, nicht als dB-Werte.
pub fn calculate_noise_level(
    room_agents_count: usize,
    has_meeting: bool,
    has_phone_call: bool,
    adjacent_rooms_noise: &[f32],
) -> f32 {
    // Basis-Leistung (leerer Raum)
    let mut total_power = db_to_power(BASE_NOISE_DB);

    // Jeder Agent als separate Schallquelle
    for _ in 0..room_agents_count {
        total_power += db_to_power(DB_PER_AGENT);
    }

    // Activity-Bonus als eigene Schallquelle
    if has_meeting {
        total_power += db_to_power(MEETING_BONUS_DB);
    }
    if has_phone_call {
        total_power += db_to_power(PHONE_CALL_BONUS_DB);
    }

    // Nachbarraum-Laerm (Wanddaempfung abziehen)
    for &adj_db in adjacent_rooms_noise {
        let damped = adj_db - ADJACENT_WALL_DAMPING_DB;
        if damped > 0.0 {
            total_power += db_to_power(damped);
        }
    }

    power_to_db(total_power).min(MAX_OFFICE_NOISE_DB)
}

/// Mappt dB auf Wahrnehmungstext
pub fn noise_to_text(noise_db: f32) -> &'static str {
    match noise_db {
        n if n < 35.0 => "Angenehme Stille",
        n if n < 50.0 => "Normales Buerogeraeusch",
        n if n < 65.0 => "Lebhafte Unterhaltungen",
        n if n < 80.0 => "Es ist laut",
        _ => "Unertraeglicher Laerm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn test_empty_room_base_noise() {
        let noise = calculate_noise_level(0, false, false, &[]);
        assert_relative_eq!(noise, 30.0, epsilon = 0.1);
    }

    #[test]
    fn test_agents_add_noise_logarithmic() {
        // 5 Agents: 10*log10(10^(30/10) + 5*10^(5/10)) = 10*log10(1000 + 5*3.16)
        // = 10*log10(1000 + 15.81) = 10*log10(1015.81) ≈ 30.07 dB
        let noise = calculate_noise_level(5, false, false, &[]);
        assert!(
            noise > 30.0,
            "noise should be > 30 with agents, got {noise}"
        );
        assert!(
            noise < 35.0,
            "noise should be < 35 with 5 agents (log), got {noise}"
        );
    }

    #[test]
    fn test_many_agents_still_reasonable() {
        // 10 Agents: should be around 31 dB (not 80 dB like linear)
        let noise = calculate_noise_level(10, false, false, &[]);
        assert!(noise > 30.0, "noise should be > 30, got {noise}");
        assert!(
            noise < 40.0,
            "noise should be < 40 with 10 agents (log), got {noise}"
        );
    }

    #[test]
    fn test_meeting_adds_noise() {
        let without = calculate_noise_level(5, false, false, &[]);
        let with_meeting = calculate_noise_level(5, true, false, &[]);
        assert!(
            with_meeting > without,
            "meeting should increase noise: {with_meeting} > {without}"
        );
    }

    #[test]
    fn test_adjacent_room_noise_damped() {
        // Nachbarraum mit 60dB: 60 - 20 (Wanddaempfung) = 40dB durchgelassen
        let noise = calculate_noise_level(0, false, false, &[60.0]);
        // 10*log10(10^(30/10) + 10^(40/10)) = 10*log10(1000 + 10000) = 10*log10(11000) ≈ 40.4
        assert!(
            noise > 40.0,
            "adjacent 60dB room should raise noise, got {noise}"
        );
        assert!(noise < 42.0, "should be around 40.4dB, got {noise}");
    }

    #[test]
    fn test_adjacent_room_quiet_no_effect() {
        // Nachbarraum mit 15dB: 15 - 20 = -5 (negativ, wird ignoriert)
        let noise = calculate_noise_level(0, false, false, &[15.0]);
        assert_relative_eq!(noise, 30.0, epsilon = 0.1);
    }

    #[test]
    fn test_noise_to_text_mapping() {
        assert_eq!(noise_to_text(30.0), "Angenehme Stille");
        assert_eq!(noise_to_text(34.9), "Angenehme Stille");
        assert_eq!(noise_to_text(35.0), "Normales Buerogeraeusch");
        assert_eq!(noise_to_text(49.9), "Normales Buerogeraeusch");
        assert_eq!(noise_to_text(50.0), "Lebhafte Unterhaltungen");
        assert_eq!(noise_to_text(64.9), "Lebhafte Unterhaltungen");
        assert_eq!(noise_to_text(65.0), "Es ist laut");
        assert_eq!(noise_to_text(79.9), "Es ist laut");
        assert_eq!(noise_to_text(80.0), "Unertraeglicher Laerm");
        assert_eq!(noise_to_text(100.0), "Unertraeglicher Laerm");
    }

    #[test]
    fn noise_never_exceeds_85db() {
        // Extremszenario: 50 Agents, Meeting, Telefonat, 3 laute Nachbarraeume
        let noise = calculate_noise_level(50, true, true, &[80.0, 80.0, 80.0]);
        assert!(noise <= 85.0, "noise must be capped at 85 dB, got {noise}");
    }
}
