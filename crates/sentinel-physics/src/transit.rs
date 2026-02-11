//! Transit-Simulation: Raumwechsel und Flurbegegnungen

/// Minimale Transit-Dauer in ms
const TRANSIT_MIN_MS: u32 = 2000;

/// Maximale Transit-Dauer in ms
const TRANSIT_MAX_MS: u32 = 5000;

/// Wahrscheinlichkeit einer Flurbegegnung wenn beide in Transit
const HALLWAY_ENCOUNTER_PROBABILITY: f32 = 0.3;

/// Startet einen Raumwechsel
pub fn start_transit(current_room: &str, target_room: &str, rng_duration_ms: u32) -> TransitState {
    TransitState {
        origin: current_room.to_string(),
        target: target_room.to_string(),
        remaining_ms: rng_duration_ms.clamp(TRANSIT_MIN_MS, TRANSIT_MAX_MS),
        in_transit: true,
    }
}

/// Transit-Zustand
#[derive(Debug, Clone)]
pub struct TransitState {
    pub origin: String,
    pub target: String,
    pub remaining_ms: u32,
    pub in_transit: bool,
}

/// Aktualisiert Transit-Timer, gibt true zurueck wenn Transit abgeschlossen
pub fn tick_transit(state: &mut TransitState, delta_ms: u32) -> bool {
    if !state.in_transit {
        return false;
    }
    state.remaining_ms = state.remaining_ms.saturating_sub(delta_ms);
    if state.remaining_ms == 0 {
        state.in_transit = false;
        true
    } else {
        false
    }
}

/// Prueft ob zwei Agenten sich im Flur begegnen (30% Wahrscheinlichkeit)
pub fn check_hallway_encounter(
    agent_a_in_transit: bool,
    agent_b_in_transit: bool,
    rng_value: f32, // 0.0-1.0 Random-Wert (von aussen injiziert)
) -> bool {
    if !agent_a_in_transit || !agent_b_in_transit {
        return false;
    }
    rng_value < HALLWAY_ENCOUNTER_PROBABILITY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_start_transit_creates_correct_state() {
        let state = start_transit("buero-dev-1", "kueche", 3500);
        assert_eq!(state.origin, "buero-dev-1");
        assert_eq!(state.target, "kueche");
        assert_eq!(state.remaining_ms, 3500);
        assert!(state.in_transit);
    }

    #[test]
    fn test_tick_transit_not_finished() {
        let mut state = start_transit("buero-dev-1", "kueche", 3000);
        let finished = tick_transit(&mut state, 1000);
        assert!(!finished);
        assert_eq!(state.remaining_ms, 2000);
        assert!(state.in_transit);
    }

    #[test]
    fn test_tick_transit_finished() {
        let mut state = start_transit("buero-dev-1", "kueche", 3000);
        let finished = tick_transit(&mut state, 3000);
        assert!(finished);
        assert_eq!(state.remaining_ms, 0);
        assert!(!state.in_transit);
    }

    #[test]
    fn test_tick_transit_not_in_transit() {
        let mut state = TransitState {
            origin: "buero-dev-1".to_string(),
            target: "kueche".to_string(),
            remaining_ms: 0,
            in_transit: false,
        };
        let finished = tick_transit(&mut state, 1000);
        assert!(!finished);
        assert_eq!(state.remaining_ms, 0);
    }

    #[test]
    fn test_hallway_encounter_both_in_transit_below_threshold() {
        // rng < 0.3 → encounter
        assert!(check_hallway_encounter(true, true, 0.29));
        assert!(check_hallway_encounter(true, true, 0.1));
        assert!(check_hallway_encounter(true, true, 0.0));
    }

    #[test]
    fn test_hallway_encounter_both_in_transit_above_threshold() {
        // rng >= 0.3 → no encounter
        assert!(!check_hallway_encounter(true, true, 0.3));
        assert!(!check_hallway_encounter(true, true, 0.5));
        assert!(!check_hallway_encounter(true, true, 1.0));
    }

    #[test]
    fn test_hallway_encounter_one_not_in_transit() {
        // one not in transit → never encounter
        assert!(!check_hallway_encounter(true, false, 0.1));
        assert!(!check_hallway_encounter(false, true, 0.1));
        assert!(!check_hallway_encounter(false, false, 0.1));
    }
}
