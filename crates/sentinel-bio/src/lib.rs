//! Bio-Engine: Differenzialgleichungen fuer physiologische Agent-Zustaende.
//!
//! 6 biologische Parameter mit Formeln + 3 Action-Funktionen.

use sentinel_common::components::{BioState, Personality, WorkContext};

// ── Konstanten ──

/// Hunger-Rate: 12.5% pro Stunde
const HUNGER_RATE_PER_HOUR: f32 = 12.5;

/// Koffein-Halbwertszeit in Sekunden (5.7 Stunden = 20520 Sekunden)
const CAFFEINE_HALF_LIFE_SECS: f32 = 20520.0;

/// Blasendrang-Basisrate: 12% pro Stunde
const BLADDER_RATE_PER_HOUR: f32 = 12.0;

/// Koffein-Schwellenwert fuer erhoehten Blasendrang (mg)
const CAFFEINE_BLADDER_THRESHOLD: f32 = 50.0;

/// Koffein-Multiplikator fuer Blasendrang bei hohem Koffein
const CAFFEINE_BLADDER_MULTIPLIER: f32 = 1.5;

/// Standard-Koffein-Dosis pro Tasse Kaffee (mg)
const COFFEE_CAFFEINE_MG: f32 = 95.0;

/// Soziale Beduerfnis-Rate fuer Extrovertierte: +10/h
const SOCIAL_EXTROVERT_RATE: f32 = 10.0;

/// Soziale Beduerfnis-Rate fuer Introvertierte: -5/h
const SOCIAL_INTROVERT_RATE: f32 = -5.0;

/// Extroversion-Schwellenwert (>=0.5 = extrovertiert)
const EXTROVERSION_THRESHOLD: f32 = 0.5;

/// Arbeitsdrain pro Stunde seit Schichtbeginn (Energy-Penalty waehrend Arbeitszeit)
const WORK_DRAIN_PER_HOUR: f32 = 3.0;

/// Aktualisiert alle Bio-Zustaende fuer einen Tick
///
/// # Arguments
/// - `bio`: Aktueller biologischer Zustand (mutable)
/// - `personality`: Persoenlichkeit des Agenten
/// - `work`: Arbeitskontext
/// - `dt_seconds`: Zeitdifferenz in Sekunden
/// - `sim_hour`: Simulierte Tageszeit (0.0-24.0)
pub fn update_bio_state(
    bio: &mut BioState,
    personality: &Personality,
    work: &WorkContext,
    dt_seconds: f32,
    sim_hour: f32,
) {
    let dt_hours = dt_seconds / 3600.0;

    update_hunger(bio, dt_hours);
    update_caffeine(bio, dt_seconds);
    update_bladder(bio, dt_hours);
    update_stress(bio, personality, work);
    update_social_need(bio, personality, dt_hours);
    update_energy(bio, personality, sim_hour);
}

/// Aktualisiert Hunger (linear)
fn update_hunger(bio: &mut BioState, dt_hours: f32) {
    bio.hunger = (bio.hunger + HUNGER_RATE_PER_HOUR * dt_hours).clamp(0.0, 100.0);
}

/// Aktualisiert Koffein-Level (exponentieller Decay)
fn update_caffeine(bio: &mut BioState, dt_seconds: f32) {
    let decay_factor = (-f32::ln(2.0) / CAFFEINE_HALF_LIFE_SECS * dt_seconds).exp();
    bio.caffeine_mg *= decay_factor;

    // Numerische Stabilitaet: Bei <0.1 auf 0 setzen
    if bio.caffeine_mg < 0.1 {
        bio.caffeine_mg = 0.0;
    }
}

/// Aktualisiert Blasendrang (linear + Koffein-Multiplikator)
fn update_bladder(bio: &mut BioState, dt_hours: f32) {
    let multiplier = if bio.caffeine_mg > CAFFEINE_BLADDER_THRESHOLD {
        CAFFEINE_BLADDER_MULTIPLIER
    } else {
        1.0
    };

    bio.bladder = (bio.bladder + BLADDER_RATE_PER_HOUR * dt_hours * multiplier).clamp(0.0, 100.0);
}

