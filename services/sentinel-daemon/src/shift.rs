//! Schicht-Erkennung basierend auf der aktuellen Uhrzeit.

use sentinel_common::agent_config::AgentConfig;

/// Erkennt das aktuelle Schicht-Set basierend auf der lokalen Uhrzeit.
///
/// - Set 1 (Frueh): 06:00-13:59
/// - Set 2 (Mittel): 14:00-21:59
/// - Set 3 (Spaet): 22:00-05:59
pub fn detect_current_shift() -> u8 {
    let hour = chrono::Local::now().hour();
    shift_for_hour(hour)
}

/// Shift-Erkennung basierend auf sim_hour (virtualisierte Simulationszeit).
/// Wird verwendet wenn `time_scale != 1.0` (beschleunigte/verlangsamte Simulation),
/// damit Schichtwechsel mit der simulierten Zeit synchron laufen.
pub fn detect_shift_from_sim_hour(sim_hour: f32) -> u8 {
    shift_for_hour(sim_hour.floor() as u32 % 24)
}

/// Bestimmt das Schicht-Set fuer eine gegebene Stunde (0-23).
/// Testbare Variante ohne Systemuhr-Abhaengigkeit.
pub fn shift_for_hour(hour: u32) -> u8 {
    match hour {
        6..=13 => 1,
        14..=21 => 2,
        _ => 3,
    }
}

/// Filtert Agents die zur aktuellen Schicht gehoeren.
/// Set 0 (Sonder) ist IMMER aktiv.
pub fn agents_for_shift(all: &[AgentConfig], shift: u8) -> Vec<&AgentConfig> {
    all.iter()
        .filter(|a| a.identity.shift_set == shift || a.identity.shift_set == 0)
        .collect()
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shift_mapping() {
        // Frueh
        for h in 6..=13 {
            assert_eq!(shift_for_hour(h), 1, "Stunde {h} sollte Fruehschicht sein");
        }
        // Mittel
        for h in 14..=21 {
            assert_eq!(shift_for_hour(h), 2, "Stunde {h} sollte Mittelschicht sein");
        }
        // Spaet
        for h in [22, 23, 0, 1, 2, 3, 4, 5] {
            assert_eq!(shift_for_hour(h), 3, "Stunde {h} sollte Spaetschicht sein");
        }
    }

    #[test]
    fn test_detect_returns_valid_set() {
        let set = detect_current_shift();
        assert!((1..=3).contains(&set), "Shift-Set muss 1-3 sein, war {set}");
    }

    #[test]
    fn test_detect_shift_from_sim_hour_boundaries() {
        // Fruehschicht 06:00-13:59
        assert_eq!(detect_shift_from_sim_hour(6.0), 1);
        assert_eq!(detect_shift_from_sim_hour(13.99), 1);
        // Mittelschicht 14:00-21:59
        assert_eq!(detect_shift_from_sim_hour(14.0), 2);
        assert_eq!(detect_shift_from_sim_hour(21.99), 2);
        // Spaetschicht 22:00-05:59
        assert_eq!(detect_shift_from_sim_hour(22.0), 3);
        assert_eq!(detect_shift_from_sim_hour(5.99), 3);
        assert_eq!(detect_shift_from_sim_hour(0.0), 3);
    }

    #[test]
    fn test_detect_shift_from_sim_hour_wraps() {
        // sim_hour kann ueber 24.0 gehen, wird via % 24 gewrappt
        assert_eq!(detect_shift_from_sim_hour(24.5), 3); // 0.5 → Stunde 0 → Set 3
        assert_eq!(detect_shift_from_sim_hour(25.0), 3); // 1.0 → Stunde 1 → Set 3
        assert_eq!(detect_shift_from_sim_hour(30.0), 1); // 6.0 → Stunde 6 → Set 1
        assert_eq!(detect_shift_from_sim_hour(38.0), 2); // 14.0 → Stunde 14 → Set 2
    }
}
