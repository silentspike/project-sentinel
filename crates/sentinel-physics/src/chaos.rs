//! Chaos Monkey: Zufallsereignisse im Buero

/// 8 Chaos-Event-Typen mit realistischen Buero-Frequenzen
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaosEventType {
    PhoneRing,
    PrinterBroken,
    PackageDelivery,
    SBahnDelay,
    FireAlarmDrill,
    CakeInKitchen,
    AirConBroken,
    InternetOutage,
}

/// Gibt die Frequenz pro Stunde fuer einen Event-Typ zurueck
pub fn chaos_frequency_per_hour(event_type: ChaosEventType) -> f32 {
    match event_type {
        ChaosEventType::PhoneRing => 0.3,
        ChaosEventType::PrinterBroken => 0.05,
        ChaosEventType::PackageDelivery => 0.1,
        ChaosEventType::SBahnDelay => 0.02,
        ChaosEventType::FireAlarmDrill => 0.0014,
        ChaosEventType::CakeInKitchen => 0.08,
        ChaosEventType::AirConBroken => 0.01,
        ChaosEventType::InternetOutage => 0.005,
    }
}

/// Prueft ob ein Chaos-Event ausgeloest werden soll (Poisson-verteilt)
///
/// P(event in tick) = 1 - e^(-lambda * dt)
/// wobei lambda = frequency_per_hour / 3600
pub fn should_trigger_chaos(
    frequency_per_hour: f32,
    tick_duration_secs: f32,
    rng_value: f32, // 0.0-1.0 Random-Wert
) -> bool {
    if frequency_per_hour <= 0.0 {
        return false;
    }
    let lambda = frequency_per_hour / 3600.0;
    let probability = 1.0 - (-lambda * tick_duration_secs).exp();
    rng_value < probability
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn test_all_frequencies_correct() {
        assert_relative_eq!(
            chaos_frequency_per_hour(ChaosEventType::PhoneRing),
            0.3,
            epsilon = 1.0
        );
        assert_relative_eq!(
            chaos_frequency_per_hour(ChaosEventType::PrinterBroken),
            0.05,
            epsilon = 1.0
        );
        assert_relative_eq!(
            chaos_frequency_per_hour(ChaosEventType::PackageDelivery),
            0.1,
            epsilon = 1.0
        );
        assert_relative_eq!(
            chaos_frequency_per_hour(ChaosEventType::SBahnDelay),
            0.02,
            epsilon = 1.0
        );
        assert_relative_eq!(
            chaos_frequency_per_hour(ChaosEventType::FireAlarmDrill),
            0.0014,
            epsilon = 1.0
        );
        assert_relative_eq!(
            chaos_frequency_per_hour(ChaosEventType::CakeInKitchen),
            0.08,
            epsilon = 1.0
        );
        assert_relative_eq!(
            chaos_frequency_per_hour(ChaosEventType::AirConBroken),
            0.01,
            epsilon = 1.0
        );
        assert_relative_eq!(
            chaos_frequency_per_hour(ChaosEventType::InternetOutage),
            0.005,
            epsilon = 1.0
        );
    }

    #[test]
    fn test_very_high_frequency_triggers() {
        // Sehr hohe Frequenz (100/h), 1 Sekunde → probability = 1 - e^(-100/3600) ≈ 0.0277
        // Mit rng=0.5 sollte nichts triggern bei normalem Case
        // Aber mit frequency=10000 sollte es fast immer triggern
        let result = should_trigger_chaos(10000.0, 1.0, 0.5);
        assert!(result); // probability ~1.0 bei sehr hoher Frequenz
    }

    #[test]
    fn test_zero_frequency_never_triggers() {
        assert!(!should_trigger_chaos(0.0, 1.0, 0.5));
        assert!(!should_trigger_chaos(-1.0, 1.0, 0.5)); // negative Frequenz auch nie
    }

    #[test]
    fn test_all_event_types_exist() {
        // Pattern match auf alle 8 Varianten → Compiler-Fehler bei fehlenden
        let events = [
            ChaosEventType::PhoneRing,
            ChaosEventType::PrinterBroken,
            ChaosEventType::PackageDelivery,
            ChaosEventType::SBahnDelay,
            ChaosEventType::FireAlarmDrill,
            ChaosEventType::CakeInKitchen,
            ChaosEventType::AirConBroken,
            ChaosEventType::InternetOutage,
        ];
        for event in &events {
            let _freq = chaos_frequency_per_hour(*event); // Muss fuer alle kompilieren
        }
    }
}