/// Aktualisiert Stress (gewichteter Multi-Faktor)
fn update_stress(bio: &mut BioState, personality: &Personality, work: &WorkContext) {
    // Meeting-Stress
    let meeting_stress = if work.in_meeting { 60.0 } else { 0.0 };

    // Deadline-Stress
    let deadline_stress = if work.has_deadline { 70.0 } else { 0.0 };

    // Conflict-Stress
    let conflict_stress = if work.has_conflict { 80.0 } else { 0.0 };

    // Bio-Stress (Hunger >50 erzeugt Stress)
    let bio_stress = (bio.hunger - 50.0).max(0.0) / 50.0 * 100.0;

    // Gewichtete Summe
    let raw =
        0.3 * meeting_stress + 0.3 * deadline_stress + 0.2 * conflict_stress + 0.2 * bio_stress;

    // Neurotizismus skaliert Stress-Sensitivitaet
    let neuroticism_scale = 0.5 + personality.neuroticism * 0.5;

    bio.stress = (raw * neuroticism_scale).clamp(0.0, 100.0);
}

/// Aktualisiert soziale Beduerfnisse (persoenlichkeitsabhaengig)
fn update_social_need(bio: &mut BioState, personality: &Personality, dt_hours: f32) {
    let rate = if personality.extraversion >= EXTROVERSION_THRESHOLD {
        SOCIAL_EXTROVERT_RATE
    } else {
        SOCIAL_INTROVERT_RATE
    };

    bio.social_need = (bio.social_need + rate * dt_hours).clamp(0.0, 100.0);
}

/// Aktualisiert Energie (circadian + Work-Drain + Penalties + Koffein-Boost)
///
/// Work-Drain: Arbeitsmuedigkeit steigt linear waehrend Arbeitszeit (06-22 Uhr).
/// Conscientiousness reduziert den Drain (gewissenhafte Agents ermuerden langsamer).
/// Caffeine-Tolerance moduliert den Koffein-Boost (hohe Toleranz = weniger Wirkung).
fn update_energy(bio: &mut BioState, personality: &Personality, sim_hour: f32) {
    use std::f32::consts::PI;

    // Basis-Energie aus circadian Rhythmus
    let base = if personality.is_morning_person {
        // Peak 8-12 Uhr
        80.0 + 15.0 * ((sim_hour - 2.0) * PI / 12.0).sin()
    } else {
        // Peak 10-14 Uhr
        75.0 + 15.0 * ((sim_hour - 4.0) * PI / 12.0).sin()
    };

    // Penalties
    let hunger_penalty = (bio.hunger - 70.0).max(0.0) / 30.0 * 20.0; // >70 Hunger -> max -20
    let stress_penalty = bio.stress / 100.0 * 15.0; // Stress -> max -15

    // Koffein-Boost mit Toleranz-Modifier
    // Hohe Toleranz (1.0) → weniger Wirkung (0.5x), niedrige (0.0) → volle Wirkung (1.0x)
    let tolerance_modifier = 1.0 - personality.caffeine_tolerance * 0.5;
    let caffeine_boost = (bio.caffeine_mg / 95.0).min(1.0) * 10.0 * tolerance_modifier;

    // Work-Drain: Muedigkeit akkumuliert waehrend Arbeitszeit (06-22 Uhr)
    let work_drain = if (6.0_f32..22.0).contains(&sim_hour) {
        let hours_worked = sim_hour - 6.0;
        // Conscientiousness reduziert Drain: 0.5x (sehr gewissenhaft) bis 1.5x (nachlässig)
        let drain_modifier = 1.5 - personality.conscientiousness;
        hours_worked * WORK_DRAIN_PER_HOUR * drain_modifier
    } else {
        0.0
    };

    bio.energy =
        (base - hunger_penalty - stress_penalty + caffeine_boost - work_drain).clamp(0.0, 100.0);
}

// ── PSI-Stress Konstanten ──

/// CPU PSI avg10 Schwellenwert fuer Stress-Erhoehung
const PSI_CPU_STRESS_THRESHOLD: f64 = 50.0;

/// Stress-Erhoehung bei CPU-Pressure ueber Schwelle
const PSI_CPU_STRESS_ADDITION: f32 = 10.0;

