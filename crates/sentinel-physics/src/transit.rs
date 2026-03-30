//! Transit-Simulation: Raumwechsel und Flurbegegnungen

/// Millisekunden pro Hop (Gehgeschwindigkeit ~1.2 m/s, ~20s pro Raum-Uebergang).
pub const TRANSIT_MS_PER_HOP: u32 = 20_000;

/// Minimale Transit-Dauer in ms (Nachbar-Raum, Tuer raus).
pub const TRANSIT_MIN_MS: u32 = 15_000;

/// Maximale Transit-Dauer in ms (EG-Ende bis OG-Ende, ~2 Min).
pub const TRANSIT_MAX_MS: u32 = 120_000;

/// Wahrscheinlichkeit einer Flurbegegnung wenn beide in Transit
const HALLWAY_ENCOUNTER_PROBABILITY: f32 = 0.3;

/// Berechnet realistische Transit-Dauer aus Hop-Count.
pub fn transit_duration_ms(hops: u32) -> u32 {
    (hops * TRANSIT_MS_PER_HOP).clamp(TRANSIT_MIN_MS, TRANSIT_MAX_MS)
}

/// Bestimmt den aktuellen Zwischen-Raum basierend auf elapsed_ratio.
///
/// Route enthaelt nur Zwischen-Raeume (ohne Start/Ziel). Bei leerer Route
/// (direkter Nachbar) gibt es keinen Zwischen-Raum.
pub fn current_transit_room(route: &[String], remaining_ms: u32, total_ms: u32) -> Option<&str> {
    if route.is_empty() || total_ms == 0 {
        return None;
    }
    let elapsed_ratio = 1.0 - (remaining_ms as f64 / total_ms as f64);
    let idx = (elapsed_ratio * route.len() as f64).floor() as usize;
    route
        .get(idx.min(route.len().saturating_sub(1)))
        .map(|s| s.as_str())
}

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
        let state = start_transit("buero-dev-1", "kueche", 40_000);
        assert_eq!(state.origin, "buero-dev-1");
        assert_eq!(state.target, "kueche");
        assert_eq!(state.remaining_ms, 40_000);
        assert!(state.in_transit);
    }

    #[test]
    fn test_start_transit_clamps_to_min() {
        let state = start_transit("buero-dev-1", "kueche", 5000);
        assert_eq!(state.remaining_ms, TRANSIT_MIN_MS); // 15_000
    }

    #[test]
    fn test_start_transit_clamps_to_max() {
        let state = start_transit("buero-dev-1", "kueche", 200_000);
        assert_eq!(state.remaining_ms, TRANSIT_MAX_MS); // 120_000
    }

    #[test]
    fn test_transit_duration_ms_realistic() {
        assert_eq!(transit_duration_ms(1), 20_000); // 1 Hop = 20s, clamped to 20s
        assert_eq!(transit_duration_ms(2), 40_000); // 2 Hops = 40s
        assert_eq!(transit_duration_ms(4), 80_000); // 4 Hops = 80s
        assert_eq!(transit_duration_ms(5), 100_000); // 5 Hops = 100s
        assert_eq!(transit_duration_ms(7), TRANSIT_MAX_MS); // 7 Hops clamped to 120s
    }

    #[test]
    fn test_tick_transit_not_finished() {
        let mut state = start_transit("buero-dev-1", "kueche", 40_000);
        let finished = tick_transit(&mut state, 1000);
        assert!(!finished);
        assert_eq!(state.remaining_ms, 39_000);
        assert!(state.in_transit);
    }

    #[test]
    fn test_tick_transit_finished() {
        let mut state = start_transit("buero-dev-1", "kueche", 20_000);
        let finished = tick_transit(&mut state, 20_000);
        assert!(finished);
        assert_eq!(state.remaining_ms, 0);
        assert!(!state.in_transit);
    }

    #[test]
    fn test_current_transit_room_empty_route() {
        let route: Vec<String> = vec![];
        assert_eq!(current_transit_room(&route, 10_000, 20_000), None);
    }

    #[test]
    fn test_current_transit_room_single_room() {
        let route = vec!["flur-eg".to_string()];
        // Egal wo im Transit — immer in flur-eg
        assert_eq!(
            current_transit_room(&route, 15_000, 20_000),
            Some("flur-eg")
        );
        assert_eq!(current_transit_room(&route, 5_000, 20_000), Some("flur-eg"));
    }

    #[test]
    fn test_current_transit_room_multi_room() {
        let route = vec![
            "flur-og".to_string(),
            "treppenhaus".to_string(),
            "flur-eg".to_string(),
        ];
        let total = 80_000; // 4 Hops
                            // 0% elapsed → flur-og (idx=0)
        assert_eq!(current_transit_room(&route, 80_000, total), Some("flur-og"));
        // 10% elapsed → flur-og (idx=floor(0.1*3)=0)
        assert_eq!(current_transit_room(&route, 72_000, total), Some("flur-og"));
        // 34% elapsed → treppenhaus (idx=floor(0.34*3)=1)
        assert_eq!(
            current_transit_room(&route, 52_800, total),
            Some("treppenhaus")
        );
        // 50% elapsed → treppenhaus (idx=floor(0.5*3)=1)
        assert_eq!(
            current_transit_room(&route, 40_000, total),
            Some("treppenhaus")
        );
        // 67% elapsed → flur-eg (idx=floor(0.67*3)=2)
        assert_eq!(current_transit_room(&route, 26_400, total), Some("flur-eg"));
        // 99% elapsed → flur-eg (idx=min(2,2)=2)
        assert_eq!(current_transit_room(&route, 800, total), Some("flur-eg"));
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
