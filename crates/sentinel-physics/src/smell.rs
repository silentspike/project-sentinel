//! Olfaktorische Simulation: Gerueche propagieren ueber Raeume

/// Ein Geruchsereignis mit Quelle, Typ und Propagation
#[derive(Debug, Clone)]
pub struct SmellEvent {
    pub source_room: String,
    pub smell_type: SmellType,
    pub intensity: f32,      // 0.0-1.0
    pub decay_per_room: f32, // Intensitaetsverlust pro Raum
    pub created_tick: u64,
    pub duration_ticks: u64,
}

/// Geruchstypen
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmellType {
    Coffee,
    Food,
    Perfume,
    Smoke,
    CleaningAgent,
}

/// Berechnet Geruchsintensitaet in Entfernung distance Raeume
pub fn smell_intensity_at_distance(event: &SmellEvent, distance_rooms: u32) -> f32 {
    let intensity = event.intensity - event.decay_per_room * distance_rooms as f32;
    intensity.max(0.0)
}

/// Prueft ob Geruchsereignis noch aktiv ist
pub fn is_smell_active(event: &SmellEvent, current_tick: u64) -> bool {
    current_tick < event.created_tick + event.duration_ticks
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn test_intensity_at_source() {
        let event = SmellEvent {
            source_room: "kueche".to_string(),
            smell_type: SmellType::Coffee,
            intensity: 0.8,
            decay_per_room: 0.25,
            created_tick: 10,
            duration_ticks: 100,
        };
        let intensity = smell_intensity_at_distance(&event, 0);
        assert_relative_eq!(intensity, 0.8, epsilon = 1.0);
    }

    #[test]
    fn test_propagation_decay() {
        let event = SmellEvent {
            source_room: "kueche".to_string(),
            smell_type: SmellType::Coffee,
            intensity: 0.8,
            decay_per_room: 0.25,
            created_tick: 10,
            duration_ticks: 100,
        };
        // distance 1: 0.8 - 0.25 = 0.55
        assert_relative_eq!(smell_intensity_at_distance(&event, 1), 0.55, epsilon = 1.0);
        // distance 2: 0.8 - 0.5 = 0.30
        assert_relative_eq!(smell_intensity_at_distance(&event, 2), 0.30, epsilon = 1.0);
        // distance 3: 0.8 - 0.75 = 0.05
        assert_relative_eq!(smell_intensity_at_distance(&event, 3), 0.05, epsilon = 1.0);
    }

    #[test]
    fn test_beyond_max_radius() {
        let event = SmellEvent {
            source_room: "kueche".to_string(),
            smell_type: SmellType::Coffee,
            intensity: 0.8,
            decay_per_room: 0.25,
            created_tick: 10,
            duration_ticks: 100,
        };
        // distance 4: 0.8 - 1.0 = -0.2 → clamped to 0.0
        assert_relative_eq!(smell_intensity_at_distance(&event, 4), 0.0, epsilon = 1.0);
        assert_relative_eq!(smell_intensity_at_distance(&event, 10), 0.0, epsilon = 1.0);
    }

    #[test]
    fn test_smell_active_and_expired() {
        let event = SmellEvent {
            source_room: "kueche".to_string(),
            smell_type: SmellType::Coffee,
            intensity: 0.8,
            decay_per_room: 0.25,
            created_tick: 10,
            duration_ticks: 100,
        };
        // active: tick 50 < 10 + 100 = 110
        assert!(is_smell_active(&event, 50));
        assert!(is_smell_active(&event, 109));
        // expired: tick 110 >= 110
        assert!(!is_smell_active(&event, 110));
        assert!(!is_smell_active(&event, 200));
    }
}