/// Memory PSI avg10 Schwellenwert fuer Stress + Comfort-Reduktion
const PSI_MEM_STRESS_THRESHOLD: f64 = 70.0;

/// Stress-Erhoehung bei Memory-Pressure ueber Schwelle
const PSI_MEM_STRESS_ADDITION: f32 = 20.0;

/// Comfort-Reduktion bei Memory-Pressure ueber Schwelle
const PSI_MEM_COMFORT_REDUCTION: f32 = 15.0;

/// Wendet PSI-basierte Stress/Comfort-Aenderungen auf BioState an.
///
/// Mappt cgroup PSI-Metriken auf biologische Reaktionen:
/// - CPU avg10 > 50 → stress += 10 (Kopfschmerzen, Konzentrationsprobleme)
/// - Memory avg10 > 70 → stress += 20, comfort -= 15 (Systemueberlastung)
pub fn apply_psi_stress(bio: &mut BioState, cpu_avg10: f64, mem_avg10: f64) {
    if cpu_avg10 > PSI_CPU_STRESS_THRESHOLD {
        bio.stress = (bio.stress + PSI_CPU_STRESS_ADDITION).clamp(0.0, 100.0);
    }
    if mem_avg10 > PSI_MEM_STRESS_THRESHOLD {
        bio.stress = (bio.stress + PSI_MEM_STRESS_ADDITION).clamp(0.0, 100.0);
        bio.comfort = (bio.comfort - PSI_MEM_COMFORT_REDUCTION).clamp(0.0, 100.0);
    }
}

/// Agent trinkt Kaffee: +95mg Koffein
pub fn drink_coffee(bio: &mut BioState) {
    bio.caffeine_mg += COFFEE_CAFFEINE_MG;
}

/// Agent isst: Hunger auf 0, +15 Energie
pub fn eat_meal(bio: &mut BioState) {
    bio.hunger = 0.0;
    bio.energy = (bio.energy + 15.0).min(100.0);
}

