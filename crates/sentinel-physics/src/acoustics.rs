//! Akustik-Simulation: Laermpegel pro Raum

/// Basis-Laermpegel in dB fuer leeren Raum
const BASE_NOISE_DB: f32 = 30.0;

/// dB pro Agent im Raum
const DB_PER_AGENT: f32 = 5.0;

/// Zusaetzliche dB bei Meeting
const MEETING_BONUS_DB: f32 = 10.0;

/// Zusaetzliche dB bei Telefonat
const PHONE_CALL_BONUS_DB: f32 = 5.0;

/// Daempfung fuer Adjacent-Room-Laerm (Multiplikator)
const ADJACENT_DAMPING: f32 = 0.3;

/// Berechnet Laermpegel eines Raums in dB
pub fn calculate_noise_level(
    room_agents_count: usize,
    has_meeting: bool,
    has_phone_call: bool,
    adjacent_rooms_noise: &[f32],
) -> f32 {
    let mut noise = BASE_NOISE_DB + DB_PER_AGENT * room_agents_count as f32;
    if has_meeting {
        noise += MEETING_BONUS_DB;
    }
    if has_phone_call {
        noise += PHONE_CALL_BONUS_DB;
    }

    // Laerm aus Nachbarraeumen (gedaempft)
    let adjacent_noise: f32 = adjacent_rooms_noise.iter().sum::<f32>() * ADJACENT_DAMPING
        / adjacent_rooms_noise.len().max(1) as f32;
    noise += adjacent_noise;
    noise
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
        assert_relative_eq!(noise, 30.0, epsilon = 1.0);
    }

    #[test]
    fn test_agents_add_noise() {
        let noise = calculate_noise_level(5, false, false, &[]);
        assert_relative_eq!(noise, 30.0 + 25.0, epsilon = 1.0);
    }

    #[test]
    fn test_meeting_and_phone_bonuses() {
        let noise = calculate_noise_level(10, true, true, &[]);
        // 30 + 50 (10 agents) + 10 (meeting) + 5 (phone) = 95
        assert_relative_eq!(noise, 95.0, epsilon = 1.0);
    }

    #[test]
    fn test_adjacent_room_noise() {
        // Adjacent rooms: [60.0, 50.0] → avg = 55.0, damped = 55 * 0.3 = 16.5
        let noise = calculate_noise_level(0, false, false, &[60.0, 50.0]);
        assert_relative_eq!(noise, 30.0 + 16.5, epsilon = 1.0);
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
}
