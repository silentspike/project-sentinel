//! Schicht-Erkennung und Mapping.
//!
//! Shift-Sets:
//! - 0: Sonder (always-on, wird NIE konsolidiert)
//! - 1: Frueh (06:00-14:00)
//! - 2: Mittel (14:00-22:00)
//! - 3: Spaet (22:00-06:00)

/// Bestimmt die aktuelle Schicht anhand der Wall-Clock-Stunde.
pub fn current_shift_set() -> u8 {
    shift_set_for_hour(current_local_hour())
}

/// Mapping: Stunde → Schicht-Set.
pub fn shift_set_for_hour(hour: u8) -> u8 {
    match hour {
        6..=13 => 1,
        14..=21 => 2,
        _ => 3, // 22-05
    }
}

/// Bestimmt welche Schicht abgeht (= konsolidiert werden soll).
///
/// Bei Schichtwechsel zu `new_shift` geht die vorherige Schicht ab:
/// - Frueh (1) startet → Spaet (3) geht ab
/// - Mittel (2) startet → Frueh (1) geht ab
/// - Spaet (3) startet → Mittel (2) geht ab
pub fn outgoing_shift_set(new_shift: u8) -> u8 {
    match new_shift {
        1 => 3,
        2 => 1,
        3 => 2,
        _ => 0, // Sonder: nie konsolidiert
    }
}

/// Gibt die aktuelle lokale Stunde zurueck (0-23).
fn current_local_hour() -> u8 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // SAFETY: libc::localtime_r ist thread-safe
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time_t = secs as libc::time_t;
    unsafe { libc::localtime_r(&time_t, &mut tm) };
    tm.tm_hour as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_mapping_frueh() {
        for hour in 6..=13 {
            assert_eq!(shift_set_for_hour(hour), 1, "hour {hour} should be Frueh");
        }
    }

    #[test]
    fn shift_mapping_mittel() {
        for hour in 14..=21 {
            assert_eq!(shift_set_for_hour(hour), 2, "hour {hour} should be Mittel");
        }
    }

    #[test]
    fn shift_mapping_spaet() {
        for hour in [22, 23, 0, 1, 2, 3, 4, 5] {
            assert_eq!(shift_set_for_hour(hour), 3, "hour {hour} should be Spaet");
        }
    }

    #[test]
    fn outgoing_mapping() {
        assert_eq!(outgoing_shift_set(1), 3); // Frueh startet → Spaet geht
        assert_eq!(outgoing_shift_set(2), 1); // Mittel startet → Frueh geht
        assert_eq!(outgoing_shift_set(3), 2); // Spaet startet → Mittel geht
        assert_eq!(outgoing_shift_set(0), 0); // Sonder → nix
    }

    #[test]
    fn boundary_hours() {
        assert_eq!(shift_set_for_hour(5), 3); // 05:59 → Spaet
        assert_eq!(shift_set_for_hour(6), 1); // 06:00 → Frueh
        assert_eq!(shift_set_for_hour(13), 1); // 13:59 → Frueh
        assert_eq!(shift_set_for_hour(14), 2); // 14:00 → Mittel
        assert_eq!(shift_set_for_hour(21), 2); // 21:59 → Mittel
        assert_eq!(shift_set_for_hour(22), 3); // 22:00 → Spaet
    }

    #[test]
    fn current_shift_set_returns_valid() {
        let shift = current_shift_set();
        assert!((1..=3).contains(&shift));
    }
}
