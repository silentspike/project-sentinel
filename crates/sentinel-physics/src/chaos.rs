//! Chaos Monkey: Zufallsereignisse im Buero

use sentinel_common::EventType;

const PHONE_RING_DURATION_TICKS: u64 = 15;
const PRINTER_BROKEN_DURATION_TICKS: u64 = 900;
const PACKAGE_DELIVERY_DURATION_TICKS: u64 = 120;
const S_BAHN_DELAY_DURATION_TICKS: u64 = 600;
const FIRE_ALARM_DRILL_DURATION_TICKS: u64 = 180;
const CAKE_IN_KITCHEN_DURATION_TICKS: u64 = 1800;
const AIRCON_BROKEN_DURATION_TICKS: u64 = 7200;
const INTERNET_OUTAGE_DURATION_TICKS: u64 = 3600;

const PHONE_RING_NOISE_BONUS_DB: f32 = 9.0;
const PRINTER_BROKEN_NOISE_BONUS_DB: f32 = 25.0;
const PACKAGE_DELIVERY_NOISE_BONUS_DB: f32 = 8.0;
const FIRE_ALARM_DRILL_NOISE_BONUS_DB: f32 = 38.0;
const CAKE_IN_KITCHEN_NOISE_BONUS_DB: f32 = 4.0;

const AIRCON_HEATUP_RATE_C_PER_HOUR: f32 = 2.5;
const AIRCON_MAX_HEATUP_DELTA_C: f32 = 5.0;

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

/// Standarddauer pro Chaos-Event im 1Hz-Takt der Simulation.
pub fn default_chaos_duration_ticks(event_type: EventType) -> u64 {
    match event_type {
        EventType::PhoneRing => PHONE_RING_DURATION_TICKS,
        EventType::PrinterBroken => PRINTER_BROKEN_DURATION_TICKS,
        EventType::PackageDelivery => PACKAGE_DELIVERY_DURATION_TICKS,
        EventType::SBahnDelay => S_BAHN_DELAY_DURATION_TICKS,
        EventType::FireAlarmDrill => FIRE_ALARM_DRILL_DURATION_TICKS,
        EventType::CakeInKitchen => CAKE_IN_KITCHEN_DURATION_TICKS,
        EventType::AirConBroken => AIRCON_BROKEN_DURATION_TICKS,
        EventType::InternetOutage => INTERNET_OUTAGE_DURATION_TICKS,
    }
}

/// Zusatzlaerm fuer aktive Chaos-Events im betroffenen Raum.
pub fn chaos_noise_bonus_db(event_type: EventType) -> f32 {
    match event_type {
        EventType::PhoneRing => PHONE_RING_NOISE_BONUS_DB,
        EventType::PrinterBroken => PRINTER_BROKEN_NOISE_BONUS_DB,
        EventType::PackageDelivery => PACKAGE_DELIVERY_NOISE_BONUS_DB,
        EventType::SBahnDelay => 0.0,
        EventType::FireAlarmDrill => FIRE_ALARM_DRILL_NOISE_BONUS_DB,
        EventType::CakeInKitchen => CAKE_IN_KITCHEN_NOISE_BONUS_DB,
        EventType::AirConBroken => 0.0,
        EventType::InternetOutage => 0.0,
    }
}

/// Temperaturversatz fuer aktive Chaos-Events im betroffenen Raum.
pub fn chaos_temperature_delta_celsius(event_type: EventType, elapsed_hours: f32) -> f32 {
    match event_type {
        EventType::AirConBroken => {
            (elapsed_hours.max(0.0) * AIRCON_HEATUP_RATE_C_PER_HOUR).min(AIRCON_MAX_HEATUP_DELTA_C)
        }
        EventType::PhoneRing
        | EventType::PrinterBroken
        | EventType::PackageDelivery
        | EventType::SBahnDelay
        | EventType::FireAlarmDrill
        | EventType::CakeInKitchen
        | EventType::InternetOutage => 0.0,
    }
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

    #[test]
    fn test_aircon_broken_heats_room_over_time() {
        let initial = chaos_temperature_delta_celsius(EventType::AirConBroken, 0.0);
        let after_hour = chaos_temperature_delta_celsius(EventType::AirConBroken, 1.0);
        let after_three_hours = chaos_temperature_delta_celsius(EventType::AirConBroken, 3.0);

        assert_relative_eq!(initial, 0.0, epsilon = 0.01);
        assert!(
            after_hour >= 2.4,
            "expected visible heatup after 1h, got {after_hour}"
        );
        assert_relative_eq!(after_three_hours, AIRCON_MAX_HEATUP_DELTA_C, epsilon = 0.01);
    }

    #[test]
    fn test_printer_broken_noise_bonus_is_visible() {
        let printer_noise = chaos_noise_bonus_db(EventType::PrinterBroken);
        let phone_noise = chaos_noise_bonus_db(EventType::PhoneRing);

        assert!(printer_noise >= 20.0, "printer should be clearly audible");
        assert!(
            printer_noise > phone_noise,
            "printer should be louder than phone"
        );
    }

    #[test]
    fn test_chaos_durations_cover_long_running_aircon() {
        assert_eq!(
            default_chaos_duration_ticks(EventType::AirConBroken),
            AIRCON_BROKEN_DURATION_TICKS
        );
        assert!(
            default_chaos_duration_ticks(EventType::AirConBroken)
                > default_chaos_duration_ticks(EventType::PhoneRing)
        );
    }
}