/// Agent geht auf Toilette: Blasendrang auf 0
pub fn use_bathroom(bio: &mut BioState) {
    bio.bladder = 0.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    // Helper: Erstellt Standard-BioState fuer Tests
    fn default_bio() -> BioState {
        BioState {
            hunger: 0.0,
            energy: 80.0,
            caffeine_mg: 0.0,
            bladder: 0.0,
            stress: 0.0,
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

    fn default_work() -> WorkContext {
        WorkContext {
            current_task: None,
            in_meeting: false,
            has_deadline: false,
            has_conflict: false,
            conflict_cooldown: 0,
        }
    }

    #[test]
    fn test_hunger_0_to_100_after_8h() {
        let mut bio = default_bio();
        let personality = default_personality();
        let work = default_work();

        // 8 Stunden in 1-Minuten-Schritten simulieren
        for _ in 0..(8 * 60) {
            update_bio_state(&mut bio, &personality, &work, 60.0, 10.0);
        }

        // 12.5/h * 8h = 100 → clamped auf 100
        assert_relative_eq!(bio.hunger, 100.0, epsilon = 1.0);
    }

    #[test]
    fn test_caffeine_half_life() {
        let mut bio = default_bio();
        bio.caffeine_mg = 95.0;
        let personality = default_personality();
        let work = default_work();

        // 5.7 Stunden (342 Minuten) in 1-Minuten-Schritten
        for _ in 0..342 {
            update_bio_state(&mut bio, &personality, &work, 60.0, 10.0);
        }

        // Nach einer Halbwertszeit: ~47.5mg
        assert_relative_eq!(bio.caffeine_mg, 47.5, epsilon = 1.0);
    }

    #[test]
    fn test_introvert_social_need_decreases() {
        let mut bio = default_bio();
        bio.social_need = 50.0;
        let mut personality = default_personality();
        personality.extraversion = 0.2; // Introvertiert
        let work = default_work();

        // 1 Stunde
        for _ in 0..60 {
            update_bio_state(&mut bio, &personality, &work, 60.0, 10.0);
        }

        // -5/h → 50 - 5 = 45
        assert_relative_eq!(bio.social_need, 45.0, epsilon = 1.0);
    }

    #[test]
    fn test_extrovert_social_need_increases() {
        let mut bio = default_bio();
        bio.social_need = 50.0;
        let mut personality = default_personality();
        personality.extraversion = 0.8; // Extrovertiert
        let work = default_work();

        // 1 Stunde
        for _ in 0..60 {
            update_bio_state(&mut bio, &personality, &work, 60.0, 10.0);
        }

        // +10/h → 50 + 10 = 60
        assert_relative_eq!(bio.social_need, 60.0, epsilon = 1.0);
    }

    #[test]
    fn test_morning_person_energy_peak() {
        let personality = Personality {
            openness: 0.5,
            conscientiousness: 0.5,
            extraversion: 0.5,
            agreeableness: 0.5,
            neuroticism: 0.0,
            caffeine_tolerance: 0.5,
            is_morning_person: true,
        };

        // Energie bei 10:00 Uhr (Peak fuer Morning Person) vs 22:00 (Tief)
        let mut bio_peak = default_bio();
        let mut bio_low = default_bio();
        let work = default_work();

        update_bio_state(&mut bio_peak, &personality, &work, 60.0, 10.0);
        update_bio_state(&mut bio_low, &personality, &work, 60.0, 22.0);

        // Morning Person hat bei 10:00 mehr Energie als bei 22:00
        assert!(
            bio_peak.energy > bio_low.energy,
            "Morning person energy at 10:00 ({}) should be > energy at 22:00 ({})",
            bio_peak.energy,
            bio_low.energy
        );
    }

    #[test]
    fn test_high_hunger_reduces_energy() {
        let personality = Personality {
            openness: 0.5,
            conscientiousness: 0.5,
            extraversion: 0.5,
            agreeableness: 0.5,
            neuroticism: 0.0,
            caffeine_tolerance: 0.5,
            is_morning_person: true,
        };
        let work = default_work();

        let mut bio_hungry = default_bio();
        bio_hungry.hunger = 90.0; // Sehr hungrig
        let mut bio_fed = default_bio();
        bio_fed.hunger = 10.0; // Satt

        update_bio_state(&mut bio_hungry, &personality, &work, 60.0, 10.0);
        update_bio_state(&mut bio_fed, &personality, &work, 60.0, 10.0);

        // Hunger >70 reduziert Energie
        assert!(
            bio_fed.energy > bio_hungry.energy,
            "Fed agent energy ({}) should be > hungry agent energy ({})",
            bio_fed.energy,
            bio_hungry.energy
        );
    }

    #[test]
    fn test_psi_cpu_stress_above_threshold() {
        let mut bio = default_bio();
        bio.stress = 30.0;
        apply_psi_stress(&mut bio, 60.0, 0.0);
        assert_relative_eq!(bio.stress, 40.0, epsilon = 0.01);
        assert_relative_eq!(bio.comfort, 70.0, epsilon = 0.01); // unchanged
    }

    #[test]
    fn test_psi_mem_stress_above_threshold() {
        let mut bio = default_bio();
        bio.stress = 20.0;
        bio.comfort = 80.0;
        apply_psi_stress(&mut bio, 0.0, 80.0);
        assert_relative_eq!(bio.stress, 40.0, epsilon = 0.01); // +20
        assert_relative_eq!(bio.comfort, 65.0, epsilon = 0.01); // -15
    }

    #[test]
    fn test_psi_below_threshold_no_change() {
        let mut bio = default_bio();
        bio.stress = 30.0;
        bio.comfort = 70.0;
        apply_psi_stress(&mut bio, 30.0, 40.0);
        assert_relative_eq!(bio.stress, 30.0, epsilon = 0.01);
        assert_relative_eq!(bio.comfort, 70.0, epsilon = 0.01);
    }

    #[test]
    fn test_psi_both_thresholds() {
        let mut bio = default_bio();
        bio.stress = 10.0;
        bio.comfort = 90.0;
        apply_psi_stress(&mut bio, 60.0, 80.0);
        assert_relative_eq!(bio.stress, 40.0, epsilon = 0.01); // +10 +20
        assert_relative_eq!(bio.comfort, 75.0, epsilon = 0.01); // -15
    }

    #[test]
    fn test_psi_stress_clamps_at_100() {
        let mut bio = default_bio();
        bio.stress = 95.0;
        apply_psi_stress(&mut bio, 60.0, 80.0);
        assert_relative_eq!(bio.stress, 100.0, epsilon = 0.01);
    }

    #[test]
    fn test_psi_comfort_clamps_at_0() {
        let mut bio = default_bio();
        bio.comfort = 5.0;
        apply_psi_stress(&mut bio, 0.0, 80.0);
        assert_relative_eq!(bio.comfort, 0.0, epsilon = 0.01);
    }

    #[test]
    fn test_work_drain_reduces_energy_during_work_hours() {
        let personality = default_personality();
        let work = default_work();

        // Energie um 6:00 (Arbeitsbeginn, kein Drain) vs 14:00 (8h Drain)
        let mut bio_early = default_bio();
        let mut bio_afternoon = default_bio();
        update_bio_state(&mut bio_early, &personality, &work, 60.0, 6.0);
        update_bio_state(&mut bio_afternoon, &personality, &work, 60.0, 14.0);

        // 14:00 hat 8h Work-Drain (8*3*1.0=24), 6:00 hat 0 Drain
        assert!(
            bio_early.energy > bio_afternoon.energy,
            "Energy at 6:00 ({}) should be > energy at 14:00 ({})",
            bio_early.energy,
            bio_afternoon.energy
        );
    }

    #[test]
    fn test_conscientiousness_reduces_work_drain() {
        let mut high_c = default_personality();
        high_c.conscientiousness = 1.0; // Sehr gewissenhaft → weniger Drain
        let mut low_c = default_personality();
        low_c.conscientiousness = 0.0; // Nachlässig → mehr Drain

        let work = default_work();
        let mut bio_high = default_bio();
        let mut bio_low = default_bio();

        // 14:00 = 8h in die Schicht
        update_bio_state(&mut bio_high, &high_c, &work, 60.0, 14.0);
        update_bio_state(&mut bio_low, &low_c, &work, 60.0, 14.0);

        // high_c Drain: 8*3*0.5=12, low_c Drain: 8*3*1.5=36
        assert!(
            bio_high.energy > bio_low.energy,
            "Conscientious agent energy ({}) should be > careless agent energy ({})",
            bio_high.energy,
            bio_low.energy
        );
    }

    #[test]
    fn test_no_work_drain_outside_work_hours() {
        let personality = default_personality();
        let work = default_work();
        let mut bio = default_bio();

        // 23:00 = ausserhalb Arbeitszeit → kein Work-Drain
        update_bio_state(&mut bio, &personality, &work, 60.0, 23.0);

        // Circadian base at 23:00 fuer Morning Person: 80+15*sin((23-2)*PI/12)
        // = 80+15*sin(21*PI/12) = 80+15*sin(7*PI/4) ≈ 80-10.6 ≈ 69.4
        // Kein Drain, kein Hunger/Stress-Penalty → Energie = reine circadian Basis
        assert!(
            bio.energy > 60.0,
            "Energy at 23:00 with no drain should be > 60 (got {})",
            bio.energy
        );
    }

    #[test]
    fn test_caffeine_tolerance_modulates_boost() {
        let mut low_tol = default_personality();
        low_tol.caffeine_tolerance = 0.0; // Koffein wirkt voll (modifier=1.0)
        let mut high_tol = default_personality();
        high_tol.caffeine_tolerance = 1.0; // Koffein wirkt halb (modifier=0.5)

        let work = default_work();
        let mut bio_low = default_bio();
        bio_low.caffeine_mg = 95.0;
        let mut bio_high = default_bio();
        bio_high.caffeine_mg = 95.0;

        // Gleicher Zeitpunkt ausserhalb Arbeitszeit um Work-Drain zu eliminieren
        update_bio_state(&mut bio_low, &low_tol, &work, 60.0, 4.0);
        update_bio_state(&mut bio_high, &high_tol, &work, 60.0, 4.0);

        // Niedrige Toleranz → voller Boost → mehr Energie
        assert!(
            bio_low.energy > bio_high.energy,
            "Low tolerance energy ({}) should be > high tolerance energy ({})",
            bio_low.energy,
            bio_high.energy
        );
    }
}
